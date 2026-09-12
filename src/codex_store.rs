//! Read-only access to Codex's local thread projection.
//!
//! Codex keeps durable local work state in two SQLite databases beneath
//! `~/.codex`: `state_5.sqlite` holds thread metadata and
//! `thread_history_1.sqlite` holds the ordered items for each thread. The
//! rollout reader remains the canonical CLI source; this module exposes the
//! distinct app/work, remote, and chat surfaces when Codex records them.

use crate::vscdb::VscDb;
use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexSurface {
    Work,
    Remote,
    Chat,
}

impl CodexSurface {
    pub fn source_label(self) -> &'static str {
        match self {
            CodexSurface::Work => "codex-work",
            CodexSurface::Remote => "codex-remote",
            CodexSurface::Chat => "codex-chat",
        }
    }

    fn matches_thread(self, source: &str) -> bool {
        match self {
            // `vscode` is Codex's locally persisted work surface. CLI rollouts
            // are deliberately kept in Provider::Codex to avoid double-counting.
            CodexSurface::Work => source == "vscode",
            // These labels are written by Codex when a thread has been synced
            // from the corresponding surface. They are currently absent from
            // the local sample, but the schema supports them without guessing a
            // cloud API or reading browser cache data.
            CodexSurface::Remote => source == "remote",
            CodexSurface::Chat => source == "chat",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CodexThread {
    pub id: String,
    pub source: String,
    pub cwd: String,
    pub title: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub git_branch: Option<String>,
    pub repository: Option<String>,
    pub cli_version: Option<String>,
    pub thread_source: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Default)]
pub struct CodexThreadItem {
    pub thread_id: String,
    pub item_id: String,
    pub rollout_ordinal: i64,
    pub created_at_ms: i64,
    pub item_json: String,
    pub item_type: String,
    pub updated_at_ordinal: i64,
}

#[derive(Debug, Default)]
pub struct CodexThreadStore {
    pub threads: HashMap<String, CodexThread>,
    pub items: Vec<CodexThreadItem>,
}

pub fn read_codex_thread_store(base_path: &Path, surface: CodexSurface) -> CodexThreadStore {
    let state_path = base_path.join("state_5.sqlite");
    let history_path = base_path.join("thread_history_1.sqlite");
    let mut store = CodexThreadStore::default();

    let Some(state_db) = VscDb::open(&state_path) else {
        return store;
    };

    for row in state_db.read_rows("threads") {
        let source = row.text(4).unwrap_or_default();
        if !surface.matches_thread(&source) {
            continue;
        }
        let id = row.text(0).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        store.threads.insert(
            id.clone(),
            CodexThread {
                id,
                source,
                cwd: row.text(6).unwrap_or_default(),
                title: row.text(7).unwrap_or_default(),
                model: row.text(22),
                reasoning_effort: row.text(23),
                git_branch: row.text(15),
                repository: row.text(16),
                cli_version: row.text(17),
                thread_source: row.text(27),
                created_at_ms: row.int(25).or_else(|| row.int(2).map(|v| v * 1000)).unwrap_or(0),
            },
        );
    }

    if store.threads.is_empty() {
        return store;
    }
    let thread_ids: HashSet<_> = store.threads.keys().cloned().collect();
    let Some(history_db) = VscDb::open(&history_path) else {
        return store;
    };

    for row in history_db.read_rows("thread_items") {
        let thread_id = row.text(0).unwrap_or_default();
        if !thread_ids.contains(&thread_id) {
            continue;
        }
        store.items.push(CodexThreadItem {
            thread_id,
            item_id: row.text(2).unwrap_or_default(),
            rollout_ordinal: row.int(3).unwrap_or(0),
            created_at_ms: row.int(4).unwrap_or(0),
            item_json: row.text(5).unwrap_or_default(),
            item_type: row.text(6).unwrap_or_default(),
            updated_at_ordinal: row.int(7).unwrap_or(0),
        });
    }
    store.items.sort_by(|a, b| {
        (&a.thread_id, a.rollout_ordinal, a.updated_at_ordinal)
            .cmp(&(&b.thread_id, b.rollout_ordinal, b.updated_at_ordinal))
    });
    store
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    enum TestValue<'a> {
        Null,
        Integer(i8),
        Text(&'a str),
    }

    fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
        bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
    }

    fn write_varint(mut value: u64, output: &mut Vec<u8>) {
        if value <= 0x7f {
            output.push(value as u8);
            return;
        }
        let mut chunks = Vec::new();
        while value > 0 {
            chunks.push((value & 0x7f) as u8);
            value >>= 7;
        }
        for (index, chunk) in chunks.iter().rev().enumerate() {
            output.push(if index + 1 == chunks.len() { *chunk } else { *chunk | 0x80 });
        }
    }

    fn encode_record(values: &[TestValue<'_>]) -> Vec<u8> {
        let mut serial_types = Vec::new();
        let mut body = Vec::new();
        for value in values {
            match value {
                TestValue::Null => write_varint(0, &mut serial_types),
                TestValue::Integer(value) => {
                    write_varint(1, &mut serial_types);
                    body.push(*value as u8);
                }
                TestValue::Text(value) => {
                    write_varint(13 + (value.len() as u64 * 2), &mut serial_types);
                    body.extend_from_slice(value.as_bytes());
                }
            }
        }
        let header_size = serial_types.len() + 1;
        let mut record = Vec::new();
        write_varint(header_size as u64, &mut record);
        record.extend(serial_types);
        record.extend(body);
        record
    }

    fn encode_cell(row_id: u64, values: &[TestValue<'_>]) -> Vec<u8> {
        let record = encode_record(values);
        let mut cell = Vec::new();
        write_varint(record.len() as u64, &mut cell);
        write_varint(row_id, &mut cell);
        cell.extend(record);
        cell
    }

    fn write_leaf_page(bytes: &mut [u8], page: usize, cells: Vec<Vec<u8>>) {
        const PAGE_SIZE: usize = 4096;
        let base = (page - 1) * PAGE_SIZE;
        let header = base + if page == 1 { 100 } else { 0 };
        bytes[header] = 0x0d;
        put_u16(bytes, header + 1, 0);
        put_u16(bytes, header + 3, cells.len() as u16);
        bytes[header + 7] = 0;

        let mut content_start = base + PAGE_SIZE;
        for (index, cell) in cells.iter().enumerate() {
            content_start -= cell.len();
            bytes[content_start..content_start + cell.len()].copy_from_slice(cell);
            put_u16(bytes, header + 8 + index * 2, (content_start - base) as u16);
        }
        put_u16(bytes, header + 5, (content_start - base) as u16);
    }

    fn write_single_table_db(path: &Path, table: &str, row: Vec<TestValue<'_>>) {
        const PAGE_SIZE: usize = 4096;
        let mut bytes = vec![0; PAGE_SIZE * 2];
        bytes[..16].copy_from_slice(b"SQLite format 3\0");
        put_u16(&mut bytes, 16, PAGE_SIZE as u16);
        bytes[18] = 1;
        bytes[19] = 1;
        bytes[21] = 64;
        bytes[44..48].copy_from_slice(&4u32.to_be_bytes());
        bytes[56..60].copy_from_slice(&1u32.to_be_bytes());
        let master = vec![
            TestValue::Text("table"),
            TestValue::Text(table),
            TestValue::Text(table),
            TestValue::Integer(2),
            TestValue::Text("CREATE TABLE fixture"),
        ];
        write_leaf_page(&mut bytes, 1, vec![encode_cell(1, &master)]);
        write_leaf_page(&mut bytes, 2, vec![encode_cell(1, &row)]);
        fs::write(path, bytes).unwrap();
    }

    #[test]
    fn reads_work_thread_metadata_and_items_from_generic_sqlite_tables() {
        let dir = std::env::temp_dir().join(format!(
            "agent-data-codex-store-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let mut thread = (0..31).map(|_| TestValue::Null).collect::<Vec<_>>();
        thread[0] = TestValue::Text("work-thread");
        thread[1] = TestValue::Text("rollout.jsonl");
        thread[2] = TestValue::Integer(1);
        thread[3] = TestValue::Integer(1);
        thread[4] = TestValue::Text("vscode");
        thread[6] = TestValue::Text("/workspace");
        thread[7] = TestValue::Text("Work thread");
        thread[15] = TestValue::Text("main");
        thread[16] = TestValue::Text("https://example.test/repo");
        thread[17] = TestValue::Text("0.1.0");
        thread[22] = TestValue::Text("gpt-test");
        thread[23] = TestValue::Text("high");
        thread[25] = TestValue::Integer(1);
        thread[26] = TestValue::Integer(1);
        thread[27] = TestValue::Text("user");
        write_single_table_db(&dir.join("state_5.sqlite"), "threads", thread);

        let item = vec![
            TestValue::Text("work-thread"),
            TestValue::Text("turn-1"),
            TestValue::Text("item-1"),
            TestValue::Integer(1),
            TestValue::Integer(1),
            TestValue::Text(r#"{"type":"plan","id":"item-1","text":"- [ ] Test it"}"#),
            TestValue::Text("plan"),
            TestValue::Integer(1),
        ];
        write_single_table_db(&dir.join("thread_history_1.sqlite"), "thread_items", item);

        let store = read_codex_thread_store(&dir, CodexSurface::Work);
        assert_eq!(store.threads.len(), 1);
        assert_eq!(store.items.len(), 1);
        let thread = store.threads.get("work-thread").unwrap();
        assert_eq!(thread.cwd, "/workspace");
        assert_eq!(thread.model.as_deref(), Some("gpt-test"));
        assert_eq!(store.items[0].item_type, "plan");

        fs::remove_dir_all(dir).unwrap();
    }
}
