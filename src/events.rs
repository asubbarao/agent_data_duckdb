//! `read_events(path, source)` — the lossless raw JSONL relation.
//!
//! Where `read_conversations()` normalizes each provider into one shared,
//! opinionated schema (and therefore drops fields it has no column for),
//! `read_events()` hands back every physical line of every transcript file
//! exactly as it sits on disk, addressed by `file_path` + `line_number` +
//! `byte_offset`. Nothing is filtered: unknown event types, malformed JSON,
//! blank lines and a truncated final line all come back as rows. Parsing is
//! limited to two *optional* convenience columns (`event_type`, `timestamp`);
//! everything else stays in `raw` for the caller to pick apart (e.g. with the
//! `json` extension).
//!
//! Supported sources: `'claude'` and `'codex'`. Anything else is an error
//! rather than an empty result, so a typo never looks like "no data".

use crate::detect::{self, Provider};
use crate::utils;
use crate::vtab::{self, ColDef, TableFunc};
use duckdb::core::DataChunkHandle;
use std::error::Error;
use std::path::{Path, PathBuf};

/// One physical line of one transcript file, preserved verbatim.
pub struct EventRow {
    source: &'static str,
    session_id: String,
    file_path: String,
    file_name: String,
    line_number: i64,
    byte_offset: i64,
    byte_length: i64,
    line_ending: Option<&'static str>,
    raw: String,
    raw_is_exact: bool,
    is_valid_json: bool,
    parse_error: Option<String>,
    event_type: Option<String>,
    timestamp: Option<String>,
    raw_bytes: Vec<u8>,
}

pub struct Events;

impl Events {
    /// Resolve provider + path, then scan every transcript file it owns.
    ///
    /// Errors (rather than returning zero rows) on a missing/unusable path and
    /// on a provider `read_events` does not implement: a raw reader that
    /// silently returns nothing is indistinguishable from a lossless read of an
    /// empty directory.
    fn load(path: Option<&str>, source: Option<&str>) -> Result<Vec<EventRow>, Box<dyn Error>> {
        let base_path = utils::resolve_data_path(path);

        if !base_path.exists() {
            return Err(
                format!("read_events: path '{}' does not exist", base_path.display()).into(),
            );
        }
        if !base_path.is_dir() {
            return Err(format!(
                "read_events: path '{}' is not a directory (pass the provider data \
                 directory, e.g. '~/.claude' or '~/.codex')",
                base_path.display()
            )
            .into());
        }

        let base_path = base_path.canonicalize()?;
        // An explicit typo must not fall back to auto-detection.
        let provider = source
            .map(detect::parse_source)
            .unwrap_or_else(|| detect::detect_provider(&base_path));
        let (source_name, files) = match provider {
            Provider::Claude => ("claude", Self::claude_files(&base_path)?),
            Provider::Codex => {
                let mut files = Vec::new();
                let mut seen = std::collections::HashSet::new();
                Self::codex_files(&base_path.join("sessions"), &mut files, &mut seen)?;
                files.sort_by(|a, b| a.1.cmp(&b.1));
                ("codex", files)
            }
            other => {
                return Err(format!(
                    "read_events: unsupported provider {:?} for path '{}' — read_events \
                     supports source := 'claude' or 'codex' only (use read_conversations() \
                     for the other providers)",
                    other,
                    base_path.display()
                )
                .into())
            }
        };

        let mut rows = Vec::new();
        for (session_id, file_path) in files {
            Self::scan_file(source_name, &session_id, &file_path, &mut rows)?;
        }
        Ok(rows)
    }

    /// Same transcript layout as the normalized reader, with fallible traversal.
    /// The session id is file-derived; an event's own id remains in `raw`.
    fn claude_files(base_path: &Path) -> Result<Vec<(String, PathBuf)>, Box<dyn Error>> {
        let mut files = Vec::new();
        for project in entries(&base_path.join("projects"))? {
            if !project.metadata()?.is_dir() {
                continue;
            }
            for entry in entries(&project.path())? {
                let path = entry.path();
                if entry.metadata()?.is_dir() {
                    for subagent in entries(&path.join("subagents"))? {
                        let subpath = subagent.path();
                        if subagent.metadata()?.is_file()
                            && subagent.file_name().to_string_lossy().starts_with("agent-")
                            && subpath.extension().is_some_and(|e| e == "jsonl")
                        {
                            files.push((utils::fallback_session_id(&subpath), subpath));
                        }
                    }
                } else if path.extension().is_some_and(|e| e == "jsonl") {
                    files.push((utils::fallback_session_id(&path), path));
                }
            }
        }
        files.sort_by(|a, b| a.1.cmp(&b.1));
        Ok(files)
    }

    fn codex_files(
        dir: &Path,
        files: &mut Vec<(String, PathBuf)>,
        seen: &mut std::collections::HashSet<PathBuf>,
    ) -> Result<(), Box<dyn Error>> {
        let children = entries(dir)?;
        if children.is_empty() {
            return Ok(());
        }
        if !seen.insert(dir.canonicalize()?) {
            return Ok(());
        }
        for entry in children {
            let path = entry.path();
            if entry.metadata()?.is_dir() {
                Self::codex_files(&path, files, seen)?;
            } else {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(stem) = name
                    .strip_prefix("rollout-")
                    .and_then(|n| n.strip_suffix(".jsonl"))
                {
                    let session = stem
                        .rsplit('-')
                        .take(5)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("-");
                    files.push((session, path));
                }
            }
        }
        Ok(())
    }

    /// Split one file into physical lines without normalizing anything.
    ///
    /// Newline handling: `\n` (LF) and `\r\n` (CRLF) terminate a line; the
    /// terminator is reported in `line_ending` and excluded from `raw` and
    /// `byte_length`. A bare `\r` is *not* a terminator — it stays inside
    /// `raw`. A final line with no terminator (a partially written record)
    /// gets `line_ending = NULL`. An empty line is a row with `raw = ''`.
    fn scan_file(
        source: &'static str,
        session_id: &str,
        file_path: &Path,
        out: &mut Vec<EventRow>,
    ) -> Result<(), Box<dyn Error>> {
        let bytes = std::fs::read(file_path).map_err(|e| {
            format!(
                "read_events: failed to read '{}': {}",
                file_path.display(),
                e
            )
        })?;

        let path_str = file_path.to_string_lossy().to_string();
        let file_name = file_path
            .file_name()
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();

        let mut offset = 0usize;
        let mut line_number = 0i64;

        while offset < bytes.len() {
            let newline = bytes[offset..].iter().position(|b| *b == b'\n');
            let (content_end, line_ending, next) = match newline {
                Some(i) => {
                    let lf_at = offset + i;
                    if lf_at > offset && bytes[lf_at - 1] == b'\r' {
                        (lf_at - 1, Some("CRLF"), lf_at + 1)
                    } else {
                        (lf_at, Some("LF"), lf_at + 1)
                    }
                }
                None => (bytes.len(), None, bytes.len()),
            };

            let raw_bytes = &bytes[offset..content_end];
            let (raw, raw_is_exact) = decode_line(raw_bytes);
            let (is_valid_json, parse_error, event_type, timestamp) = probe_json(raw_bytes);

            line_number += 1;
            out.push(EventRow {
                source,
                session_id: session_id.to_string(),
                file_path: path_str.clone(),
                file_name: file_name.clone(),
                line_number,
                byte_offset: offset as i64,
                byte_length: raw_bytes.len() as i64,
                line_ending,
                raw,
                raw_is_exact,
                is_valid_json,
                parse_error,
                event_type,
                timestamp,
                raw_bytes: raw_bytes.to_vec(),
            });

            offset = next;
        }

        Ok(())
    }
}

/// Missing provider subtrees are empty; permission and traversal errors are not.
fn entries(dir: &Path) -> Result<Vec<std::fs::DirEntry>, Box<dyn Error>> {
    let reader = match std::fs::read_dir(dir) {
        Ok(reader) => reader,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(format!("read_events: failed to list '{}': {e}", dir.display()).into())
        }
    };
    let mut entries = reader
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("read_events: failed to list '{}': {e}", dir.display()))?;
    entries.sort_by_key(|e| e.file_name());
    Ok(entries)
}

/// VARCHAR needs UTF-8; `raw_bytes` preserves invalid input without replacement.
fn decode_line(bytes: &[u8]) -> (String, bool) {
    (
        String::from_utf8_lossy(bytes).into_owned(),
        std::str::from_utf8(bytes).is_ok(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir()
                .join(format!("agent-data-events-{}-{nonce}", std::process::id()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn corrupt_bytes_and_unterminated_records_round_trip() {
        let fixture = Fixture::new();
        let path = fixture.0.join("events.jsonl");
        let original = b"{\"x\":\"\0\"}\r\n\xff\n\n{\"partial\":";
        std::fs::write(&path, original).unwrap();
        let mut rows = Vec::new();
        Events::scan_file("codex", "session", &path, &mut rows).unwrap();
        assert_eq!(rows.len(), 4);
        assert!(rows[0].raw_is_exact);
        assert!(rows[0].raw.contains('\0'));
        assert!(!rows[1].raw_is_exact);
        assert_eq!(rows[1].raw_bytes, b"\xff");
        assert!(rows[2].raw_bytes.is_empty());
        assert!(rows[3].line_ending.is_none());
        let mut rebuilt = Vec::new();
        for row in &rows {
            assert_eq!(row.byte_offset as usize, rebuilt.len());
            assert_eq!(row.byte_length as usize, row.raw_bytes.len());
            rebuilt.extend_from_slice(&row.raw_bytes);
            match row.line_ending {
                Some("CRLF") => rebuilt.extend_from_slice(b"\r\n"),
                Some("LF") => rebuilt.push(b'\n'),
                None => {}
                _ => unreachable!(),
            }
        }
        assert_eq!(rebuilt, original);
    }

    #[test]
    fn explicit_unknown_source_never_falls_back() {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.0.join("projects")).unwrap();
        let error = Events::load(fixture.0.to_str(), Some("claudee"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("unsupported provider"));
        assert!(Events::load(fixture.0.to_str(), Some("claude"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn broken_discovery_tree_is_an_error() {
        let fixture = Fixture::new();
        std::fs::write(fixture.0.join("projects"), b"not a directory").unwrap();
        let error = Events::load(fixture.0.to_str(), Some("claude"))
            .err()
            .unwrap();
        assert!(error.to_string().contains("failed to list"));
    }

    #[test]
    fn missing_transcript_is_an_error() {
        let fixture = Fixture::new();
        let error = Events::scan_file(
            "codex",
            "session",
            &fixture.0.join("absent.jsonl"),
            &mut Vec::new(),
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("failed to read"));
    }
}

/// Best-effort, non-destructive peek at a line: is it JSON, and if so what are
/// its top-level `type` / `timestamp`? Unknown and provider-specific shapes are
/// reported as-is — no mapping table, no dropped values.
fn probe_json(bytes: &[u8]) -> (bool, Option<String>, Option<String>, Option<String>) {
    match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(value) => {
            let str_field = |key: &str| {
                value
                    .get(key)
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            };
            (true, None, str_field("type"), str_field("timestamp"))
        }
        Err(e) => (false, Some(e.to_string()), None, None),
    }
}

impl TableFunc for Events {
    type Row = EventRow;

    fn columns() -> Vec<ColDef> {
        vec![
            vtab::varchar("source"),
            vtab::varchar("session_id"),
            vtab::varchar("file_path"),
            vtab::varchar("file_name"),
            vtab::bigint("line_number"),
            vtab::bigint("byte_offset"),
            vtab::bigint("byte_length"),
            vtab::varchar("line_ending"),
            vtab::varchar("raw"),
            vtab::boolean("raw_is_exact"),
            vtab::boolean("is_valid_json"),
            vtab::varchar("parse_error"),
            vtab::varchar("event_type"),
            vtab::varchar("timestamp"),
            vtab::blob("raw_bytes"),
        ]
    }

    fn load_rows(path: Option<&str>, source: Option<&str>) -> Vec<EventRow> {
        Self::load(path, source).unwrap_or_default()
    }

    fn try_load_rows(
        path: Option<&str>,
        source: Option<&str>,
    ) -> Result<Vec<EventRow>, Box<dyn Error>> {
        Self::load(path, source)
    }

    fn write_row(output: &mut DataChunkHandle, idx: usize, row: &EventRow) {
        vtab::set_varchar(output, 0, idx, row.source);
        vtab::set_varchar(output, 1, idx, &row.session_id);
        vtab::set_varchar(output, 2, idx, &row.file_path);
        vtab::set_varchar(output, 3, idx, &row.file_name);
        vtab::set_i64(output, 4, idx, row.line_number);
        vtab::set_i64(output, 5, idx, row.byte_offset);
        vtab::set_i64(output, 6, idx, row.byte_length);
        vtab::set_varchar_opt(output, 7, idx, row.line_ending);
        vtab::set_varchar(output, 8, idx, &row.raw);
        vtab::set_bool(output, 9, idx, row.raw_is_exact);
        vtab::set_bool(output, 10, idx, row.is_valid_json);
        vtab::set_varchar_opt(output, 11, idx, row.parse_error.as_deref());
        vtab::set_varchar_opt(output, 12, idx, row.event_type.as_deref());
        vtab::set_varchar_opt(output, 13, idx, row.timestamp.as_deref());
        vtab::set_blob(output, 14, idx, &row.raw_bytes);
    }
}
