//! Self-contained, pure-Rust, read-only SQLite reader shared by Cursor and Codex.
//!
//! Cursor persists chat data in a SQLite KV store. Rather than depend on a
//! bundled C SQLite (which would break `windows_amd64_mingw` and add ~1 MB to
//! every build), we read the handful of `(key, value)` rows we need directly off
//! the SQLite file format. This module has **zero** new dependencies, compiles
//! cleanly on every target arch (it is plain Rust), and adds negligible binary
//! size.
//!
//! It resolves a named table's root page via `sqlite_master`, walks table b-trees,
//! and decodes every cell in each leaf record. Cursor consumes the first two
//! `cursorDiskKV` columns as raw bytes; Codex consumes typed columns from its
//! local thread-state and thread-history databases.
//!
//! Large payloads (Cursor JSON blobs routinely spill) are reassembled across
//! overflow-page chains per the SQLite file format spec.
//!
//! SQLite databases can retain committed pages in a `-wal` sidecar. `open()`
//! overlays frames through the last complete commit before scanning, so recent
//! Codex work is visible without linking a platform-specific SQLite library.
//! The reader does not acquire SQLite's WAL read-lock; it intentionally takes a
//! best-effort, read-only snapshot and ignores an incomplete trailing frame.
//!
//! Reference: <https://www.sqlite.org/fileformat2.html>

use std::fs;
use std::path::Path;

const HEADER_SIZE: usize = 100;

/// A decoded `(key, value)` row from a `(key TEXT, value BLOB/TEXT)` KV table.
pub struct KvRow {
    pub key: Vec<u8>,
    pub value: Vec<u8>,
}

/// A decoded SQLite table record with positional, typed values.
pub struct SqliteRow {
    values: Vec<Value>,
}

impl SqliteRow {
    pub fn text(&self, index: usize) -> Option<String> {
        self.values.get(index).and_then(Value::as_text)
    }

    pub fn int(&self, index: usize) -> Option<i64> {
        self.values.get(index).and_then(Value::as_int)
    }

    pub fn bytes(&self, index: usize) -> Vec<u8> {
        self.values
            .get(index)
            .map(Value::to_bytes)
            .unwrap_or_default()
    }
}

/// An in-memory SQLite database file opened read-only.
pub struct VscDb {
    data: Vec<u8>,
    page_size: usize,
    /// Usable bytes per page = page_size - reserved_bytes_per_page.
    usable: usize,
}

impl VscDb {
    /// Open and slurp a SQLite file. Returns `None` if the file is missing or
    /// not a recognisable SQLite database (caller falls back to "no rows").
    pub fn open(path: &Path) -> Option<Self> {
        let mut db = Self::from_bytes(fs::read(path).ok()?)?;
        let wal_path = path.with_file_name(format!(
            "{}-wal",
            path.file_name()?.to_string_lossy()
        ));
        if let Ok(wal) = fs::read(wal_path) {
            db.apply_wal(&wal);
        }
        Some(db)
    }

    /// Validate a SQLite header off an in-memory buffer and derive page geometry.
    fn from_bytes(data: Vec<u8>) -> Option<Self> {
        if data.len() < HEADER_SIZE || &data[..16] != b"SQLite format 3\0" {
            return None;
        }

        // Page size: big-endian u16 at offset 16; the literal value 1 means 65536.
        let raw_page_size = u16::from_be_bytes([data[16], data[17]]) as usize;
        let page_size = if raw_page_size == 1 { 65536 } else { raw_page_size };
        if page_size < 512 || !page_size.is_power_of_two() {
            return None;
        }

        // Reserved bytes per page (byte 20), usually 0.
        let reserved = data[20] as usize;
        if reserved >= page_size {
            return None;
        }
        let usable = page_size - reserved;

        Some(VscDb {
            data,
            page_size,
            usable,
        })
    }

    /// Byte offset where 1-indexed page `n` begins.
    fn page_offset(&self, n: u32) -> usize {
        (n as usize - 1) * self.page_size
    }

    /// Read every decoded record in a named table.
    pub fn read_rows(&self, table: &str) -> Vec<SqliteRow> {
        let root = match self.find_root_page(table) {
            Some(r) => r,
            None => return Vec::new(),
        };
        let mut records = Vec::new();
        let mut seen = std::collections::HashSet::new();
        self.scan_records(root, &mut records, &mut seen);
        records
            .into_iter()
            .map(|values| SqliteRow { values })
            .collect()
    }

    /// Read the first two columns of a named table as raw bytes.
    ///
    /// This preserves the original Cursor KV API while the generic reader above
    /// serves normal SQLite tables used by Codex.
    pub fn read_table(&self, table: &str) -> Vec<KvRow> {
        self.read_rows(table)
            .into_iter()
            .map(|row| KvRow {
                key: row.bytes(0),
                value: row.bytes(1),
            })
            .collect()
    }

    /// Overlay committed SQLite WAL frames onto the main database image.
    fn apply_wal(&mut self, wal: &[u8]) {
        const WAL_HEADER: usize = 32;
        const FRAME_HEADER: usize = 24;
        if wal.len() < WAL_HEADER {
            return;
        }

        let magic = u32::from_be_bytes([wal[0], wal[1], wal[2], wal[3]]);
        if !matches!(magic, 0x377f0682 | 0x377f0683) {
            return;
        }
        let wal_page_size = u32::from_be_bytes([wal[8], wal[9], wal[10], wal[11]]) as usize;
        let wal_page_size = if wal_page_size == 0 { 65536 } else { wal_page_size };
        if wal_page_size != self.page_size {
            return;
        }
        let salt = [wal[16], wal[17], wal[18], wal[19], wal[20], wal[21], wal[22], wal[23]];
        let frame_size = FRAME_HEADER + self.page_size;
        let mut frames: Vec<(usize, usize)> = Vec::new();
        let mut last_commit: Option<(usize, usize)> = None;
        let mut offset = WAL_HEADER;

        while offset + frame_size <= wal.len() {
            let page_no = u32::from_be_bytes([
                wal[offset],
                wal[offset + 1],
                wal[offset + 2],
                wal[offset + 3],
            ]) as usize;
            if page_no == 0 || wal[offset + 8..offset + 16] != salt {
                break;
            }
            frames.push((page_no, offset + FRAME_HEADER));
            let db_size = u32::from_be_bytes([
                wal[offset + 4],
                wal[offset + 5],
                wal[offset + 6],
                wal[offset + 7],
            ]) as usize;
            if db_size != 0 {
                last_commit = Some((frames.len(), db_size));
            }
            offset += frame_size;
        }

        let Some((committed_frames, committed_pages)) = last_commit else {
            return;
        };
        let required_len = committed_pages.saturating_mul(self.page_size);
        if required_len == 0 {
            return;
        }
        self.data.resize(required_len, 0);
        for (page_no, page_offset) in frames.into_iter().take(committed_frames) {
            let destination = match page_no.checked_sub(1).and_then(|n| n.checked_mul(self.page_size)) {
                Some(offset) if offset + self.page_size <= self.data.len() => offset,
                _ => continue,
            };
            self.data[destination..destination + self.page_size]
                .copy_from_slice(&wal[page_offset..page_offset + self.page_size]);
        }
    }

    /// Walk `sqlite_master` (root page 1) for `name == table`, returning rootpage.
    ///
    /// `sqlite_master` columns: (type TEXT, name TEXT, tbl_name TEXT,
    /// rootpage INTEGER, sql TEXT). We match on `name` (col 1) and read
    /// `rootpage` (col 3). Root pages are never hardcoded.
    fn find_root_page(&self, table: &str) -> Option<u32> {
        let mut rows = Vec::new();
        let mut seen = std::collections::HashSet::new();
        self.scan_records(1, &mut rows, &mut seen);
        for rec in &rows {
            // col 1 = name, col 3 = rootpage
            let name = rec.get(1).and_then(|c| c.as_text());
            if name.as_deref() == Some(table) {
                if let Some(rp) = rec.get(3).and_then(|c| c.as_int()) {
                    if rp > 0 {
                        return Some(rp as u32);
                    }
                }
            }
        }
        None
    }

    /// Recursively collect every leaf record (Vec<Value> per row) under `page`.
    fn scan_records(
        &self,
        page: u32,
        out: &mut Vec<Vec<Value>>,
        seen: &mut std::collections::HashSet<u32>,
    ) {
        // Cycle / corruption guard.
        if page == 0 || !seen.insert(page) {
            return;
        }
        let base = self.page_offset(page);
        if base + 8 > self.data.len() {
            return;
        }
        // Page 1 carries the 100-byte database header before its b-tree header.
        let hdr = if page == 1 { base + HEADER_SIZE } else { base };
        let page_type = self.data[hdr];
        let cell_count = u16::from_be_bytes([self.data[hdr + 3], self.data[hdr + 4]]) as usize;

        match page_type {
            0x0d => {
                // Leaf table page. Cell pointer array starts after the 8-byte header.
                let ptr_array = hdr + 8;
                for i in 0..cell_count {
                    let p = ptr_array + i * 2;
                    if p + 2 > self.data.len() {
                        break;
                    }
                    let cell_off =
                        base + u16::from_be_bytes([self.data[p], self.data[p + 1]]) as usize;
                    if let Some(rec) = self.decode_leaf_cell(cell_off) {
                        out.push(rec);
                    }
                }
            }
            0x05 => {
                // Interior table page: 12-byte header, right-most child at offset 8.
                let ptr_array = hdr + 12;
                for i in 0..cell_count {
                    let p = ptr_array + i * 2;
                    if p + 2 > self.data.len() {
                        break;
                    }
                    let cell_off =
                        base + u16::from_be_bytes([self.data[p], self.data[p + 1]]) as usize;
                    if cell_off + 4 <= self.data.len() {
                        let child = u32::from_be_bytes([
                            self.data[cell_off],
                            self.data[cell_off + 1],
                            self.data[cell_off + 2],
                            self.data[cell_off + 3],
                        ]);
                        self.scan_records(child, out, seen);
                    }
                }
                // Right-most child pointer.
                if hdr + 12 <= self.data.len() {
                    let right = u32::from_be_bytes([
                        self.data[hdr + 8],
                        self.data[hdr + 9],
                        self.data[hdr + 10],
                        self.data[hdr + 11],
                    ]);
                    self.scan_records(right, out, seen);
                }
            }
            // 0x0a (index leaf) / 0x02 (index interior) are ignored for table scans.
            _ => {}
        }
    }

    /// Decode a table-leaf cell at `cell_off` into a record (Vec<Value>),
    /// reassembling the payload across overflow pages when it spills.
    fn decode_leaf_cell(&self, cell_off: usize) -> Option<Vec<Value>> {
        let mut pos = cell_off;
        let (payload_len, n1) = read_varint(&self.data, pos)?;
        pos += n1;
        // rowid varint (unused — record carries its own columns)
        let (_rowid, n2) = read_varint(&self.data, pos)?;
        pos += n2;

        let payload_len = payload_len as usize;
        let payload = self.read_payload(pos, payload_len, /*table_leaf=*/ true)?;
        decode_record(&payload)
    }

    /// Read `payload_len` bytes of record payload starting at `start`, following
    /// the overflow-page chain if the payload does not fit on the page.
    ///
    /// Overflow threshold math (SQLite spec, table b-tree leaf):
    ///   X = usable - 35
    ///   if P <= X            => entire payload on page
    ///   else
    ///     M = ((usable - 12) * 32 / 255) - 23
    ///     K = M + ((P - M) % (usable - 4))
    ///     local = if K <= X { K } else { M }
    /// The remaining `P - local` bytes chain through overflow pages; each overflow
    /// page begins with a 4-byte BE next-page number (0 = last) then content.
    fn read_payload(&self, start: usize, p: usize, table_leaf: bool) -> Option<Vec<u8>> {
        // A payload can never legitimately exceed the file itself. Reject an
        // out-of-range length up front so a corrupt varint cannot drive a huge
        // `Vec::with_capacity(p)` allocation (which would abort the process).
        if p > self.data.len() {
            return None;
        }
        let usable = self.usable;
        let x = if table_leaf {
            usable - 35
        } else {
            ((usable - 12) * 64 / 255) - 23
        };

        if p <= x {
            // Fits entirely on the page.
            if start + p > self.data.len() {
                return None;
            }
            return Some(self.data[start..start + p].to_vec());
        }

        let m = ((usable - 12) * 32 / 255) - 23;
        let k = m + ((p - m) % (usable - 4));
        let local = if k <= x { k } else { m };

        if start + local + 4 > self.data.len() {
            return None;
        }
        let mut out = Vec::with_capacity(p);
        out.extend_from_slice(&self.data[start..start + local]);

        // 4-byte BE overflow page number immediately after the local payload.
        let mut next = u32::from_be_bytes([
            self.data[start + local],
            self.data[start + local + 1],
            self.data[start + local + 2],
            self.data[start + local + 3],
        ]);

        let mut remaining = p - local;
        let mut guard = 0usize;
        let max_pages = self.data.len() / self.page_size + 2;
        while next != 0 && remaining > 0 {
            guard += 1;
            if guard > max_pages {
                break; // corrupt / cyclic chain
            }
            let off = self.page_offset(next);
            if off + 4 > self.data.len() {
                break;
            }
            let nxt = u32::from_be_bytes([
                self.data[off],
                self.data[off + 1],
                self.data[off + 2],
                self.data[off + 3],
            ]);
            let avail = (self.usable - 4).min(remaining);
            let content_start = off + 4;
            if content_start + avail > self.data.len() {
                break;
            }
            out.extend_from_slice(&self.data[content_start..content_start + avail]);
            remaining -= avail;
            next = nxt;
        }
        Some(out)
    }
}

/// A single decoded SQLite record column value (only the variants we need).
enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(Vec<u8>),
    Blob(Vec<u8>),
}

impl Value {
    fn as_text(&self) -> Option<String> {
        match self {
            Value::Text(b) => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        }
    }

    fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Return TEXT/BLOB bodies as raw bytes; numeric/null collapse to empty.
    fn to_bytes(&self) -> Vec<u8> {
        match self {
            Value::Text(b) | Value::Blob(b) => b.clone(),
            Value::Int(i) => i.to_string().into_bytes(),
            Value::Real(r) => r.to_string().into_bytes(),
            Value::Null => Vec::new(),
        }
    }
}

/// Decode a SQLite record (header of serial types + column bodies) into values.
fn decode_record(payload: &[u8]) -> Option<Vec<Value>> {
    let (header_len, n) = read_varint(payload, 0)?;
    let header_len = header_len as usize;
    if header_len > payload.len() {
        return None;
    }

    // Read serial types from the header region.
    let mut serials = Vec::new();
    let mut hpos = n;
    while hpos < header_len {
        let (st, sn) = read_varint(payload, hpos)?;
        serials.push(st);
        hpos += sn;
    }

    // Column bodies start right after the header.
    let mut body = header_len;
    let mut values = Vec::with_capacity(serials.len());
    for st in serials {
        let (val, consumed) = decode_serial(st, payload, body)?;
        body += consumed;
        values.push(val);
    }
    Some(values)
}

/// Decode one column body given its serial type. Returns (value, bytes consumed).
fn decode_serial(st: u64, data: &[u8], pos: usize) -> Option<(Value, usize)> {
    let read = |len: usize| -> Option<&[u8]> {
        if pos + len <= data.len() {
            Some(&data[pos..pos + len])
        } else {
            None
        }
    };
    let val = match st {
        0 => (Value::Null, 0),
        1 => (Value::Int(read(1)?[0] as i8 as i64), 1),
        2 => {
            let b = read(2)?;
            (Value::Int(i16::from_be_bytes([b[0], b[1]]) as i64), 2)
        }
        3 => {
            let b = read(3)?;
            let mut v = ((b[0] as i64) << 16) | ((b[1] as i64) << 8) | (b[2] as i64);
            if v & 0x80_0000 != 0 {
                v -= 1 << 24; // sign-extend 24-bit
            }
            (Value::Int(v), 3)
        }
        4 => {
            let b = read(4)?;
            (
                Value::Int(i32::from_be_bytes([b[0], b[1], b[2], b[3]]) as i64),
                4,
            )
        }
        5 => {
            let b = read(6)?;
            let mut v = 0i64;
            for &byte in b {
                v = (v << 8) | byte as i64;
            }
            if v & 0x8000_0000_0000 != 0 {
                v -= 1 << 48; // sign-extend 48-bit
            }
            (Value::Int(v), 6)
        }
        6 => {
            let b = read(8)?;
            (
                Value::Int(i64::from_be_bytes([
                    b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
                ])),
                8,
            )
        }
        7 => {
            let b = read(8)?;
            (
                Value::Real(f64::from_be_bytes([
                    b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
                ])),
                8,
            )
        }
        8 => (Value::Int(0), 0),
        9 => (Value::Int(1), 0),
        n if n >= 12 && n % 2 == 0 => {
            let len = ((n - 12) / 2) as usize;
            (Value::Blob(read(len)?.to_vec()), len)
        }
        n if n >= 13 => {
            let len = ((n - 13) / 2) as usize;
            (Value::Text(read(len)?.to_vec()), len)
        }
        // 10 and 11 are reserved/internal serial types — treat as empty.
        _ => (Value::Null, 0),
    };
    Some(val)
}

/// Read a SQLite varint (big-endian, 1–9 bytes; high bit = continuation; the 9th
/// byte contributes all 8 bits). Returns (value, bytes consumed).
fn read_varint(data: &[u8], start: usize) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    let mut i = 0;
    while i < 9 {
        let byte = *data.get(start + i)?;
        if i == 8 {
            // 9th byte: use all 8 bits.
            result = (result << 8) | byte as u64;
            return Some((result, 9));
        }
        result = (result << 7) | (byte & 0x7f) as u64;
        if byte & 0x80 == 0 {
            return Some((result, i + 1));
        }
        i += 1;
    }
    Some((result, 9))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("test")
            .join("data_cursor")
            .join("state.vscdb")
    }

    fn large_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("test")
            .join("data_cursor")
            .join("state_large.vscdb")
    }

    #[test]
    fn varint_roundtrip_basics() {
        assert_eq!(read_varint(&[0x00], 0), Some((0, 1)));
        assert_eq!(read_varint(&[0x7f], 0), Some((127, 1)));
        assert_eq!(read_varint(&[0x81, 0x00], 0), Some((128, 2)));
        assert_eq!(read_varint(&[0x82, 0x2c], 0), Some((300, 2)));
    }

    #[test]
    fn opens_fixture_and_reads_cursordiskkv() {
        let db = VscDb::open(&fixture()).expect("fixture should open as a SQLite db");
        let rows = db.read_table("cursorDiskKV");

        // The fixture has 3 conversation rows + 2 noise rows = 5 total.
        assert_eq!(rows.len(), 5, "expected 5 cursorDiskKV rows");

        let keys: Vec<String> = rows
            .iter()
            .map(|r| String::from_utf8_lossy(&r.key).into_owned())
            .collect();

        // composerData / bubbleId rows are present and decodable.
        let composer: Vec<_> = keys
            .iter()
            .filter(|k| k.starts_with("composerData:"))
            .collect();
        assert_eq!(composer.len(), 1, "exactly one composerData row");

        let bubbles: Vec<_> = keys.iter().filter(|k| k.starts_with("bubbleId:")).collect();
        assert_eq!(bubbles.len(), 2, "exactly two bubbleId rows");

        // Values are valid UTF-8 JSON the parser can consume.
        for r in &rows {
            if String::from_utf8_lossy(&r.key).starts_with("composerData:") {
                let v: serde_json::Value =
                    serde_json::from_slice(&r.value).expect("composer value is JSON");
                assert_eq!(v["composerId"], "comp-1111-0000-0000-0000-000000000001");
            }
        }
    }

    #[test]
    fn unknown_table_returns_empty() {
        let db = VscDb::open(&fixture()).unwrap();
        assert!(db.read_table("no_such_table").is_empty());
    }

    #[test]
    fn missing_file_returns_none() {
        assert!(VscDb::open(Path::new("/nonexistent/state.vscdb")).is_none());
    }

    #[test]
    fn reads_large_fixture_interior_pages_and_overflow() {
        // The large fixture's cursorDiskKV b-tree has an interior root page and a
        // ~100 KB value that spills across overflow pages (verified by the fixture
        // generator). This asserts the reader traverses every leaf under the
        // interior page and reassembles the overflow chain byte-for-byte.
        let db = VscDb::open(&large_fixture()).expect("large fixture opens");
        let rows = db.read_table("cursorDiskKV");

        // 1 composer + 300 bubbles: proves interior-page traversal reads all
        // leaf pages, not just the first.
        assert_eq!(rows.len(), 301, "expected 1 composer + 300 bubble rows");

        // The oversized bubble round-trips byte-exact through the overflow chain.
        let big = rows
            .iter()
            .find(|r| r.key.ends_with(b"bub-0299"))
            .expect("large bubble present");
        let v: serde_json::Value =
            serde_json::from_slice(&big.value).expect("large value is valid JSON");
        let text = v["text"].as_str().expect("text field");
        assert_eq!(text.len(), 100_000, "overflow payload reassembled to full length");
        assert!(text.starts_with("0123456789ABCDEF"));
        assert!(text.ends_with("0123456789ABCDEF"));
    }

    #[test]
    fn oversized_payload_len_is_rejected_without_panic() {
        // Corrupt one leaf cell's payload-length varint to claim far more bytes
        // than the file holds. The guard in `read_payload` must drop that row
        // (rather than attempt a huge `Vec::with_capacity` that aborts), while
        // the remaining rows still decode.
        let mut bytes = std::fs::read(fixture()).unwrap();
        // cursorDiskKV rootpage is 4 at page_size 4096 -> a single leaf page.
        let page = 3 * 4096;
        assert_eq!(bytes[page], 0x0d, "rootpage should be a leaf table page");
        let ptr = ((bytes[page + 8] as usize) << 8) | bytes[page + 9] as usize;
        let cell = page + ptr;
        // Overwrite the length varint with an 8-byte varint decoding to ~2^52,
        // an allocation no host can satisfy. Without the guard, `read_payload`
        // reaches `Vec::with_capacity(p)` and the process aborts; the guard
        // rejects it up front so the row is simply skipped.
        for (i, b) in [0x87, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F]
            .into_iter()
            .enumerate()
        {
            bytes[cell + i] = b;
        }

        let db = VscDb::from_bytes(bytes).expect("header still valid after patch");
        let rows = db.read_table("cursorDiskKV"); // must not panic / not over-allocate
        assert!(
            rows.len() < 5,
            "the corrupted oversized row should be skipped"
        );
    }
}
