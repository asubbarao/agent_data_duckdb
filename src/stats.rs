use crate::detect::{self, Provider};
use crate::types::claude::StatsCache;
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
            .map(
                |(date, (message_count, session_count, tool_call_count))| StatsRow {
                    source: "grok".to_string(),
                    date,
                    message_count,
                    session_count,
                    tool_call_count,
                },
            )
            .collect()
    }

    /// Codex has no stats cache. Roll up rollout rows (as `read_conversations`
    /// emits them) by UTC date: user/assistant turns (harness envelopes
    /// excluded), tool calls, and sessions counted on the date of their first
    /// timestamped row.
    fn load_codex_rows(path: Option<&str>) -> Vec<StatsRow> {
        let date_of = |ts: &Option<String>| -> Option<String> {
            ts.as_deref()
                .and_then(|t| t.get(..10))
                .filter(|d| {
                    d.char_indices().all(|(i, c)| match i {
                        4 | 7 => c == '-',
                        _ => c.is_ascii_digit(),
                    })
                })
                .map(str::to_string)
        };
        // date → (messages, sessions, tool_calls)
        let mut by_date: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();
        // session → earliest timestamp seen
        let mut session_start: BTreeMap<String, Option<String>> = BTreeMap::new();

        for (session_id, timestamp, message_type, author) in
            crate::conversations::Conversations::codex_activity(path)
        {
            let start = session_start.entry(session_id).or_insert(None);
            if timestamp.is_some() && (start.is_none() || timestamp < *start) {
                *start = timestamp.clone();
            }
            let is_message = matches!(
                message_type.as_str(),
                "user" | "assistant" | "agent_message"
            ) && author != "system";
            let is_tool_call = matches!(
                message_type.as_str(),
                "function_call" | "custom_tool_call" | "local_shell_call" | "web_search_call"
            );
            if !is_message && !is_tool_call {
                continue;
            }
            let date = date_of(&timestamp).unwrap_or_else(|| "unknown".to_string());
            let entry = by_date.entry(date).or_insert((0, 0, 0));
            entry.0 += is_message as i64;
            entry.2 += is_tool_call as i64;
        }
        for start in session_start.values() {
            let date = date_of(start).unwrap_or_else(|| "unknown".to_string());
            by_date.entry(date).or_insert((0, 0, 0)).1 += 1;
        }

        by_date
            .into_iter()
            .map(
                |(date, (message_count, session_count, tool_call_count))| StatsRow {
                    source: "codex".to_string(),
                    date,
                    message_count,
                    session_count,
                    tool_call_count,
                },
            )
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
                cache
                    .daily_activity
                    .unwrap_or_default()
                    .into_iter()
                    .map(|day| StatsRow {
                        source: "claude".to_string(),
                        date: day.date.unwrap_or_default(),
                        message_count: day.message_count.unwrap_or(0),
                        session_count: day.session_count.unwrap_or(0),
                        tool_call_count: day.tool_call_count.unwrap_or(0),
                    })
                    .collect()
            }
            Provider::Grok => Self::load_grok_rows(&base_path),
            Provider::Codex => Self::load_codex_rows(path),
            // Only Claude ships stats-cache.json; Grok rolls up signals.json and
            // Codex rolls up its rollouts. Other providers: derive in SQL from
            // read_conversations() instead.
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
