use crate::detect::{self, Provider};
use crate::codex_store::{self, CodexSurface};
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

        BufReader::new(file).lines().enumerate().filter_map(|(line_idx, line_result)| {
            let line = line_result.ok()?;
            if line.trim().is_empty() { return None; }

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
        }).collect()
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

        history.command_history.into_iter().enumerate().map(|(idx, cmd)| {
            HistoryRow {
                source: "copilot".to_string(),
                line_number: (idx + 1) as i64,
                timestamp_ms: None,
                project: None,
                session_id: None,
                display: Some(cmd),
                pasted_contents: None,
            }
        }).collect()
    }

    fn load_codex_rows(base_path: &std::path::Path) -> Vec<HistoryRow> {
        let history_path = utils::history_file_path(base_path);
        let file = match std::fs::File::open(&history_path) {
            Ok(file) => file,
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
                Some(match serde_json::from_str::<CodexHistoryEntry>(&line) {
                    Ok(entry) => HistoryRow {
                        source: "codex".to_string(),
                        line_number,
                        timestamp_ms: entry.ts.and_then(|seconds| seconds.checked_mul(1_000)),
                        project: None,
                        session_id: entry.session_id,
                        display: entry.text,
                        pasted_contents: None,
                    },
                    Err(error) => HistoryRow {
                        source: "codex".to_string(),
                        line_number,
                        timestamp_ms: None,
                        project: None,
                        session_id: None,
                        display: Some(format!("Parse error: {}", error)),
                        pasted_contents: None,
                    },
                })
            })
            .collect()
    }

    fn load_codex_surface_rows(
        base_path: &std::path::Path,
        surface: CodexSurface,
    ) -> Vec<HistoryRow> {
        let store = codex_store::read_codex_thread_store(base_path, surface);
        store
            .items
            .into_iter()
            .filter(|item| item.item_type == "userMessage")
            .enumerate()
            .map(|(index, item)| {
                let thread = store.threads.get(&item.thread_id);
                let payload: serde_json::Value = serde_json::from_str(&item.item_json)
                    .unwrap_or(serde_json::Value::Null);
                let display = payload
                    .get("content")
                    .map(utils::extract_text_content)
                    .or_else(|| payload.get("text").and_then(|value| value.as_str()).map(String::from));
                HistoryRow {
                    source: surface.source_label().to_string(),
                    line_number: (index + 1) as i64,
                    timestamp_ms: Some(item.created_at_ms).filter(|timestamp| *timestamp > 0),
                    project: thread.and_then(|thread| {
                        (!thread.cwd.is_empty()).then(|| thread.cwd.clone())
                    }),
                    session_id: Some(item.thread_id),
                    display,
                    pasted_contents: None,
                }
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
            Provider::Codex => Self::load_codex_rows(&base_path),
            Provider::CodexWork => Self::load_codex_surface_rows(&base_path, CodexSurface::Work),
            Provider::CodexRemote => Self::load_codex_surface_rows(&base_path, CodexSurface::Remote),
            Provider::CodexChat => Self::load_codex_surface_rows(&base_path, CodexSurface::Chat),
            // Claude Desktop has no history.jsonl. Cursor has no command-history
            // equivalent. Gemini keeps user prompts in tmp/<hash>/logs.json,
            // already surfaced as `user` rows in read_conversations; Grok's prompt
            // history is a candidate source — deferred to a follow-up. Return empty.
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
