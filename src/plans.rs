use crate::detect::{self, Provider};
use crate::codex_store::{self, CodexSurface, CodexThreadItem};
use crate::utils;
use crate::vtab::{self, ColDef, TableFunc};
use duckdb::core::DataChunkHandle;

pub struct PlanRow {
    source: String,
    session_id: Option<String>,
    plan_name: String,
    file_name: String,
    file_path: String,
    content: String,
    file_size: i64,
}

pub struct Plans;

impl Plans {
    fn load_claude_rows(base_path: &std::path::Path) -> Vec<PlanRow> {
        utils::discover_plan_files(base_path).into_iter().filter_map(|file_path| {
            let content = std::fs::read_to_string(&file_path).ok()?;
            let file_size = std::fs::metadata(&file_path).map(|m| m.len() as i64).unwrap_or(0);
            Some(PlanRow {
                source: "claude".to_string(),
                session_id: None,
                plan_name: file_path.file_stem()?.to_string_lossy().to_string(),
                file_name: file_path.file_name()?.to_string_lossy().to_string(),
                file_path: file_path.to_string_lossy().to_string(),
                content,
                file_size,
            })
        }).collect()
    }

    fn load_copilot_rows(base_path: &std::path::Path) -> Vec<PlanRow> {
        utils::discover_copilot_plan_files(base_path).into_iter().filter_map(|(session_id, file_path)| {
            let content = std::fs::read_to_string(&file_path).ok()?;
            let file_size = std::fs::metadata(&file_path).map(|m| m.len() as i64).unwrap_or(0);
            let workspace = file_path.parent().and_then(|p| utils::read_workspace_yaml(p));
            let plan_name = workspace.and_then(|w| w.summary).unwrap_or_else(|| session_id.clone());
            Some(PlanRow {
                source: "copilot".to_string(),
                session_id: Some(session_id),
                plan_name,
                file_name: file_path.file_name()?.to_string_lossy().to_string(),
                file_path: file_path.to_string_lossy().to_string(),
                content,
                file_size,
            })
        }).collect()
    }

    /// Grok writes a `plan.md` into each session directory (when plan mode is used).
    fn load_grok_rows(base_path: &std::path::Path) -> Vec<PlanRow> {
        utils::discover_grok_session_files(base_path).into_iter().filter_map(|(session_id, _cwd, _enc, chat_path)| {
            let plan_path = chat_path.parent()?.join("plan.md");
            let content = std::fs::read_to_string(&plan_path).ok()?;
            let file_size = std::fs::metadata(&plan_path).map(|m| m.len() as i64).unwrap_or(0);
            let summary = chat_path.parent().and_then(utils::read_grok_summary);
            let plan_name = summary
                .and_then(|s| s.generated_title)
                .unwrap_or_else(|| session_id.clone());
            Some(PlanRow {
                source: "grok".to_string(),
                session_id: Some(session_id),
                plan_name,
                file_name: "plan.md".to_string(),
                file_path: plan_path.to_string_lossy().to_string(),
                content,
                file_size,
            })
        }).collect()
    }

    fn load_codex_rows(base_path: &std::path::Path) -> Vec<PlanRow> {
        utils::discover_codex_rollout_files(base_path)
            .into_iter()
            .filter_map(|(fallback_session_id, file_path)| {
                let snapshot = utils::read_codex_latest_plan(&file_path, &fallback_session_id)?;
                let file_name = file_path.file_name()?.to_string_lossy().to_string();
                let file_size = std::fs::metadata(&file_path).ok()?.len() as i64;
                let mut content = snapshot.explanation.unwrap_or_default();

                for step in snapshot.steps {
                    if !content.is_empty() {
                        content.push('\n');
                    }
                    match step.status.as_str() {
                        "completed" => content.push_str(&format!("- [x] {}", step.step)),
                        "in_progress" => content.push_str(&format!(
                            "- [ ] {} _(in progress)_",
                            step.step
                        )),
                        "pending" | "" => content.push_str(&format!("- [ ] {}", step.step)),
                        status => content.push_str(&format!("- [ ] {} _({})_", step.step, status)),
                    }
                }

                Some(PlanRow {
                    source: "codex".to_string(),
                    session_id: Some(snapshot.session_id),
                    plan_name: "Codex plan".to_string(),
                    file_name,
                    file_path: file_path.to_string_lossy().to_string(),
                    content,
                    file_size,
                })
            })
            .collect()
    }

    fn load_codex_surface_rows(
        base_path: &std::path::Path,
        surface: CodexSurface,
    ) -> Vec<PlanRow> {
        let store = codex_store::read_codex_thread_store(base_path, surface);
        let mut latest: std::collections::HashMap<String, CodexThreadItem> =
            std::collections::HashMap::new();
        for item in store.items.into_iter().filter(|item| item.item_type == "plan") {
            let replace = latest
                .get(&item.thread_id)
                .map(|previous| {
                    (item.rollout_ordinal, item.updated_at_ordinal)
                        > (previous.rollout_ordinal, previous.updated_at_ordinal)
                })
                .unwrap_or(true);
            if replace {
                latest.insert(item.thread_id.clone(), item);
            }
        }
        let file_path = base_path.join("thread_history_1.sqlite");
        let file_name = "thread_history_1.sqlite".to_string();
        let file_size = std::fs::metadata(&file_path)
            .map(|metadata| metadata.len() as i64)
            .unwrap_or(0);
        let mut rows: Vec<_> = latest
            .into_values()
            .filter_map(|item| {
                let thread = store.threads.get(&item.thread_id)?;
                let payload: serde_json::Value = serde_json::from_str(&item.item_json).ok()?;
                let content = payload.get("text")?.as_str()?.to_string();
                Some(PlanRow {
                    source: surface.source_label().to_string(),
                    session_id: Some(thread.id.clone()),
                    plan_name: if thread.title.is_empty() {
                        "Codex plan".to_string()
                    } else {
                        thread.title.clone()
                    },
                    file_name: file_name.clone(),
                    file_path: file_path.to_string_lossy().to_string(),
                    content,
                    file_size,
                })
            })
            .collect();
        rows.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        rows
    }
}

impl TableFunc for Plans {
    type Row = PlanRow;

    fn columns() -> Vec<ColDef> {
        vec![
            vtab::varchar("source"),
            vtab::varchar("session_id"),
            vtab::varchar("plan_name"),
            vtab::varchar("file_name"),
            vtab::varchar("file_path"),
            vtab::varchar("content"),
            vtab::bigint("file_size"),
        ]
    }

    fn load_rows(path: Option<&str>, source: Option<&str>) -> Vec<PlanRow> {
        let base_path = utils::resolve_data_path(path);
        match detect::resolve_provider(&base_path, source) {
            Provider::Claude => Self::load_claude_rows(&base_path),
            Provider::Copilot => Self::load_copilot_rows(&base_path),
            Provider::Grok => Self::load_grok_rows(&base_path),
            Provider::Codex => Self::load_codex_rows(&base_path),
            Provider::CodexWork => Self::load_codex_surface_rows(&base_path, CodexSurface::Work),
            Provider::CodexRemote => Self::load_codex_surface_rows(&base_path, CodexSurface::Remote),
            Provider::CodexChat => Self::load_codex_surface_rows(&base_path, CodexSurface::Chat),
            // Claude Desktop has no top-level plans/ directory; Cursor has no
            // standalone plan files; Gemini plan steps live inline in the chat
            // transcript (no standalone plan files). Return empty.
            Provider::ClaudeDesktop
            | Provider::Cursor
            | Provider::Gemini
            | Provider::Unknown => Vec::new(),
        }
    }

    fn write_row(output: &mut DataChunkHandle, idx: usize, row: &PlanRow) {
        vtab::set_varchar(output, 0, idx, &row.source);
        vtab::set_varchar_opt(output, 1, idx, row.session_id.as_deref());
        vtab::set_varchar(output, 2, idx, &row.plan_name);
        vtab::set_varchar(output, 3, idx, &row.file_name);
        vtab::set_varchar(output, 4, idx, &row.file_path);
        vtab::set_varchar(output, 5, idx, &row.content);
        vtab::set_i64(output, 6, idx, row.file_size);
    }
}
