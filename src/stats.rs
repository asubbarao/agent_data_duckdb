use crate::detect::{self, Provider};
use crate::codex_store::{self, CodexSurface};
use crate::types::claude::StatsCache;
use crate::types::codex::{CodexLine, CodexResponseItem};
use crate::utils;
use crate::vtab::{self, ColDef, TableFunc};
use duckdb::core::DataChunkHandle;
use std::collections::BTreeMap;

pub struct StatsRow {
    source: String,
    date: String,
    message_count: i64,
    session_count: i64,
    tool_call_count: i64,
}

pub struct Stats;

impl Stats {
    /// Codex CLI has no stats-cache file. Aggregate its canonical rollout
    /// messages and function calls by session start day without double-counting
    /// the event-message fallback copies.
    fn load_codex_rows(base_path: &std::path::Path) -> Vec<StatsRow> {
        let mut by_date: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();

        for (_session_id, file_path) in utils::discover_codex_rollout_files(base_path) {
            let file = match std::fs::File::open(file_path) {
                Ok(file) => file,
                Err(_) => continue,
            };
            let mut date = None;
            let mut canonical_messages = 0i64;
            let mut fallback_messages = 0i64;
            let mut tool_calls = 0i64;

            for line in std::io::BufRead::lines(std::io::BufReader::new(file)).map_while(Result::ok) {
                let Ok(line) = serde_json::from_str::<CodexLine>(&line) else {
                    continue;
                };
                if date.is_none() {
                    date = line.timestamp.as_deref().and_then(utils::iso_date);
                }
                match line.line_type.as_str() {
                    "response_item" => {
                        let Ok(item) = serde_json::from_value::<CodexResponseItem>(line.payload) else {
                            continue;
                        };
                        match item.item_type.as_deref() {
                            Some("message") if matches!(item.role.as_deref(), Some("user" | "assistant")) => {
                                canonical_messages += 1;
                            }
                            Some("function_call") => tool_calls += 1,
                            _ => {}
                        }
                    }
                    "event_msg" => {
                        let event_type = line.payload.get("type").and_then(|value| value.as_str());
                        if matches!(event_type, Some("user_message" | "agent_message")) {
                            fallback_messages += 1;
                        }
                    }
                    _ => {}
                }
            }

            let Some(date) = date else {
                continue;
            };
            let entry = by_date.entry(date).or_insert((0, 0, 0));
            entry.0 += if canonical_messages > 0 {
                canonical_messages
            } else {
                fallback_messages
            };
            entry.1 += 1;
            entry.2 += tool_calls;
        }

        by_date
            .into_iter()
            .map(|(date, (message_count, session_count, tool_call_count))| StatsRow {
                source: "codex".to_string(),
                date,
                message_count,
                session_count,
                tool_call_count,
            })
            .collect()
    }

    fn load_codex_surface_rows(
        base_path: &std::path::Path,
        surface: CodexSurface,
    ) -> Vec<StatsRow> {
        let store = codex_store::read_codex_thread_store(base_path, surface);
        let mut by_date: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();

        for thread in store.threads.values() {
            if let Some(date) = utils::unix_ms_to_date(thread.created_at_ms) {
                by_date.entry(date).or_insert((0, 0, 0)).1 += 1;
            }
        }
        for item in store.items {
            let Some(date) = utils::unix_ms_to_date(item.created_at_ms) else {
                continue;
            };
            let entry = by_date.entry(date).or_insert((0, 0, 0));
            match item.item_type.as_str() {
                "userMessage" | "agentMessage" => entry.0 += 1,
                "commandExecution" | "mcpToolCall" | "webSearch" | "fileChange"
                | "collabAgentToolCall" => entry.2 += 1,
                _ => {}
            }
        }

        by_date
            .into_iter()
            .map(|(date, (message_count, session_count, tool_call_count))| StatsRow {
                source: surface.source_label().to_string(),
                date,
                message_count,
                session_count,
                tool_call_count,
            })
            .collect()
    }

    /// Grok has no stats-cache.json. Roll up per-session `signals.json` (+
    /// summary fallbacks) into the existing daily stats columns — one row per
    /// date, rejectable by maintainers (no new table function).
    fn load_grok_rows(base_path: &std::path::Path) -> Vec<StatsRow> {
        // date → (messages, sessions, tool_calls)
        let mut by_date: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();

        for (_session_uuid, _cwd, _enc, chat_path) in utils::discover_grok_session_files(base_path)
        {
            let session_dir = chat_path.parent().unwrap_or(&chat_path);
            let summary = utils::read_grok_summary(session_dir);
            let signals = utils::read_grok_signals(session_dir);

            let date = summary
                .as_ref()
                .and_then(|s| s.created_at.as_deref())
                .and_then(utils::grok_date_from_timestamp)
                .unwrap_or_else(|| "unknown".to_string());

            let message_count = signals
                .as_ref()
                .map(|sig| {
                    let u = sig.user_message_count.unwrap_or(0);
                    let a = sig.assistant_message_count.unwrap_or(0);
                    if u > 0 || a > 0 {
                        u + a
                    } else {
                        sig.turn_count.unwrap_or(0)
                    }
                })
                .filter(|&n| n > 0)
                .or_else(|| summary.as_ref().and_then(|s| s.num_messages))
                .unwrap_or(0);

            let tool_call_count = signals
                .as_ref()
                .and_then(|s| s.tool_call_count)
                .unwrap_or(0);

            let entry = by_date.entry(date).or_insert((0, 0, 0));
            entry.0 += message_count;
            entry.1 += 1;
            entry.2 += tool_call_count;
        }

        by_date
            .into_iter()
            .map(|(date, (message_count, session_count, tool_call_count))| StatsRow {
                source: "grok".to_string(),
                date,
                message_count,
                session_count,
                tool_call_count,
            })
            .collect()
    }
}

impl TableFunc for Stats {
    type Row = StatsRow;

    fn columns() -> Vec<ColDef> {
        vec![
            vtab::varchar("source"),
            vtab::varchar("date"),
            vtab::bigint("message_count"),
            vtab::bigint("session_count"),
            vtab::bigint("tool_call_count"),
        ]
    }

    fn load_rows(path: Option<&str>, source: Option<&str>) -> Vec<StatsRow> {
        let base_path = utils::resolve_data_path(path);
        match detect::resolve_provider(&base_path, source) {
            Provider::Claude => {
                let stats_path = utils::stats_file_path(&base_path);
                let content = match std::fs::read_to_string(&stats_path) {
                    Ok(c) => c,
                    Err(_) => return Vec::new(),
                };
                let cache: StatsCache = match serde_json::from_str(&content) {
                    Ok(c) => c,
                    Err(_) => return Vec::new(),
                };
                cache.daily_activity.unwrap_or_default().into_iter().map(|day| StatsRow {
                    source: "claude".to_string(),
                    date: day.date.unwrap_or_default(),
                    message_count: day.message_count.unwrap_or(0),
                    session_count: day.session_count.unwrap_or(0),
                    tool_call_count: day.tool_call_count.unwrap_or(0),
                }).collect()
            }
            Provider::Grok => Self::load_grok_rows(&base_path),
            Provider::Codex => Self::load_codex_rows(&base_path),
            Provider::CodexWork => Self::load_codex_surface_rows(&base_path, CodexSurface::Work),
            Provider::CodexRemote => Self::load_codex_surface_rows(&base_path, CodexSurface::Remote),
            Provider::CodexChat => Self::load_codex_surface_rows(&base_path, CodexSurface::Chat),
            // Only Claude ships stats-cache.json; Grok and Codex roll up their
            // own durable local stores. Other providers derive in SQL.
            Provider::ClaudeDesktop
            | Provider::Copilot
            | Provider::Cursor
            | Provider::Gemini
            | Provider::Unknown => Vec::new(),
        }
    }

    fn write_row(output: &mut DataChunkHandle, idx: usize, row: &StatsRow) {
        vtab::set_varchar(output, 0, idx, &row.source);
        vtab::set_varchar(output, 1, idx, &row.date);
        vtab::set_i64(output, 2, idx, row.message_count);
        vtab::set_i64(output, 3, idx, row.session_count);
        vtab::set_i64(output, 4, idx, row.tool_call_count);
    }
}
