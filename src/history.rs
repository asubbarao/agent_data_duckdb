use crate::detect::{self, Provider};
use crate::types::claude::HistoryEntry;
use crate::types::codex::CodexHistoryEntry;
use crate::types::copilot::CopilotCommandHistory;
use crate::utils;
use crate::vtab::{self, ColDef, TableFunc};
use duckdb::core::DataChunkHandle;
use std::io::{BufRead, BufReader};

pub struct HistoryRow {
    source: String,
    line_number: i64,
    timestamp_ms: Option<i64>,
    project: Option<String>,
    session_id: Option<String>,
    display: Option<String>,
    pasted_contents: Option<String>,
}

pub struct History;

impl History {
    fn load_claude_rows(base_path: &std::path::Path) -> Vec<HistoryRow> {
        let history_path = utils::history_file_path(base_path);
        let file = match std::fs::File::open(&history_path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };

        BufReader::new(file)
            .lines()
            .enumerate()
            .filter_map(|(line_idx, line_result)| {
                let line = line_result.ok()?;
                if line.trim().is_empty() {
                    return None;
                }

                let line_number = (line_idx + 1) as i64;
                Some(match serde_json::from_str::<HistoryEntry>(&line) {
                    Ok(entry) => HistoryRow {
                        source: "claude".to_string(),
                        line_number,
                        timestamp_ms: entry.timestamp.map(|t| t as i64),
                        project: entry.project,
                        session_id: entry.session_id,
                        display: entry.display,
                        pasted_contents: entry.pasted_contents.map(|v| v.to_string()),
                    },
                    Err(e) => HistoryRow {
                        source: "claude".to_string(),
                        line_number,
                        timestamp_ms: None,
                        project: None,
                        session_id: None,
                        display: Some(format!("Parse error: {}", e)),
                        pasted_contents: None,
                    },
                })
            })
            .collect()
    }

    /// Codex keeps prompt history at `<CODEX_HOME>/history.jsonl`, one
    /// `{session_id, ts, text}` object per line (`ts` in Unix seconds).
    /// Codex records no project or pasted content there; those stay NULL.
    fn load_codex_rows(path: Option<&str>) -> Vec<HistoryRow> {
        let home = crate::codex_discovery::resolve_codex_home(path);
        let history_path = if home.is_file() {
            home
        } else {
            home.join("history.jsonl")
        };
        let file = match std::fs::File::open(&history_path) {
            Ok(f) => f,
            Err(_) => return Vec::new(),
        };

        BufReader::new(file)
            .lines()
            .enumerate()
            .filter_map(|(line_idx, line_result)| {
                let line = line_result.ok()?;
                if line.trim().is_empty() {
                    return None;
                }
                let line_number = (line_idx + 1) as i64;
                let (timestamp_ms, session_id, display) =
                    match serde_json::from_str::<CodexHistoryEntry>(&line) {
                        Ok(entry) => (
                            entry.ts.map(|t| (t * 1000.0) as i64),
                            entry.session_id,
                            entry.text,
                        ),
                        Err(e) => (None, None, Some(format!("Parse error: {}", e))),
                    };
                Some(HistoryRow {
                    source: "codex".to_string(),
                    line_number,
                    timestamp_ms,
                    project: None,
                    session_id,
                    display,
                    pasted_contents: None,
                })
            })
            .collect()
    }

    fn load_copilot_rows(base_path: &std::path::Path) -> Vec<HistoryRow> {
        let history_path = utils::copilot_history_file_path(base_path);
        let content = match std::fs::read_to_string(&history_path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let history: CopilotCommandHistory = match serde_json::from_str(&content) {
            Ok(h) => h,
            Err(_) => return Vec::new(),
        };

        history
            .command_history
            .into_iter()
            .enumerate()
            .map(|(idx, cmd)| HistoryRow {
                source: "copilot".to_string(),
                line_number: (idx + 1) as i64,
                timestamp_ms: None,
                project: None,
                session_id: None,
                display: Some(cmd),
                pasted_contents: None,
            })
            .collect()
    }
}

impl TableFunc for History {
    type Row = HistoryRow;

    fn columns() -> Vec<ColDef> {
        vec![
            vtab::varchar("source"),
            vtab::bigint("line_number"),
            vtab::bigint("timestamp_ms"),
            vtab::varchar("project"),
            vtab::varchar("session_id"),
            vtab::varchar("display"),
            vtab::varchar("pasted_contents"),
        ]
    }

    fn load_rows(path: Option<&str>, source: Option<&str>) -> Vec<HistoryRow> {
        let base_path = utils::resolve_data_path(path);
        match detect::resolve_provider(&base_path, source) {
            Provider::Claude => Self::load_claude_rows(&base_path),
            Provider::Copilot => Self::load_copilot_rows(&base_path),
            Provider::Codex => Self::load_codex_rows(path),
            // Claude Desktop has no history.jsonl. Cursor has no command-history
            // equivalent. Gemini keeps user prompts in tmp/<hash>/logs.json, already
            // surfaced as `user` rows in read_conversations; Grok's prompt history
            // is a candidate source — deferred to a follow-up. Return empty.
            Provider::ClaudeDesktop
            | Provider::Cursor
            | Provider::Gemini
            | Provider::Grok
            | Provider::Unknown => Vec::new(),
        }
    }

    fn write_row(output: &mut DataChunkHandle, idx: usize, row: &HistoryRow) {
        vtab::set_varchar(output, 0, idx, &row.source);
        vtab::set_i64(output, 1, idx, row.line_number);
        vtab::set_i64_opt(output, 2, idx, row.timestamp_ms);
        vtab::set_varchar_opt(output, 3, idx, row.project.as_deref());
        vtab::set_varchar_opt(output, 4, idx, row.session_id.as_deref());
        vtab::set_varchar_opt(output, 5, idx, row.display.as_deref());
        vtab::set_varchar_opt(output, 6, idx, row.pasted_contents.as_deref());
    }
}
