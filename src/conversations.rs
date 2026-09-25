use crate::detect::{self, Provider};
use crate::types::claude::*;
use crate::types::codex::*;
use crate::types::copilot::*;
#[cfg(feature = "cursor")]
use crate::types::cursor::*;
use crate::types::gemini::*;
use crate::types::grok::*;
use crate::utils;
use crate::vtab::{self, ColDef, TableFunc};
use duckdb::core::DataChunkHandle;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// A flattened conversation row ready for output.
#[derive(Default, Clone)]
pub struct ConversationRow {
    source: String,
    session_id: String,
    project_path: String,
    project_dir: String,
    file_name: String,
    is_agent: bool,
    line_number: i64,
    message_type: String,
    uuid: Option<String>,
    parent_uuid: Option<String>,
    timestamp: Option<String>,
    message_role: Option<String>,
    message_content: Option<String>,
    model: Option<String>,
    tool_name: Option<String>,
    tool_use_id: Option<String>,
    tool_input: Option<String>,
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    cache_creation_tokens: Option<i64>,
    cache_read_tokens: Option<i64>,
    /// Grok-only: `updates.jsonl` turn_completed `reasoningTokens`. Other providers NULL.
    reasoning_tokens: Option<i64>,
    slug: Option<String>,
    git_branch: Option<String>,
    cwd: Option<String>,
    version: Option<String>,
    stop_reason: Option<String>,
    /// Grok-only: per-message effort, else session summary backfill.
    reasoning_effort: Option<String>,
    repository: Option<String>,
    // Appended columns.  Keep the original 29 fields above in their historic
    // order: SQL clients rely on positional expansion of this relation.
    record_id: Option<String>,
    file_path: Option<String>,
    byte_offset: Option<i64>,
    ordinal: Option<i64>,
    event_type: Option<String>,
    client: Option<String>,
    originator: Option<String>,
    thread_source: Option<String>,
    parent_session_id: Option<String>,
    forked_from_session_id: Option<String>,
    turn_id: Option<String>,
    root_turn_id: Option<String>,
    response_id: Option<String>,
    model_provider: Option<String>,
    git_commit: Option<String>,
    agent_path: Option<String>,
    agent_nickname: Option<String>,
    agent_role: Option<String>,
    channel: Option<String>,
    status: Option<String>,
    session_created_at: Option<String>,
    session_updated_at: Option<String>,
    archived: Option<bool>,
    usage_scope: Option<String>,
    usage_source_line: Option<i64>,
    parse_error: Option<String>,
    raw_event: Option<String>,
    metadata: Option<String>,
    // Event-message copies can precede their canonical response_item. Keep the
    // row long enough for later completion/usage correlation, then omit it.
    suppress_output: bool,
}

pub struct Conversations;

// ─── Claude loading helpers ───

impl Conversations {
    fn claude_base_row(
        source: &str,
        base: &BaseFields,
        project_dir: &str,
        file_name: &str,
        is_agent: bool,
        file_session_id: &str,
        line_number: i64,
        message_type: &str,
    ) -> ConversationRow {
        let fallback = utils::decode_project_path(project_dir);
        ConversationRow {
            source: source.to_string(),
            session_id: base
                .session_id
                .clone()
                .unwrap_or_else(|| file_session_id.to_string()),
            project_path: base.cwd.clone().unwrap_or(fallback),
            project_dir: project_dir.to_string(),
            file_name: file_name.to_string(),
            is_agent,
            line_number,
            message_type: message_type.to_string(),
            uuid: base.uuid.clone(),
            parent_uuid: base.parent_uuid.clone(),
            timestamp: base.timestamp.clone(),
            slug: base.slug.clone(),
            git_branch: base.git_branch.clone(),
            cwd: base.cwd.clone(),
            version: base.version.clone(),
            ..Default::default()
        }
    }

    fn claude_simple_row(
        source: &str,
        project_dir: &str,
        file_name: &str,
        is_agent: bool,
        file_session_id: &str,
        line_number: i64,
        message_type: &str,
    ) -> ConversationRow {
        ConversationRow {
            source: source.to_string(),
            session_id: file_session_id.to_string(),
            project_path: utils::decode_project_path(project_dir),
            project_dir: project_dir.to_string(),
            file_name: file_name.to_string(),
            is_agent,
            line_number,
            message_type: message_type.to_string(),
            ..Default::default()
        }
    }

    fn claude_message_to_row(
        source: &str,
        msg: ConversationMessage,
        project_dir: &str,
        file_name: &str,
        is_agent: bool,
        file_session_id: &str,
        line_number: i64,
    ) -> ConversationRow {
        match msg {
            ConversationMessage::User(u) => {
                let content = u
                    .message
                    .as_ref()
                    .and_then(|m| m.content.as_ref())
                    .map(utils::extract_text_content);
                let mut row = Self::claude_base_row(
                    source,
                    &u.base,
                    project_dir,
                    file_name,
                    is_agent,
                    file_session_id,
                    line_number,
                    "user",
                );
                row.message_role = Some("user".to_string());
                row.message_content = content;
                row
            }
            ConversationMessage::Assistant(a) => {
                let msg_content = a.message.as_ref();
                let mut row = Self::claude_base_row(
                    source,
                    &a.base,
                    project_dir,
                    file_name,
                    is_agent,
                    file_session_id,
                    line_number,
                    "assistant",
                );
                row.message_role = Some("assistant".to_string());

                row.message_content = msg_content.and_then(|m| m.content.as_ref()).map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| match b {
                            ContentBlock::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                });

                if let Some(blocks) = msg_content.and_then(|m| m.content.as_ref()) {
                    for b in blocks {
                        if let ContentBlock::ToolUse { id, name, input } = b {
                            row.tool_name = name.clone();
                            row.tool_use_id = id.clone();
                            row.tool_input = input.as_ref().map(|i| i.to_string());
                            break;
                        }
                    }
                }

                let usage = msg_content.and_then(|m| m.usage.as_ref());
                row.model = msg_content.and_then(|m| m.model.clone());
                row.input_tokens = usage.and_then(|u| u.input_tokens);
                row.output_tokens = usage.and_then(|u| u.output_tokens);
                row.cache_creation_tokens = usage.and_then(|u| u.cache_creation_input_tokens);
                row.cache_read_tokens = usage.and_then(|u| u.cache_read_input_tokens);
                row.stop_reason = msg_content.and_then(|m| m.stop_reason.clone());
                row
            }
            ConversationMessage::System(s) => {
                let mut row = Self::claude_base_row(
                    source,
                    &s.base,
                    project_dir,
                    file_name,
                    is_agent,
                    file_session_id,
                    line_number,
                    "system",
                );
                row.message_content = s.content.as_ref().map(utils::extract_text_content);
                row
            }
            ConversationMessage::Summary(s) => {
                let mut row = Self::claude_simple_row(
                    source,
                    project_dir,
                    file_name,
                    is_agent,
                    file_session_id,
                    line_number,
                    "summary",
                );
                row.message_content = s.summary;
                row
            }
            ConversationMessage::FileHistorySnapshot { .. } => Self::claude_simple_row(
                source,
                project_dir,
                file_name,
                is_agent,
                file_session_id,
                line_number,
                "file-history-snapshot",
            ),
            ConversationMessage::QueueOperation(q) => {
                let mut row = Self::claude_simple_row(
                    source,
                    project_dir,
                    file_name,
                    is_agent,
                    file_session_id,
                    line_number,
                    "queue-operation",
                );
                if let Some(sid) = q.session_id {
                    row.session_id = sid;
                }
                row.timestamp = q.timestamp;
                row.message_content = q.content;
                row
            }
        }
    }

    fn load_claude_rows(base_path: &std::path::Path) -> Vec<ConversationRow> {
        let files = utils::discover_conversation_files(base_path);
        Self::load_claude_jsonl_rows("claude", &files)
    }

    /// Claude Desktop ("Cowork") stores transcripts using the same camelCase
    /// schema as Claude Code, so this delegates to the shared line-parser; only
    /// the discovered file set and the `source` label differ.
    fn load_claude_desktop_rows(base_path: &std::path::Path) -> Vec<ConversationRow> {
        let files = utils::discover_claude_desktop_files(base_path);
        Self::load_claude_jsonl_rows("claude-desktop", &files)
    }

    /// Parse a set of discovered Claude-schema JSONL transcript files into rows.
    /// Shared by both `Provider::Claude` and `Provider::ClaudeDesktop`.
    fn load_claude_jsonl_rows(
        source: &str,
        files: &[(String, bool, std::path::PathBuf)],
    ) -> Vec<ConversationRow> {
        let mut rows = Vec::new();

        for (project_dir, is_agent, file_path) in files {
            let file_name = file_path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            let file_session_id = utils::fallback_session_id(file_path);

            let file = match std::fs::File::open(file_path) {
                Ok(f) => f,
                Err(_) => continue,
            };

            let file_rows_start = rows.len();
            let mut file_cwd: Option<String> = None;
            let mut file_line: i64 = 0;

            for line_result in BufReader::new(file).lines() {
                file_line += 1;
                let line = match line_result {
                    Ok(l) if !l.trim().is_empty() => l,
                    _ => continue,
                };

                let row = match serde_json::from_str::<ConversationMessage>(&line) {
                    Ok(msg) => Self::claude_message_to_row(
                        source,
                        msg,
                        project_dir,
                        &file_name,
                        *is_agent,
                        &file_session_id,
                        file_line,
                    ),
                    Err(e) => {
                        let mut row = Self::claude_simple_row(
                            source,
                            project_dir,
                            &file_name,
                            *is_agent,
                            &file_session_id,
                            file_line,
                            "_parse_error",
                        );
                        row.message_content = Some(format!("Parse error: {}", e));
                        row
                    }
                };

                if file_cwd.is_none() && row.cwd.is_some() {
                    file_cwd = row.cwd.clone();
                }
                rows.push(row);
            }

            if let Some(ref cwd) = file_cwd {
                let fallback = utils::decode_project_path(project_dir);
                for row in &mut rows[file_rows_start..] {
                    if row.project_path == fallback {
                        row.project_path = cwd.clone();
                    }
                }
            }
        }
        rows
    }
}

// ─── Copilot loading ───

/// Session-level metadata extracted from workspace.yaml and session.start events.
struct CopilotSessionMeta {
    session_id: String,
    project_path: String,
    git_branch: Option<String>,
    repository: Option<String>,
    version: Option<String>,
    model: Option<String>,
}

impl Conversations {
    fn load_copilot_rows(base_path: &std::path::Path) -> Vec<ConversationRow> {
        let event_files = utils::discover_copilot_event_files(base_path);
        let mut rows = Vec::new();

        for (dir_session_id, file_path) in &event_files {
            // Read workspace.yaml for session metadata
            let workspace = file_path.parent().and_then(|p| {
                if p.join("workspace.yaml").exists() {
                    utils::read_workspace_yaml(p)
                } else {
                    None
                }
            });

            let mut meta = CopilotSessionMeta {
                session_id: workspace
                    .as_ref()
                    .and_then(|w| w.id.clone())
                    .unwrap_or_else(|| dir_session_id.clone()),
                project_path: workspace
                    .as_ref()
                    .and_then(|w| w.cwd.clone())
                    .unwrap_or_default(),
                git_branch: workspace.as_ref().and_then(|w| w.branch.clone()),
                repository: workspace.as_ref().and_then(|w| w.repository.clone()),
                version: None,
                model: None,
            };

            let file_name = file_path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();

            let file = match std::fs::File::open(file_path) {
                Ok(f) => f,
                Err(_) => continue,
            };

            let mut file_line: i64 = 0;
            for line_result in BufReader::new(file).lines() {
                file_line += 1;
                let line = match line_result {
                    Ok(l) if !l.trim().is_empty() => l,
                    _ => continue,
                };

                let event = match serde_json::from_str::<CopilotEvent>(&line) {
                    Ok(e) => e,
                    Err(e) => {
                        rows.push(ConversationRow {
                            source: "copilot".to_string(),
                            session_id: meta.session_id.clone(),
                            file_name: file_name.clone(),
                            line_number: file_line,
                            message_type: "_parse_error".to_string(),
                            message_content: Some(format!("Parse error: {}", e)),
                            ..Default::default()
                        });
                        continue;
                    }
                };

                // Update session metadata from session.start
                if event.event_type == "session.start" {
                    if let Ok(data) = serde_json::from_value::<SessionStartData>(event.data.clone())
                    {
                        if let Some(sid) = &data.session_id {
                            meta.session_id = sid.clone();
                        }
                        if let Some(ver) = &data.copilot_version {
                            meta.version = Some(ver.clone());
                        }
                        if let Some(ctx) = &data.context {
                            if let Some(cwd) = &ctx.cwd {
                                meta.project_path = cwd.clone();
                            }
                            if let Some(br) = &ctx.branch {
                                meta.git_branch = Some(br.clone());
                            }
                            if let Some(repo) = &ctx.repository {
                                meta.repository = Some(repo.clone());
                            }
                        }
                    }
                }

                // Track model changes
                if event.event_type == "session.model_change" {
                    if let Ok(data) = serde_json::from_value::<ModelChangeData>(event.data.clone())
                    {
                        if let Some(m) = data.new_model {
                            meta.model = Some(m);
                        }
                    }
                }

                let row = Self::copilot_event_to_row(&event, &meta, &file_name, file_line);
                rows.push(row);
            }

            // Backfill session metadata to all rows from this file
            let start = rows.len().saturating_sub(file_line as usize);
            for row in &mut rows[start..] {
                if row.session_id.is_empty() {
                    row.session_id = meta.session_id.clone();
                }
            }
        }
        rows
    }

    fn copilot_event_to_row(
        event: &CopilotEvent,
        meta: &CopilotSessionMeta,
        file_name: &str,
        line_number: i64,
    ) -> ConversationRow {
        let (message_type, message_role) = Self::copilot_type_role(&event.event_type);

        let mut row = ConversationRow {
            source: "copilot".to_string(),
            session_id: meta.session_id.clone(),
            project_path: meta.project_path.clone(),
            file_name: file_name.to_string(),
            line_number,
            message_type: message_type.to_string(),
            uuid: event.id.clone(),
            parent_uuid: event.parent_id.clone(),
            timestamp: event.timestamp.clone(),
            message_role: message_role.map(String::from),
            git_branch: meta.git_branch.clone(),
            cwd: if meta.project_path.is_empty() {
                None
            } else {
                Some(meta.project_path.clone())
            },
            version: meta.version.clone(),
            model: meta.model.clone(),
            repository: meta.repository.clone(),
            ..Default::default()
        };

        // Extract type-specific fields
        match event.event_type.as_str() {
            "user.message" => {
                if let Ok(data) = serde_json::from_value::<UserMessageData>(event.data.clone()) {
                    row.message_content = data.content;
                }
            }
            "assistant.message" => {
                if let Ok(data) = serde_json::from_value::<AssistantMessageData>(event.data.clone())
                {
                    row.message_content = data.content;
                    if let Some(reqs) = &data.tool_requests {
                        if let Some(first) = reqs.first() {
                            row.tool_name = first.name.clone();
                            row.tool_use_id = first.tool_call_id.clone();
                            row.tool_input = first.arguments.as_ref().map(|a| a.to_string());
                        }
                    }
                }
            }
            "assistant.reasoning" => {
                if let Ok(data) = serde_json::from_value::<ReasoningData>(event.data.clone()) {
                    row.message_content = data.content;
                }
            }
            "tool.execution_start" => {
                if let Ok(data) =
                    serde_json::from_value::<ToolExecutionStartData>(event.data.clone())
                {
                    row.tool_name = data.tool_name;
                    row.tool_use_id = data.tool_call_id;
                    row.tool_input = data.arguments.as_ref().map(|a| a.to_string());
                }
            }
            "tool.execution_complete" => {
                if let Ok(data) =
                    serde_json::from_value::<ToolExecutionCompleteData>(event.data.clone())
                {
                    row.tool_use_id = data.tool_call_id;
                    row.message_content = data.result.and_then(|r| r.content);
                }
            }
            "session.truncation" => {
                if let Ok(data) = serde_json::from_value::<TruncationData>(event.data.clone()) {
                    row.input_tokens = data.pre_truncation_tokens;
                    row.output_tokens = data.post_truncation_tokens;
                }
            }
            "session.error" => {
                if let Ok(data) = serde_json::from_value::<SessionErrorData>(event.data.clone()) {
                    row.message_content = data.message;
                }
            }
            "session.start" => {
                if let Ok(data) = serde_json::from_value::<SessionStartData>(event.data.clone()) {
                    row.version = data.copilot_version;
                }
            }
            _ => {} // turn_start, turn_end, info, resume, abort, compaction — no extra fields
        }

        row
    }

    fn copilot_type_role(event_type: &str) -> (&'static str, Option<&'static str>) {
        match event_type {
            "user.message" => ("user", Some("user")),
            "assistant.message" => ("assistant", Some("assistant")),
            "assistant.reasoning" => ("reasoning", Some("assistant")),
            "assistant.turn_start" => ("turn_start", Some("assistant")),
            "assistant.turn_end" => ("turn_end", Some("assistant")),
            "tool.execution_start" => ("tool_start", Some("tool")),
            "tool.execution_complete" => ("tool_result", Some("tool")),
            "session.start" => ("session_start", None),
            "session.resume" => ("session_resume", None),
            "session.info" => ("session_info", None),
            "session.error" => ("session_error", None),
            "session.truncation" => ("truncation", None),
            "session.compaction_start" => ("compaction_start", None),
            "session.compaction_complete" => ("compaction_complete", None),
            "session.model_change" => ("model_change", None),
            "abort" => ("abort", None),
            _ => ("unknown", None),
        }
    }
}

// ─── Codex loading ───
//
// rollout-*.jsonl is a single ordered stream. `session_meta` (first line) and the
// latest `turn_context` are carried forward and applied to every emitted row —
// the same "session metadata backfill" technique used for Copilot above.
// `session_meta` and `turn_context` update context; cumulative token totals
// remain separate rows so they cannot be mistaken for response usage.

impl Conversations {
    fn load_codex_rows(
        path: Option<&str>,
        include_archived: bool,
        retain_raw_event: bool,
        retain_metadata: bool,
    ) -> Result<Vec<ConversationRow>, Box<dyn std::error::Error>> {
        let discovery = crate::codex_discovery::discover_codex_rollouts(path, include_archived)?;
        let index = &discovery.index;
        let discovery_diagnostics = discovery.diagnostics.clone();
        let mut rows = Vec::new();

        for discovered in &discovery.files {
            let file_start = rows.len();
            let session_uuid = &discovered.fallback_session_id;
            let file_path = &discovered.file_path;
            let file_name = file_path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            let mut meta = CodexSessionMeta::default();
            let mut turn = CodexTurnContext::default();
            let mut calls: HashMap<String, String> = HashMap::new();
            let mut item_rows: HashMap<String, usize> = HashMap::new();
            let mut canonical_messages: HashMap<(String, String), usize> = HashMap::new();
            let bytes = match std::fs::read(file_path) {
                Ok(bytes) => bytes,
                Err(err) => {
                    rows.push(Self::codex_error_row(
                        session_uuid,
                        &file_name,
                        file_path,
                        0,
                        0,
                        format!("failed to read transcript: {err}"),
                    ));
                    continue;
                }
            };
            let mut index_metadata = index.metadata_for(session_uuid);

            for (file_line, byte_offset, raw) in Self::codex_physical_lines(&bytes) {
                if raw.trim().is_empty() {
                    continue;
                }
                let parsed: CodexLine = match serde_json::from_str(&raw) {
                    Ok(p) => p,
                    Err(e) => {
                        let row = Self::codex_error_row(
                            session_uuid,
                            &file_name,
                            file_path,
                            file_line,
                            byte_offset,
                            e.to_string(),
                        );
                        rows.push(if retain_raw_event {
                            row.with_raw(&raw)
                        } else {
                            row
                        });
                        continue;
                    }
                };

                match parsed.line_type.as_str() {
                    "session_meta" => {
                        let (parsed_meta, diagnostics) =
                            CodexSessionMeta::from_value_tolerant(&parsed.payload);
                        meta = parsed_meta;
                        index_metadata =
                            index.metadata_for(meta.id.as_deref().unwrap_or(session_uuid));
                        if !diagnostics.is_empty() {
                            rows.push(Self::codex_context_row(
                                session_uuid,
                                &file_name,
                                file_path,
                                file_line,
                                byte_offset,
                                &parsed,
                                &raw,
                                &meta,
                                &turn,
                                Some(discovered.archived),
                                &index_metadata,
                                "session_meta",
                                Some(diagnostics.join("; ")),
                                retain_raw_event,
                                retain_metadata,
                            ));
                        }
                        continue;
                    }
                    "turn_context" => {
                        match serde_json::from_value::<CodexTurnContext>(parsed.payload.clone()) {
                            Ok(tc) => {
                                turn = Self::merge_turn_context(turn, tc);
                                canonical_messages.clear();
                            },
                            Err(e) => rows.push(Self::codex_context_row(
                                session_uuid,
                                &file_name,
                                file_path,
                                file_line,
                                byte_offset,
                                &parsed,
                                &raw,
                                &meta,
                                &turn,
                                Some(discovered.archived),
                                &index_metadata,
                                "turn_context",
                                Some(e.to_string()),
                                retain_raw_event,
                                retain_metadata,
                            )),
                        };
                        continue;
                    }
                    _ => {}
                }

                // Desktop records completed UI items inside an event message.  They are
                // the only copy for many Desktop sessions, while other sessions also
                // have a canonical response_item for the same native item id.
                if parsed.line_type == "event_msg"
                    && Self::json_string(&parsed.payload, &["type"]).as_deref()
                        == Some("item_completed")
                {
                    let Some(item) = parsed.payload.get("item") else {
                        rows.push(Self::codex_context_row(
                            session_uuid,
                            &file_name,
                            file_path,
                            file_line,
                            byte_offset,
                            &parsed,
                            &raw,
                            &meta,
                            &turn,
                            Some(discovered.archived),
                            &index_metadata,
                            "item_completed",
                            Some("item_completed event has no item".to_string()),
                            retain_raw_event,
                            retain_metadata,
                        ));
                        continue;
                    };
                    let item_id = Self::codex_item_id(item);
                    let call_id = Self::codex_call_id(item);
                    let existing = item_id
                        .as_deref()
                        .and_then(|id| item_rows.get(id).copied())
                        .or_else(|| call_id.as_deref().and_then(|id| item_rows.get(id).copied()));
                    if let Some(existing) = existing {
                        Self::enrich_codex_completed_item(
                            &mut rows[existing],
                            &parsed.payload,
                            item,
                        );
                        continue;
                    }
                    match Self::codex_desktop_completed_item_row(
                        &parsed,
                        &raw,
                        session_uuid,
                        &file_name,
                        file_path,
                        file_line,
                        byte_offset,
                        &meta,
                        &turn,
                        Some(discovered.archived),
                        &index_metadata,
                        retain_raw_event,
                        retain_metadata,
                    ) {
                        Some(row) => {
                            if matches!(
                                row.message_type.as_str(),
                                "mcp_tool_call"
                                    | "dynamic_tool_call"
                                    | "command_execution"
                                    | "collab_agent_tool_call"
                            ) {
                                if let (Some(call_id), Some(name)) =
                                    (row.tool_use_id.clone(), row.tool_name.clone())
                                {
                                    calls.insert(call_id, name);
                                }
                            }
                            if let Some(id) = row.uuid.clone() {
                                item_rows.insert(id, rows.len());
                            }
                            if let Some(call_id) = row.tool_use_id.clone() {
                                item_rows.insert(call_id, rows.len());
                            }
                            rows.push(row);
                        }
                        None => rows.push(Self::codex_context_row(
                            session_uuid,
                            &file_name,
                            file_path,
                            file_line,
                            byte_offset,
                            &parsed,
                            &raw,
                            &meta,
                            &turn,
                            Some(discovered.archived),
                            &index_metadata,
                            "item_completed",
                            Some("item_completed event has an unrecognizable item".to_string()),
                            retain_raw_event,
                            retain_metadata,
                        )),
                    }
                    continue;
                }

                if parsed.line_type == "item_completed" {
                    let item_id = Self::codex_item_id(&parsed.payload);
                    let call_id = Self::codex_call_id(&parsed.payload);
                    let existing = item_id
                        .as_deref()
                        .and_then(|id| item_rows.get(id).copied())
                        .or_else(|| call_id.as_deref().and_then(|id| item_rows.get(id).copied()));
                    if let Some(existing) = existing {
                        let row = &mut rows[existing];
                        row.status = Self::json_string(&parsed.payload, &["status"]);
                        row.stop_reason = Self::json_string(
                            &parsed.payload,
                            &["stop_reason", "completion_reason"],
                        );
                        if row.stop_reason.is_none() && row.status.as_deref() == Some("interrupted")
                        {
                            row.stop_reason = row.status.clone();
                        }
                        continue;
                    }
                }

                if matches!(
                    parsed.line_type.as_str(),
                    "response.completed" | "response_completed"
                ) {
                    if Self::apply_codex_terminal(&mut rows, &parsed) {
                        continue;
                    }
                }

                if parsed.line_type == "event_msg"
                    && matches!(
                        Self::json_string(&parsed.payload, &["type"]).as_deref(),
                        Some("turn_aborted" | "task_complete" | "task_completed")
                    )
                {
                    Self::apply_codex_terminal(&mut rows, &parsed);
                }

                if parsed.line_type == "token_usage_record" {
                    if !Self::apply_codex_usage(&mut rows, &parsed, file_line) {
                        rows.push(Self::codex_usage_row(
                            session_uuid,
                            &file_name,
                            file_path,
                            file_line,
                            byte_offset,
                            &parsed,
                            &raw,
                            &meta,
                            &turn,
                            Some(discovered.archived),
                            &index_metadata,
                            retain_raw_event,
                            retain_metadata,
                        ));
                    }
                    continue;
                }

                if parsed.line_type == "event_msg"
                    && Self::json_string(&parsed.payload, &["type"]).as_deref()
                        == Some("token_count")
                {
                    let applied = Self::apply_codex_legacy_usage(&mut rows, &parsed, file_line);
                    let total = parsed
                        .payload
                        .get("total_token_usage")
                        .or_else(|| parsed.payload.pointer("/info/total_token_usage"))
                        .or_else(|| parsed.payload.get("total_tokens"))
                        .or_else(|| parsed.payload.pointer("/info/total_tokens"));
                    if let Some(total) = total {
                        let mut row = Self::codex_context_row(
                            session_uuid,
                            &file_name,
                            file_path,
                            file_line,
                            byte_offset,
                            &parsed,
                            &raw,
                            &meta,
                            &turn,
                            Some(discovered.archived),
                            &index_metadata,
                            "token_count",
                            None,
                            retain_raw_event,
                            retain_metadata,
                        );
                        row.message_type = "token_usage_total".to_string();
                        row.usage_scope = Some("thread_total".to_string());
                        row.usage_source_line = Some(file_line);
                        let (input, output, cache_creation, cache_read, reasoning) =
                            Self::usage_fields(total);
                        row.input_tokens = input;
                        row.output_tokens = output;
                        row.cache_creation_tokens = cache_creation;
                        row.cache_read_tokens = cache_read;
                        row.reasoning_tokens = reasoning;
                        if retain_metadata {
                            row.metadata = Some(serde_json::json!({"session": meta, "turn": turn, "item": &parsed.payload, "index": index_metadata}).to_string());
                        }
                        rows.push(row);
                    } else if !applied {
                        rows.push(Self::codex_context_row(
                            session_uuid,
                            &file_name,
                            file_path,
                            file_line,
                            byte_offset,
                            &parsed,
                            &raw,
                            &meta,
                            &turn,
                            Some(discovered.archived),
                            &index_metadata,
                            "token_count",
                            None,
                            retain_raw_event,
                            retain_metadata,
                        ));
                    }
                    continue;
                }

                if let Some(mut row) = Self::codex_line_to_row(
                    &parsed,
                    &raw,
                    session_uuid,
                    &file_name,
                    file_path,
                    file_line,
                    byte_offset,
                    &meta,
                    &turn,
                    Some(discovered.archived),
                    &index_metadata,
                    retain_raw_event,
                    retain_metadata,
                ) {
                    if matches!(
                        row.message_type.as_str(),
                        "function_call" | "custom_tool_call"
                    ) {
                        if let (Some(call_id), Some(name)) =
                            (row.tool_use_id.clone(), row.tool_name.clone())
                        {
                            calls.insert(call_id, name);
                        }
                    }
                    if matches!(
                        row.message_type.as_str(),
                        "function_call_output" | "custom_tool_call_output"
                    ) && row.tool_name.is_none()
                    {
                        row.tool_name = row
                            .tool_use_id
                            .as_ref()
                            .and_then(|id| calls.get(id).cloned());
                        if row.status.is_none() {
                            row.status = Some(if row.message_content.is_some() {
                                "completed".to_string()
                            } else {
                                "missing_output".to_string()
                            });
                        }
                    }
                    if parsed.line_type == "event_msg"
                        && matches!(row.message_type.as_str(), "user" | "assistant")
                    {
                        let key = (
                            row.message_role.clone().unwrap_or_default(),
                            row.message_content.clone().unwrap_or_default(),
                        );
                        if let Some(count) = canonical_messages.get_mut(&key) {
                            if *count > 0 {
                                *count -= 1;
                                continue;
                            }
                        }
                    }
                    if parsed.line_type == "response_item"
                        && matches!(
                            row.message_type.as_str(),
                            "user" | "assistant" | "developer" | "agent_message"
                        )
                    {
                        Self::suppress_prior_codex_event_copy(&mut rows, &mut row);
                        *canonical_messages
                            .entry((
                                row.message_role.clone().unwrap_or_default(),
                                row.message_content.clone().unwrap_or_default(),
                            ))
                            .or_default() += 1;
                    }
                    if let Some(id) = row.uuid.clone() {
                        item_rows.insert(id, rows.len());
                    }
                    if let Some(call_id) = row.tool_use_id.clone() {
                        item_rows.insert(call_id, rows.len());
                    }
                    rows.push(row);
                }
            }
            let file_rows = rows.split_off(file_start);
            rows.extend(file_rows.into_iter().filter(|row| !row.suppress_output));
            if let Some(first) = rows.get_mut(file_start) {
                if !discovery_diagnostics.is_empty() {
                    let diagnostics = discovery_diagnostics.join("; ");
                    first.parse_error = Some(match first.parse_error.take() {
                        Some(existing) => format!("{existing}; {diagnostics}"),
                        None => diagnostics,
                    });
                    if retain_metadata {
                        if let Ok(mut metadata) = serde_json::from_str::<serde_json::Value>(
                            first.metadata.as_deref().unwrap_or("{}"),
                        ) {
                            metadata["diagnostics"] = serde_json::json!(discovery_diagnostics);
                            first.metadata = Some(metadata.to_string());
                        }
                    }
                }
            }
        }
        Ok(rows)
    }

    fn codex_base_row(
        session_uuid: &str,
        file_name: &str,
        file_path: &Path,
        line_number: i64,
        byte_offset: i64,
        timestamp: Option<String>,
        meta: &CodexSessionMeta,
        turn: &CodexTurnContext,
        archived: Option<bool>,
        index_metadata: &serde_json::Value,
        event_type: &str,
        raw_event: &str,
        retain_raw_event: bool,
        retain_metadata: bool,
    ) -> ConversationRow {
        let git = meta.git.as_ref();
        let session_id = meta
            .id
            .clone()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| session_uuid.to_string());
        let source = meta.source.as_ref();
        let spawn = source.and_then(|s| s.pointer("/subagent/thread_spawn"));
        let structured_originator = source
            .and_then(|s| s.get("originator"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let structured_thread_source = source
            .and_then(|s| s.get("thread_source"))
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| source.and_then(|s| s.as_str()).map(String::from));
        let structured_client = source
            .and_then(|s| s.get("client"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let agent_path = meta
            .agent_path
            .clone()
            .or_else(|| {
                spawn
                    .and_then(|s| s.get("agent_path"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .or_else(|| Self::json_string(index_metadata, &["agent_path"]));
        let agent_nickname = meta
            .agent_nickname
            .clone()
            .or_else(|| {
                spawn
                    .and_then(|s| s.get("agent_nickname"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .or_else(|| Self::json_string(index_metadata, &["agent_nickname"]));
        let agent_role = meta
            .agent_role
            .clone()
            .or_else(|| {
                spawn
                    .and_then(|s| s.get("agent_role"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .or_else(|| Self::json_string(index_metadata, &["agent_role"]));
        let is_agent = agent_path.is_some()
            || spawn.is_some()
            || structured_thread_source.as_deref() == Some("subagent")
            || meta.thread_source.as_deref() == Some("subagent");
        ConversationRow {
            source: "codex".to_string(),
            session_id,
            project_path: meta.cwd.clone().unwrap_or_default(),
            file_name: file_name.to_string(),
            line_number,
            timestamp,
            cwd: turn.cwd.clone().or_else(|| meta.cwd.clone()),
            git_branch: git.and_then(|g| g.branch.clone()),
            repository: git.and_then(|g| g.repository_url.clone()),
            version: meta.cli_version.clone(),
            model: turn.model.clone(),
            reasoning_effort: turn
                .reasoning_effort
                .clone()
                .or_else(|| turn.effort.clone()),
            file_path: Some(file_path.to_string_lossy().into_owned()),
            byte_offset: Some(byte_offset),
            ordinal: Some(line_number - 1),
            event_type: Some(event_type.to_string()),
            client: structured_client
                .map(|client| Self::normalize_codex_client(&client))
                .or_else(|| Self::codex_client(meta)),
            originator: structured_originator.or_else(|| meta.originator.clone()),
            thread_source: structured_thread_source
                .or_else(|| meta.thread_source.clone())
                .or_else(|| Self::json_string(index_metadata, &["thread_source"])),
            forked_from_session_id: meta.forked_from_id.clone().or_else(|| {
                source.and_then(|s| {
                    Self::json_string(s, &["forked_from_session_id", "forked_from_id"])
                })
            }),
            turn_id: turn.turn_id.clone(),
            root_turn_id: turn.root_turn_id.clone(),
            response_id: turn.response_id.clone(),
            model_provider: if index_metadata.get("_sqlite_path").is_some() {
                Self::json_string(index_metadata, &["model_provider", "provider"])
                    .or_else(|| meta.model_provider.clone())
            } else {
                meta.model_provider
                    .clone()
                    .or_else(|| Self::json_string(index_metadata, &["model_provider", "provider"]))
            },
            git_commit: git.and_then(|g| g.commit_hash.clone()),
            agent_path: agent_path.clone(),
            agent_nickname,
            agent_role,
            slug: Self::json_string(index_metadata, &["title", "thread_name"])
                .or_else(|| meta.title.clone()),
            parent_session_id: meta
                .parent_thread_id
                .clone()
                .or_else(|| {
                    spawn
                        .and_then(|s| s.get("parent_thread_id"))
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
                .or_else(|| {
                    Self::json_string(index_metadata, &["parent_session_id", "parent_thread_id"])
                }),
            is_agent,
            session_created_at: meta
                .timestamp
                .clone()
                .or_else(|| Self::json_string(index_metadata, &["created_at", "createdAt"])),
            session_updated_at: Self::json_string(index_metadata, &["updated_at", "updatedAt"]),
            archived: Self::json_bool(index_metadata, &["archived", "is_archived"]).or(archived),
            raw_event: retain_raw_event.then(|| raw_event.to_string()),
            metadata: retain_metadata.then(|| {
                serde_json::json!({"session": meta, "turn": turn, "index": index_metadata})
                    .to_string()
            }),
            ..Default::default()
        }
    }

    fn codex_line_to_row(
        parsed: &CodexLine,
        raw_event: &str,
        session_uuid: &str,
        file_name: &str,
        file_path: &Path,
        line_number: i64,
        byte_offset: i64,
        meta: &CodexSessionMeta,
        turn: &CodexTurnContext,
        archived: Option<bool>,
        index_metadata: &serde_json::Value,
        retain_raw_event: bool,
        retain_metadata: bool,
    ) -> Option<ConversationRow> {
        let base = Self::codex_base_row(
            session_uuid,
            file_name,
            file_path,
            line_number,
            byte_offset,
            parsed.timestamp.clone(),
            meta,
            turn,
            archived,
            index_metadata,
            &parsed.line_type,
            raw_event,
            retain_raw_event,
            retain_metadata,
        );

        match parsed.line_type.as_str() {
            "response_item" => {
                let item: CodexResponseItem =
                    serde_json::from_value(parsed.payload.clone()).ok()?;
                let item_id = item.id.clone().or(item.item_id.clone());
                let parent_id = item.parent_id.clone().or(item.parent_item_id.clone());
                let with_item = |mut row: ConversationRow| {
                    row.uuid = item_id.clone();
                    row.parent_uuid = parent_id.clone();
                    row.response_id = item.response_id.clone().or_else(|| row.response_id.clone());
                    row.turn_id = Self::codex_turn_id(&parsed.payload).or(row.turn_id);
                    row.status = item.status.clone();
                    row.channel = item.channel.clone();
                    if matches!(
                        row.message_type.as_str(),
                        "assistant" | "agent_message" | "reasoning"
                    ) {
                        if let Some(usage) = item.usage.as_ref() {
                            let (input, output, cache_creation, cache_read, reasoning) =
                                Self::usage_fields(usage);
                            row.input_tokens = input;
                            row.output_tokens = output;
                            row.cache_creation_tokens = cache_creation;
                            row.cache_read_tokens = cache_read;
                            row.reasoning_tokens = reasoning;
                            row.usage_scope = Some("response".to_string());
                            row.usage_source_line = Some(line_number);
                        }
                    }
                    row.record_id = Some(Self::codex_record_id(
                        &row.session_id,
                        row.uuid.as_deref(),
                        &parsed.line_type,
                        &parsed.payload,
                        line_number,
                    ));
                    row.metadata = retain_metadata.then(|| serde_json::json!({"session": meta, "turn": turn, "item": &parsed.payload, "index": index_metadata}).to_string());
                    row
                };
                match item.item_type.as_deref() {
                    Some("message") => {
                        // Normalize to a role-specific type (user/assistant/...)
                        // like the other providers, so cross-source filters on
                        // message_type work; fall back to the raw role.
                        let role = item.role.clone();
                        let message_type = match role.as_deref() {
                            Some("user") => "user".to_string(),
                            Some("assistant") => "assistant".to_string(),
                            Some(other) => other.to_string(),
                            None => "message".to_string(),
                        };
                        Some(with_item(ConversationRow {
                            message_type,
                            message_role: role,
                            message_content: item.content.as_ref().map(utils::extract_text_content),
                            ..base
                        }))
                    }
                    Some("reasoning") => Some(with_item(ConversationRow {
                        message_type: "reasoning".to_string(),
                        message_role: item.role.clone().or(Some("assistant".to_string())),
                        message_content: item.summary.as_ref().and_then(Self::codex_reasoning_text),
                        ..base
                    })),
                    Some("function_call") => Some(with_item(ConversationRow {
                        message_type: "function_call".to_string(),
                        message_role: Some("tool".to_string()),
                        tool_name: item.name.clone(),
                        tool_use_id: item.call_id.clone(),
                        tool_input: item.arguments.as_ref().map(Self::codex_tool_input_text),
                        ..base
                    })),
                    Some("function_call_output") => Some(with_item(ConversationRow {
                        message_type: "function_call_output".to_string(),
                        message_role: Some("tool".to_string()),
                        tool_use_id: item.call_id.clone(),
                        message_content: item.output.as_ref().map(Self::codex_output_text),
                        ..base
                    })),
                    // Codex's newer tool protocol. Same shape as function_call
                    // under a different name, but the arguments arrive in
                    // `input` rather than `arguments`, as either a raw string
                    // (e.g. an apply_patch body) or a JSON object.
                    Some("custom_tool_call") => Some(with_item(ConversationRow {
                        message_type: "custom_tool_call".to_string(),
                        message_role: Some("tool".to_string()),
                        tool_name: item.name.clone(),
                        tool_use_id: item.call_id.clone(),
                        tool_input: item.input.as_ref().map(Self::codex_tool_input_text),
                        ..base
                    })),
                    // The output can be a plain string (like function_call_output)
                    // or an MCP-style `{content: [{type: "text", text: ...}]}`
                    // wrapper; unwrap the latter before extracting text.
                    Some("custom_tool_call_output") => Some(with_item(ConversationRow {
                        message_type: "custom_tool_call_output".to_string(),
                        message_role: Some("tool".to_string()),
                        tool_use_id: item.call_id.clone(),
                        message_content: item.output.as_ref().map(Self::codex_output_text),
                        ..base
                    })),
                    // A message from/between sub-agents. Distinct from the
                    // event_msg agent_message the loader already handles as a
                    // user/assistant fallback: this one is a real turn with
                    // its own content blocks and must not be dropped.
                    Some("agent_message") => Some(with_item(ConversationRow {
                        message_type: "agent_message".to_string(),
                        message_role: item.role.clone().or(Some("assistant".to_string())),
                        message_content: item.content.as_ref().map(utils::extract_text_content),
                        ..base
                    })),
                    Some(other) => Some(with_item(ConversationRow {
                        message_type: other.to_string(),
                        ..base
                    })),
                    None => None,
                }
            }
            "event_msg" => {
                let ev: CodexEventMsg = serde_json::from_value(parsed.payload.clone()).ok()?;
                let mut row = match ev.event_type.as_deref() {
                    // event_msg user/agent text duplicates the response_item
                    // message rows above. The loader buffers these and only
                    // emits them for sessions with no response_item/message
                    // rows, so canonical turns are never double-counted.
                    // Other events retain their recorded type and content.
                    Some("user_message") => Some(ConversationRow {
                        message_type: "user".to_string(),
                        message_role: Some("user".to_string()),
                        message_content: ev.message.clone(),
                        ..base
                    }),
                    Some("agent_message") => Some(ConversationRow {
                        message_type: "assistant".to_string(),
                        message_role: Some("assistant".to_string()),
                        message_content: ev.message.clone(),
                        ..base
                    }),
                    Some(event) => Some(ConversationRow {
                        message_type: event.to_string(),
                        message_content: ev.message.clone().or(ev.last_agent_message.clone()),
                        turn_id: Self::codex_turn_id(&parsed.payload).or(base.turn_id.clone()),
                        status: Self::codex_terminal_status(&parsed.payload, event)
                            .or_else(|| Self::json_string(&parsed.payload, &["status", "phase"])),
                        stop_reason: Self::codex_terminal_reason(&parsed.payload, event),
                        ..base
                    }),
                    None => None,
                }?;
                row.record_id = Some(Self::codex_record_id(
                    &row.session_id,
                    None,
                    &parsed.line_type,
                    &parsed.payload,
                    line_number,
                ));
                Some(row)
            }
            // These variants carry valuable source context even where Codex has
            // not assigned them a response-item identity yet.
            other => Some(ConversationRow {
                message_type: other.to_string(),
                message_content: Self::json_string(
                    &parsed.payload,
                    &["message", "summary", "text"],
                ),
                uuid: Self::json_string(&parsed.payload, &["id", "item_id"]),
                parent_uuid: Self::json_string(&parsed.payload, &["parent_id", "parent_item_id"]),
                response_id: Self::json_string(&parsed.payload, &["response_id"])
                    .or_else(|| base.response_id.clone()),
                turn_id: Self::codex_turn_id(&parsed.payload).or_else(|| base.turn_id.clone()),
                root_turn_id: Self::json_string(&parsed.payload, &["root_turn_id"])
                    .or_else(|| base.root_turn_id.clone()),
                status: Self::json_string(&parsed.payload, &["status", "outcome"]),
                record_id: Some(Self::codex_record_id(
                    &base.session_id,
                    None,
                    other,
                    &parsed.payload,
                    line_number,
                )),
                ..base
            }),
        }
    }

    fn codex_desktop_completed_item_row(
        parsed: &CodexLine,
        raw_event: &str,
        session_uuid: &str,
        file_name: &str,
        file_path: &Path,
        line_number: i64,
        byte_offset: i64,
        meta: &CodexSessionMeta,
        turn: &CodexTurnContext,
        archived: Option<bool>,
        index_metadata: &serde_json::Value,
        retain_raw_event: bool,
        retain_metadata: bool,
    ) -> Option<ConversationRow> {
        let item = parsed.payload.get("item")?;
        let item_type = Self::json_string(item, &["type", "item_type"])?;
        let item_id = Self::codex_item_id(item);
        let parent_id = Self::json_string(item, &["parent_id", "parent_item_id"]);
        let mut base = Self::codex_base_row(
            session_uuid,
            file_name,
            file_path,
            line_number,
            byte_offset,
            parsed.timestamp.clone(),
            meta,
            turn,
            archived,
            index_metadata,
            &parsed.line_type,
            raw_event,
            retain_raw_event,
            retain_metadata,
        );
        base.uuid = item_id.clone();
        base.parent_uuid = parent_id;
        base.response_id = Self::json_string(item, &["response_id"]).or(base.response_id);
        base.turn_id = Self::codex_turn_id(item)
            .or_else(|| Self::codex_turn_id(&parsed.payload))
            .or(base.turn_id);
        base.root_turn_id = Self::json_string(item, &["root_turn_id"]).or(base.root_turn_id);
        base.channel = Self::json_string(item, &["channel"])
            .or_else(|| Self::json_string(&parsed.payload, &["channel"]));
        base.status = Self::json_string(item, &["status", "phase"])
            .or_else(|| Self::json_string(&parsed.payload, &["status", "phase"]));
        base.record_id = Some(Self::codex_record_id(
            &base.session_id,
            item_id.as_deref(),
            &parsed.line_type,
            &parsed.payload,
            line_number,
        ));
        if retain_metadata {
            base.metadata = Some(serde_json::json!({"session": meta, "turn": turn, "item": item, "event": &parsed.payload, "index": index_metadata}).to_string());
        }

        let text = Self::codex_desktop_item_text(item);
        let mut row = match item_type.as_str() {
            "UserMessage" => ConversationRow {
                message_type: "user".to_string(),
                message_role: Some("user".to_string()),
                message_content: text,
                ..base
            },
            "AgentMessage" => ConversationRow {
                message_type: "agent_message".to_string(),
                message_role: Some("assistant".to_string()),
                message_content: text,
                ..base
            },
            "Reasoning" => ConversationRow {
                message_type: "reasoning".to_string(),
                message_role: Some("assistant".to_string()),
                message_content: Self::codex_reasoning_text(item),
                ..base
            },
            "McpToolCall" => ConversationRow {
                message_type: "mcp_tool_call".to_string(),
                message_role: Some("tool".to_string()),
                tool_name: Self::json_string(item, &["tool"]),
                tool_use_id: Self::json_string(item, &["call_id", "id", "item_id"]),
                tool_input: item.get("arguments").map(Self::codex_tool_input_text),
                message_content: item
                    .get("result")
                    .or_else(|| item.get("error"))
                    .map(Self::codex_output_text),
                ..base
            },
            "DynamicToolCall" => ConversationRow {
                message_type: "dynamic_tool_call".to_string(),
                message_role: Some("tool".to_string()),
                tool_name: Self::json_string(item, &["tool"]),
                tool_use_id: Self::json_string(item, &["call_id", "id", "item_id"]),
                tool_input: item.get("arguments").map(Self::codex_tool_input_text),
                message_content: item
                    .get("content_items")
                    .or_else(|| item.get("error"))
                    .map(Self::codex_output_text),
                ..base
            },
            "CommandExecution" => ConversationRow {
                message_type: "command_execution".to_string(),
                message_role: Some("tool".to_string()),
                tool_name: Some("command".to_string()),
                tool_use_id: Self::json_string(item, &["call_id", "id", "item_id"]),
                tool_input: item.get("command").map(Self::codex_tool_input_text),
                message_content: text,
                ..base
            },
            "CollabAgentToolCall" => ConversationRow {
                message_type: "collab_agent_tool_call".to_string(),
                message_role: Some("tool".to_string()),
                tool_name: Some("collab_agent".to_string()),
                tool_use_id: Self::json_string(item, &["call_id", "id", "item_id"]),
                message_content: text,
                ..base
            },
            "ContextCompaction" => ConversationRow {
                message_type: "compaction_summary".to_string(),
                message_content: text,
                ..base
            },
            "FileChange" => ConversationRow {
                message_type: "file_change".to_string(),
                message_content: text,
                ..base
            },
            "ImageView" => ConversationRow {
                message_type: "image_view".to_string(),
                message_content: text,
                ..base
            },
            "Plan" => ConversationRow {
                message_type: "plan".to_string(),
                message_content: text,
                ..base
            },
            "SubAgentActivity" => ConversationRow {
                message_type: "subagent_activity".to_string(),
                message_content: text,
                ..base
            },
            "Extension" => ConversationRow {
                message_type: "extension".to_string(),
                message_content: text,
                ..base
            },
            other => ConversationRow {
                message_type: other.to_string(),
                message_content: text,
                ..base
            },
        };
        if row.status.is_none()
            && matches!(
                row.message_type.as_str(),
                "mcp_tool_call"
                    | "dynamic_tool_call"
                    | "command_execution"
                    | "collab_agent_tool_call"
            )
        {
            row.status = Some(if row.message_content.is_some() {
                "completed".to_string()
            } else {
                "missing_output".to_string()
            });
        }
        Some(row)
    }

    fn codex_desktop_item_text(item: &serde_json::Value) -> Option<String> {
        [
            "content",
            "summary_text",
            "text",
            "formatted_output",
            "aggregated_output",
            "result",
            "error",
            "stdout",
            "stderr",
            "raw_content",
        ]
        .iter()
        .find_map(|key| item.get(*key))
        .map(Self::codex_output_text)
    }

    fn codex_reasoning_text(value: &serde_json::Value) -> Option<String> {
        value
            .get("summary")
            .or_else(|| value.get("content"))
            .map(utils::extract_text_content)
            .filter(|text| !text.is_empty())
    }

    fn enrich_codex_completed_item(
        row: &mut ConversationRow,
        event: &serde_json::Value,
        item: &serde_json::Value,
    ) {
        row.status = Self::json_string(item, &["status", "phase"])
            .or_else(|| Self::json_string(event, &["status", "phase"]))
            .or(row.status.take());
        if row.channel.is_none() {
            row.channel = Self::json_string(item, &["channel"])
                .or_else(|| Self::json_string(event, &["channel"]));
        }
        if row.message_content.is_none() {
            row.message_content = Self::codex_desktop_item_text(item);
        }
        if row.tool_name.is_none() {
            row.tool_name = Self::json_string(item, &["tool", "name"]);
        }
        if row.tool_use_id.is_none() {
            row.tool_use_id = Self::codex_call_id(item).or_else(|| Self::codex_item_id(item));
        }
        if row.tool_input.is_none() {
            row.tool_input = item
                .get("arguments")
                .or_else(|| item.get("input"))
                .or_else(|| item.get("command"))
                .map(Self::codex_tool_input_text);
        }
    }

    /// `custom_tool_call.input` is either a raw string (kept unquoted) or a
    /// JSON object/array (kept as its literal JSON text) — unlike
    /// `function_call.arguments`, which is always a JSON-encoded string.
    fn codex_tool_input_text(value: &serde_json::Value) -> String {
        match value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }

    /// `custom_tool_call_output.output` is either a plain string/array (same
    /// shape `extract_text_content` already handles) or an MCP-style
    /// `{content: [...]}` wrapper; unwrap the wrapper before extracting text.
    fn codex_output_text(value: &serde_json::Value) -> String {
        match value.get("content") {
            Some(content) => utils::extract_text_content(content),
            None => utils::extract_text_content(value),
        }
    }

    fn codex_physical_lines(bytes: &[u8]) -> Vec<(i64, i64, String)> {
        let mut lines = Vec::new();
        let mut offset = 0usize;
        let mut number = 0i64;
        while offset < bytes.len() {
            let end = bytes[offset..]
                .iter()
                .position(|b| *b == b'\n')
                .map(|i| offset + i)
                .unwrap_or(bytes.len());
            let content_end = if end > offset && bytes[end - 1] == b'\r' {
                end - 1
            } else {
                end
            };
            number += 1;
            lines.push((
                number,
                offset as i64,
                String::from_utf8_lossy(&bytes[offset..content_end]).into_owned(),
            ));
            offset = if end == bytes.len() {
                bytes.len()
            } else {
                end + 1
            };
        }
        lines
    }

    fn suppress_prior_codex_event_copy(
        rows: &mut [ConversationRow],
        canonical: &mut ConversationRow,
    ) {
        let Some(turn_id) = canonical.turn_id.as_deref() else {
            return;
        };
        let canonical_uuid = canonical.uuid.as_deref();
        let role = canonical.message_role.as_deref();
        let content = canonical.message_content.as_deref();
        if let Some(event) = rows.iter_mut().rev().find(|row| {
            !row.suppress_output
                && row.event_type.as_deref() == Some("event_msg")
                && matches!(
                    row.message_type.as_str(),
                    "user" | "assistant" | "agent_message"
                )
                && row.turn_id.as_deref() == Some(turn_id)
                && match (canonical_uuid, row.uuid.as_deref()) {
                    (Some(canonical_uuid), Some(event_uuid)) => canonical_uuid == event_uuid,
                    _ => match (role, content) {
                        (Some(role), Some(content)) => {
                            row.message_role.as_deref() == Some(role)
                                && row.message_content.as_deref() == Some(content)
                        }
                        _ => false,
                    },
                }
        }) {
            canonical.parent_uuid = canonical.parent_uuid.clone().or(event.parent_uuid.clone());
            canonical.response_id = canonical.response_id.clone().or(event.response_id.clone());
            canonical.root_turn_id = canonical
                .root_turn_id
                .clone()
                .or(event.root_turn_id.clone());
            canonical.channel = canonical.channel.clone().or(event.channel.clone());
            canonical.status = canonical.status.clone().or(event.status.clone());
            canonical.stop_reason = canonical.stop_reason.clone().or(event.stop_reason.clone());
            canonical.message_content = canonical
                .message_content
                .clone()
                .or(event.message_content.clone());
            event.suppress_output = true;
        }
    }

    fn merge_turn_context(mut old: CodexTurnContext, next: CodexTurnContext) -> CodexTurnContext {
        macro_rules! merge {
            ($field:ident) => {
                if next.$field.is_some() {
                    old.$field = next.$field;
                }
            };
        }
        merge!(model);
        merge!(cwd);
        merge!(turn_id);
        merge!(root_turn_id);
        merge!(response_id);
        merge!(reasoning_effort);
        merge!(effort);
        merge!(approval_policy);
        merge!(sandbox_policy);
        merge!(runtime_workspace_roots);
        old
    }

    fn codex_client(meta: &CodexSessionMeta) -> Option<String> {
        Some(Self::normalize_codex_client(meta.originator.as_deref()?))
    }

    fn normalize_codex_client(raw: &str) -> String {
        let raw = raw.to_ascii_lowercase();
        if raw.contains("desktop") {
            Some("desktop".to_string())
        } else if raw.contains("exec") {
            Some("exec".to_string())
        } else if raw.contains("editor") {
            Some("editor".to_string())
        } else if raw.contains("browser") {
            Some("browser".to_string())
        } else if raw.contains("cli") || raw.contains("tui") || raw.contains("codex") {
            Some("cli".to_string())
        } else {
            Some(raw)
        }
        .unwrap()
    }

    fn json_string(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
        keys.iter()
            .find_map(|key| value.get(*key))
            .and_then(|value| match value {
                serde_json::Value::String(s) if !s.is_empty() => Some(s.clone()),
                serde_json::Value::Number(n) => Some(n.to_string()),
                _ => None,
            })
    }

    fn codex_turn_id(value: &serde_json::Value) -> Option<String> {
        Self::json_string(value, &["turn_id"]).or_else(|| {
            value
                .pointer("/internal_chat_message_metadata_passthrough/turn_id")
                .and_then(|turn_id| turn_id.as_str())
                .filter(|turn_id| !turn_id.is_empty())
                .map(str::to_string)
        })
    }

    fn codex_item_id(value: &serde_json::Value) -> Option<String> {
        Self::json_string(value, &["item_id", "id"]).or_else(|| {
            value
                .get("item")
                .and_then(|item| Self::json_string(item, &["item_id", "id"]))
        })
    }

    fn codex_call_id(value: &serde_json::Value) -> Option<String> {
        Self::json_string(value, &["call_id"]).or_else(|| {
            value
                .get("item")
                .and_then(|item| Self::json_string(item, &["call_id"]))
        })
    }

    fn json_bool(value: &serde_json::Value, keys: &[&str]) -> Option<bool> {
        keys.iter()
            .find_map(|key| value.get(*key))
            .and_then(serde_json::Value::as_bool)
    }

    fn codex_record_id(
        session_id: &str,
        native_id: Option<&str>,
        event_type: &str,
        value: &serde_json::Value,
        ordinal: i64,
    ) -> String {
        // FNV-1a is deterministic without a new build dependency. Native item
        // IDs lead when available, so moving a transcript does not change IDs.
        let key = native_id
            .map(String::from)
            .unwrap_or_else(|| serde_json::to_string(value).unwrap_or_default());
        let mut hash = 0xcbf29ce484222325u64;
        for byte in format!("{session_id}\u{1f}{event_type}\u{1f}{key}\u{1f}{ordinal}").bytes() {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        format!("codex:{hash:016x}")
    }

    fn codex_error_row(
        session_id: &str,
        file_name: &str,
        file_path: &Path,
        line_number: i64,
        byte_offset: i64,
        error: String,
    ) -> ConversationRow {
        ConversationRow {
            source: "codex".to_string(),
            session_id: session_id.to_string(),
            file_name: file_name.to_string(),
            line_number,
            message_type: "_parse_error".to_string(),
            file_path: Some(file_path.to_string_lossy().into_owned()),
            byte_offset: Some(byte_offset),
            ordinal: Some(line_number.saturating_sub(1)),
            parse_error: Some(error),
            ..Default::default()
        }
    }

    fn codex_context_row(
        session_id: &str,
        file_name: &str,
        file_path: &Path,
        line_number: i64,
        byte_offset: i64,
        parsed: &CodexLine,
        raw_event: &str,
        meta: &CodexSessionMeta,
        turn: &CodexTurnContext,
        archived: Option<bool>,
        index_metadata: &serde_json::Value,
        event_type: &str,
        error: Option<String>,
        retain_raw_event: bool,
        retain_metadata: bool,
    ) -> ConversationRow {
        let mut row = Self::codex_base_row(
            session_id,
            file_name,
            file_path,
            line_number,
            byte_offset,
            parsed.timestamp.clone(),
            meta,
            turn,
            archived,
            index_metadata,
            event_type,
            raw_event,
            retain_raw_event,
            retain_metadata,
        );
        row.message_type = event_type.to_string();
        row.parse_error = error;
        row.record_id = Some(Self::codex_record_id(
            &row.session_id,
            None,
            event_type,
            &parsed.payload,
            line_number,
        ));
        row
    }

    fn usage_fields(
        value: &serde_json::Value,
    ) -> (
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    ) {
        let n = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| value.get(*name).and_then(|v| v.as_i64()))
        };
        (
            n(&["input_tokens"]),
            n(&["output_tokens"]),
            n(&["cache_write_input_tokens", "cache_creation_input_tokens"]),
            n(&["cached_input_tokens", "cache_read_input_tokens"]),
            n(&["reasoning_output_tokens", "reasoning_tokens"]),
        )
    }

    fn apply_codex_usage(
        rows: &mut [ConversationRow],
        parsed: &CodexLine,
        source_line: i64,
    ) -> bool {
        let scope = Self::json_string(&parsed.payload, &["scope", "usage_scope"])
            .unwrap_or_else(|| "response".to_string());
        if scope != "response" {
            return false;
        }
        let usage = parsed.payload.get("usage").unwrap_or(&parsed.payload);
        let response_id = Self::json_string(&parsed.payload, &["response_id"]);
        let turn_id = Self::json_string(&parsed.payload, &["turn_id"]);
        let target = rows.iter_mut().rev().find(|row| {
            matches!(
                row.message_type.as_str(),
                "assistant" | "agent_message" | "reasoning"
            ) && match response_id.as_ref() {
                Some(response_id) => row.response_id.as_ref() == Some(response_id),
                None => turn_id
                    .as_ref()
                    .is_some_and(|turn_id| row.turn_id.as_ref() == Some(turn_id)),
            }
        });
        if let Some(row) = target {
            let (i, o, cw, cr, r) = Self::usage_fields(usage);
            if row.usage_scope.is_some() {
                return row.usage_scope.as_deref() == Some("response")
                    && (
                        row.input_tokens,
                        row.output_tokens,
                        row.cache_creation_tokens,
                        row.cache_read_tokens,
                        row.reasoning_tokens,
                    ) == (i, o, cw, cr, r);
            }
            row.input_tokens = i;
            row.output_tokens = o;
            row.cache_creation_tokens = cw;
            row.cache_read_tokens = cr;
            row.reasoning_tokens = r;
            row.usage_scope = Some(scope);
            row.usage_source_line = Some(source_line);
            true
        } else {
            false
        }
    }

    fn apply_codex_legacy_usage(
        rows: &mut [ConversationRow],
        parsed: &CodexLine,
        source_line: i64,
    ) -> bool {
        let event_type = Self::json_string(&parsed.payload, &["type"]);
        if event_type.as_deref() != Some("token_count") {
            return false;
        }
        let usage = parsed
            .payload
            .get("last_token_usage")
            .or_else(|| parsed.payload.pointer("/info/last_token_usage"));
        let Some(usage) = usage else {
            return false;
        };
        let target = rows.iter_mut().rev().find(|row| {
            matches!(
                row.message_type.as_str(),
                "assistant" | "agent_message" | "reasoning"
            ) && row.usage_scope.is_none()
        });
        if let Some(row) = target {
            let (i, o, cw, cr, r) = Self::usage_fields(usage);
            row.input_tokens = i;
            row.output_tokens = o;
            row.cache_creation_tokens = cw;
            row.cache_read_tokens = cr;
            row.reasoning_tokens = r;
            row.usage_scope = Some("legacy".to_string());
            row.usage_source_line = Some(source_line);
            return true;
        }
        false
    }

    fn apply_codex_terminal(rows: &mut [ConversationRow], parsed: &CodexLine) -> bool {
        let response_id = Self::json_string(&parsed.payload, &["response_id"]);
        let turn_id = Self::codex_turn_id(&parsed.payload);
        let event_type = Self::json_string(&parsed.payload, &["type"])
            .unwrap_or_else(|| parsed.line_type.clone());
        let matches_terminal = |row: &ConversationRow| {
            match response_id.as_ref() {
                Some(id) => row.response_id.as_ref() == Some(id),
                None => turn_id
                    .as_ref()
                    .is_some_and(|id| row.turn_id.as_ref() == Some(id)),
            }
        };
        let target = rows
            .iter()
            .rposition(|row| {
                matches_terminal(row)
                    && matches!(row.message_type.as_str(), "assistant" | "agent_message")
            })
            .or_else(|| {
                rows.iter().rposition(|row| {
                    matches_terminal(row) && row.message_type == "reasoning"
                })
            });
        if let Some(target) = target {
            let row = &mut rows[target];
            row.status = Self::codex_terminal_status(&parsed.payload, &event_type);
            row.stop_reason = Self::codex_terminal_reason(&parsed.payload, &event_type);
            true
        } else {
            false
        }
    }

    fn codex_terminal_status(payload: &serde_json::Value, event_type: &str) -> Option<String> {
        Self::json_string(payload, &["status"]).or_else(|| match event_type {
            "turn_aborted" => Some("interrupted".to_string()),
            "task_complete" | "task_completed" => Some("completed".to_string()),
            _ => None,
        })
    }

    fn codex_terminal_reason(payload: &serde_json::Value, event_type: &str) -> Option<String> {
        Self::json_string(payload, &["reason", "stop_reason", "completion_reason"]).or_else(|| {
            match event_type {
                "turn_aborted" => Some("interrupted".to_string()),
                "task_complete" | "task_completed" => Some("completed".to_string()),
                _ => None,
            }
        })
    }

    fn codex_usage_row(
        session_id: &str,
        file_name: &str,
        file_path: &Path,
        line_number: i64,
        byte_offset: i64,
        parsed: &CodexLine,
        raw_event: &str,
        meta: &CodexSessionMeta,
        turn: &CodexTurnContext,
        archived: Option<bool>,
        index_metadata: &serde_json::Value,
        retain_raw_event: bool,
        retain_metadata: bool,
    ) -> ConversationRow {
        let mut row = Self::codex_base_row(
            session_id,
            file_name,
            file_path,
            line_number,
            byte_offset,
            parsed.timestamp.clone(),
            meta,
            turn,
            archived,
            index_metadata,
            "token_usage_record",
            raw_event,
            retain_raw_event,
            retain_metadata,
        );
        let usage = parsed.payload.get("usage").unwrap_or(&parsed.payload);
        let (i, o, cw, cr, r) = Self::usage_fields(usage);
        row.message_type = "token_usage".to_string();
        row.input_tokens = i;
        row.output_tokens = o;
        row.cache_creation_tokens = cw;
        row.cache_read_tokens = cr;
        row.reasoning_tokens = r;
        row.usage_scope = Self::json_string(&parsed.payload, &["scope", "usage_scope"])
            .or(Some("response".to_string()));
        row.usage_source_line = Some(line_number);
        row.response_id = Self::json_string(&parsed.payload, &["response_id"]).or(row.response_id);
        row.turn_id = Self::json_string(&parsed.payload, &["turn_id"]).or(row.turn_id);
        row.root_turn_id =
            Self::json_string(&parsed.payload, &["root_turn_id"]).or(row.root_turn_id);
        row.record_id = Some(Self::codex_record_id(
            &row.session_id,
            None,
            "token_usage_record",
            &parsed.payload,
            line_number,
        ));
        row
    }
}

impl ConversationRow {
    fn with_raw(mut self, raw: &str) -> Self {
        self.raw_event = Some(raw.to_string());
        self.record_id = Some(Conversations::codex_record_id(
            &self.session_id,
            None,
            "_parse_error",
            &serde_json::Value::String(raw.to_string()),
            self.line_number,
        ));
        self
    }
}

// ─── Gemini loading ───

impl Conversations {
    fn load_gemini_rows(base_path: &std::path::Path) -> Vec<ConversationRow> {
        let chat_files = utils::discover_gemini_chat_files(base_path);
        let project_map = utils::read_gemini_project_map(base_path);
        let mut rows = Vec::new();

        for (project_hash, file_path) in &chat_files {
            let file_name = file_path
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();

            let content = match std::fs::read_to_string(file_path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let session = match serde_json::from_str::<GeminiSession>(&content) {
                Ok(s) => s,
                Err(e) => {
                    rows.push(ConversationRow {
                        source: "gemini".to_string(),
                        session_id: project_hash.clone(),
                        project_dir: project_hash.clone(),
                        file_name: file_name.clone(),
                        line_number: 1,
                        message_type: "_parse_error".to_string(),
                        message_content: Some(format!("Parse error: {}", e)),
                        ..Default::default()
                    });
                    continue;
                }
            };

            let session_id = session
                .session_id
                .clone()
                .unwrap_or_else(|| project_hash.clone());
            // Resolve the project hash back to an absolute path when an alias
            // mapping exists; otherwise leave the project path empty (the hash
            // is an opaque SHA-256 of the original cwd).
            let project_path = project_map.get(project_hash).cloned().unwrap_or_default();
            let is_agent = session.kind.as_deref() == Some("subagent");

            // Each message gets a 1-based ordinal within the session file.
            let mut message_index: i64 = 0;
            for msg in &session.messages {
                message_index += 1;
                let (message_type, message_role) =
                    Self::gemini_type_role(msg.message_type.as_deref());

                let mut row = ConversationRow {
                    source: "gemini".to_string(),
                    session_id: session_id.clone(),
                    project_path: project_path.clone(),
                    project_dir: project_hash.clone(),
                    file_name: file_name.clone(),
                    is_agent,
                    line_number: message_index,
                    message_type: message_type.to_string(),
                    uuid: msg.id.clone(),
                    timestamp: msg.timestamp.clone().or_else(|| session.start_time.clone()),
                    message_role: message_role.map(String::from),
                    message_content: msg.content.clone().filter(|c| !c.is_empty()),
                    model: msg.model.clone(),
                    cwd: if project_path.is_empty() {
                        None
                    } else {
                        Some(project_path.clone())
                    },
                    ..Default::default()
                };

                if let Some(tokens) = &msg.tokens {
                    row.input_tokens = tokens.input;
                    row.output_tokens = tokens.output;
                    // Gemini reports a single `cached` figure (read-side reuse).
                    row.cache_read_tokens = tokens.cached;
                }

                // Every tool call is emitted as its own dedicated `tool_call` row
                // below, so the assistant row deliberately leaves the tool_* fields
                // unset. This keeps each invocation represented exactly once and
                // avoids double-counting the first call in tool-usage aggregates.
                let tool_calls = msg.tool_calls.as_deref().unwrap_or(&[]);
                rows.push(row);

                for tc in tool_calls {
                    rows.push(ConversationRow {
                        source: "gemini".to_string(),
                        session_id: session_id.clone(),
                        project_path: project_path.clone(),
                        project_dir: project_hash.clone(),
                        file_name: file_name.clone(),
                        is_agent,
                        line_number: message_index,
                        message_type: "tool_call".to_string(),
                        uuid: tc.id.clone(),
                        parent_uuid: msg.id.clone(),
                        timestamp: tc.timestamp.clone().or_else(|| msg.timestamp.clone()),
                        message_role: Some("tool".to_string()),
                        message_content: tc.status.clone(),
                        model: msg.model.clone(),
                        tool_name: tc.name.clone(),
                        tool_use_id: tc.id.clone(),
                        tool_input: tc.args.as_ref().map(|a| a.to_string()),
                        cwd: if project_path.is_empty() {
                            None
                        } else {
                            Some(project_path.clone())
                        },
                        ..Default::default()
                    });
                }
            }
        }
        rows
    }

    /// Map a Gemini message `type` to (message_type, message_role).
    /// Gemini uses `gemini` for the assistant; everything else passes through.
    fn gemini_type_role(message_type: Option<&str>) -> (&'static str, Option<&'static str>) {
        match message_type {
            Some("user") => ("user", Some("user")),
            Some("gemini") => ("assistant", Some("assistant")),
            Some("info") => ("info", None),
            Some("error") => ("error", None),
            _ => ("unknown", None),
        }
    }
}

// ─── Cursor loading ───
//
// Cursor stores chat in a SQLite KV store (state.vscdb). `composerData:<id>` rows
// are conversations; `bubbleId:<composerId>:<bubbleId>` rows are messages. Order
// within a composer comes from its `fullConversationHeadersOnly` array.
//
// The `state.vscdb` SQLite file is read with a self-contained, pure-Rust,
// read-only reader (`crate::vscdb`) — no external SQLite dependency. The whole
// path is gated behind the default-on `cursor` cargo feature. Parsing is
// defensive: missing keys/rows are tolerated and never panic (every field falls
// back to NULL via `..Default::default()`).

#[cfg(feature = "cursor")]
impl Conversations {
    fn load_cursor_rows(base_path: &std::path::Path) -> Vec<ConversationRow> {
        use crate::vscdb::VscDb;

        let db_path = if base_path.extension().map_or(false, |e| e == "vscdb") {
            base_path.to_path_buf()
        } else {
            base_path.join("state.vscdb")
        };

        let db = match VscDb::open(&db_path) {
            Some(db) => db,
            None => return Vec::new(),
        };

        // Single scan of the cursorDiskKV table; split the rows by key prefix.
        // (Equivalent to the two `key LIKE 'composerData:%' / 'bubbleId:%'`
        // queries the bundled-SQLite version used to run.)
        let mut composers: std::collections::HashMap<String, CursorComposer> =
            std::collections::HashMap::new();
        let mut bubbles: std::collections::HashMap<(String, String), CursorBubble> =
            std::collections::HashMap::new();

        for row in db.read_table("cursorDiskKV") {
            let key = match std::str::from_utf8(&row.key) {
                Ok(k) => k,
                Err(_) => continue,
            };
            if let Some(id) = key.strip_prefix("composerData:") {
                // 1. composers (sessions)
                if let Ok(c) = serde_json::from_slice::<CursorComposer>(&row.value) {
                    composers.insert(id.to_string(), c);
                }
            } else if key.starts_with("bubbleId:") {
                // 2. bubbles, keyed by (composerId, bubbleId)
                //    key = bubbleId:<composerId>:<bubbleId>
                let parts: Vec<&str> = key.splitn(3, ':').collect();
                if parts.len() == 3 {
                    if let Ok(b) = serde_json::from_slice::<CursorBubble>(&row.value) {
                        bubbles.insert((parts[1].to_string(), parts[2].to_string()), b);
                    }
                }
            }
        }

        // 3. Walk each composer's ordered headers, emit a row per bubble.
        let mut rows = Vec::new();
        let mut composer_ids: Vec<&String> = composers.keys().collect();
        composer_ids.sort();

        for composer_id in composer_ids {
            let composer = &composers[composer_id];
            let model = composer
                .model_config
                .as_ref()
                .and_then(|m| m.model_name.clone());
            let headers = composer.headers.clone().unwrap_or_default();
            let mut prev_bubble: Option<String> = None;

            for (idx, header) in headers.iter().enumerate() {
                let bubble_id = match &header.bubble_id {
                    Some(b) => b.clone(),
                    None => continue,
                };
                let bubble = bubbles.get(&(composer_id.clone(), bubble_id.clone()));

                let (message_type, role) = match header.bubble_type {
                    Some(1) => ("user", Some("user")),
                    Some(2) => ("assistant", Some("assistant")),
                    _ => ("unknown", None),
                };

                let tool = bubble.and_then(|b| b.tool_former_data.as_ref());
                let timestamp = bubble
                    .and_then(|b| {
                        b.created_at
                            .or_else(|| b.timing_info.as_ref().and_then(|t| t.client_start_time))
                    })
                    .map(utils::epoch_ms_to_iso);

                rows.push(ConversationRow {
                    source: "cursor".to_string(),
                    session_id: composer_id.clone(),
                    file_name: "state.vscdb".to_string(),
                    line_number: idx as i64 + 1,
                    message_type: message_type.to_string(),
                    message_role: role.map(String::from),
                    uuid: Some(bubble_id.clone()),
                    parent_uuid: prev_bubble.clone(),
                    timestamp,
                    is_agent: bubble.and_then(|b| b.is_agentic).unwrap_or(false),
                    message_content: bubble.and_then(|b| b.text.clone()),
                    model: model.clone(),
                    tool_name: tool.and_then(|t| t.name.clone().or_else(|| t.tool.clone())),
                    tool_use_id: tool.and_then(|t| t.tool_call_id.clone()),
                    tool_input: tool.and_then(|t| {
                        t.raw_args
                            .as_ref()
                            .or(t.params.as_ref())
                            .map(|v| v.to_string())
                    }),
                    input_tokens: bubble
                        .and_then(|b| b.token_count.as_ref())
                        .and_then(|t| t.input_tokens),
                    output_tokens: bubble
                        .and_then(|b| b.token_count.as_ref())
                        .and_then(|t| t.output_tokens),
                    ..Default::default()
                });
                prev_bubble = Some(bubble_id);
            }
        }
        rows
    }
}

#[cfg(not(feature = "cursor"))]
impl Conversations {
    fn load_cursor_rows(_base_path: &std::path::Path) -> Vec<ConversationRow> {
        Vec::new()
    }
}

// ─── Grok loading ───
//
// chat_history.jsonl is the transcript (no per-line timestamp on disk).
// Sibling updates.jsonl IS the event clock: top-level `timestamp` (unix sec).
// We walk chat lines in order and assign ISO timestamps from matching update
// kinds so `timestamp` looks like Claude/Copilot (ISO string per row).
// Fallback: summary last_active_at → updated_at → created_at.
//
// UUIDs: reasoning keeps real `id` when present; else `{session_id}:{line}`.
// Tokens: last turn_completed.usage stamped as session aggregate on every row.
// Subagents: meta.json → is_agent + parent_uuid.

impl Conversations {
    /// Prefer a real message id; otherwise `{session_id}:{line_number}`.
    fn grok_row_uuid(existing: Option<String>, session_id: &str, line_number: i64) -> String {
        existing.unwrap_or_else(|| format!("{}:{}", session_id, line_number))
    }

    /// Which updates.jsonl sessionUpdate kinds stamp this chat_history type.
    fn grok_time_candidates(message_type: &str) -> &'static [&'static str] {
        match message_type {
            "user" => &["user_message_chunk"],
            "reasoning" => &["agent_thought_chunk"],
            "assistant" => &["agent_message_chunk", "tool_call"],
            "tool_call" => &["tool_call"],
            "tool_result" => &["tool_call_update", "tool_call"],
            "system" => &["user_message_chunk", "agent_thought_chunk"], // first real event-ish
            _ => &[],
        }
    }

    fn load_grok_rows(base_path: &std::path::Path) -> Vec<ConversationRow> {
        let files = utils::discover_grok_session_files(base_path);
        let subagent_parents = utils::discover_grok_subagent_parents(base_path);
        let mut rows = Vec::new();

        for (session_uuid, decoded_cwd, encoded_cwd, file_path) in &files {
            let session_dir = file_path.parent().unwrap_or(file_path);
            let summary = utils::read_grok_summary(session_dir);
            let usage = utils::read_grok_last_turn_usage(session_dir);
            let mut time_cursor =
                utils::GrokTimeCursor::new(utils::read_grok_update_timeline(session_dir));

            let session_ts = summary.as_ref().and_then(utils::grok_session_timestamp);
            let git_branch = summary.as_ref().and_then(|s| s.head_branch.clone());
            let repository = summary
                .as_ref()
                .and_then(|s| s.git_remotes.as_ref())
                .and_then(|r| r.first().cloned());
            let session_model = summary.as_ref().and_then(|s| s.current_model_id.clone());
            let session_effort = summary.as_ref().and_then(|s| s.reasoning_effort.clone());
            let slug = summary.as_ref().and_then(|s| s.generated_title.clone());
            let version = summary.as_ref().and_then(|s| {
                s.chat_format_version.as_ref().map(|v| match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
            });
            let project_path = summary
                .as_ref()
                .and_then(|s| s.git_root_dir.clone())
                .unwrap_or_else(|| decoded_cwd.clone());

            let is_agent = subagent_parents.contains_key(session_uuid);
            let parent_uuid = subagent_parents.get(session_uuid).cloned();

            let file = match std::fs::File::open(file_path) {
                Ok(f) => f,
                Err(_) => continue,
            };

            let mut file_line: i64 = 0;
            for line_result in BufReader::new(file).lines() {
                file_line += 1;
                let line = match line_result {
                    Ok(l) if !l.trim().is_empty() => l,
                    _ => continue,
                };

                // Peek type for timestamp assignment before full map.
                let peek_type = serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|v| {
                        v.get("type")
                            .and_then(|t| t.as_str())
                            .map(|s| s.to_string())
                    })
                    .unwrap_or_default();
                let row_ts = if peek_type == "system" {
                    // Prefer session created_at for system preamble (before wire events).
                    summary
                        .as_ref()
                        .and_then(|s| s.created_at.clone())
                        .or_else(|| session_ts.clone())
                } else {
                    time_cursor.next_or(Self::grok_time_candidates(&peek_type), &session_ts)
                };

                let base = ConversationRow {
                    source: "grok".to_string(),
                    session_id: session_uuid.clone(),
                    project_path: project_path.clone(),
                    project_dir: encoded_cwd.clone(),
                    file_name: "chat_history.jsonl".to_string(),
                    is_agent,
                    line_number: file_line,
                    parent_uuid: parent_uuid.clone(),
                    timestamp: row_ts,
                    slug: slug.clone(),
                    git_branch: git_branch.clone(),
                    cwd: Some(decoded_cwd.clone()),
                    version: version.clone(),
                    repository: repository.clone(),
                    // Session/prompt aggregate from last turn_completed (duplicated).
                    input_tokens: usage.as_ref().and_then(|u| u.input_tokens),
                    output_tokens: usage.as_ref().and_then(|u| u.output_tokens),
                    cache_read_tokens: usage.as_ref().and_then(|u| u.cached_read_tokens),
                    reasoning_tokens: usage.as_ref().and_then(|u| u.reasoning_tokens),
                    ..Default::default()
                };

                match serde_json::from_str::<GrokMessage>(&line) {
                    Ok(msg) => {
                        for mut row in Self::grok_message_to_rows(
                            msg,
                            base,
                            session_model.as_deref(),
                            session_effort.as_deref(),
                        ) {
                            row.uuid = Some(Self::grok_row_uuid(
                                row.uuid.take(),
                                session_uuid,
                                file_line,
                            ));
                            rows.push(row);
                        }
                    }
                    Err(e) => rows.push(ConversationRow {
                        message_type: "_parse_error".to_string(),
                        message_content: Some(format!("Parse error: {}", e)),
                        uuid: Some(Self::grok_row_uuid(None, session_uuid, file_line)),
                        ..base
                    }),
                }
            }
        }
        rows
    }

    /// Serialize tool `arguments` (JSON string or object) to a stable varchar.
    fn grok_tool_input(args: &Option<serde_json::Value>) -> Option<String> {
        args.as_ref().map(|v| match v {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        })
    }

    /// Map one chat_history line to one or more rows.
    /// Assistant with tool_calls → optional text row + one `tool_call` row per call
    /// (Gemini-style multi-tool fan-out; first tool is no longer collapsed).
    fn grok_message_to_rows(
        msg: GrokMessage,
        base: ConversationRow,
        session_model: Option<&str>,
        session_effort: Option<&str>,
    ) -> Vec<ConversationRow> {
        match msg {
            GrokMessage::User(u) => vec![ConversationRow {
                message_type: "user".to_string(),
                message_role: Some("user".to_string()),
                message_content: u.content.as_ref().map(utils::extract_text_content),
                ..base
            }],
            GrokMessage::Reasoning(r) => {
                // Like Codex: message_type=reasoning, role=assistant, summary text only.
                let effort = r
                    .reasoning_effort
                    .or_else(|| session_effort.map(String::from));
                vec![ConversationRow {
                    message_type: "reasoning".to_string(),
                    message_role: Some("assistant".to_string()),
                    uuid: r.id,
                    message_content: r.summary.as_ref().map(utils::extract_text_content),
                    reasoning_effort: effort,
                    model: session_model.map(String::from),
                    ..base
                }]
            }
            GrokMessage::Assistant(a) => {
                let model = a
                    .model_id
                    .clone()
                    .or_else(|| session_model.map(String::from));
                let effort = a
                    .reasoning_effort
                    .or_else(|| session_effort.map(String::from));
                let content = a.content.filter(|c| !c.is_empty());
                let mut out = Vec::new();

                // Text row when content is non-empty; if only tools, skip empty
                // assistant shell (tool rows alone).
                if content.is_some() || a.tool_calls.is_empty() {
                    out.push(ConversationRow {
                        message_type: "assistant".to_string(),
                        message_role: Some("assistant".to_string()),
                        message_content: content,
                        model: model.clone(),
                        reasoning_effort: effort.clone(),
                        ..base.clone()
                    });
                }

                for tc in &a.tool_calls {
                    out.push(ConversationRow {
                        message_type: "tool_call".to_string(),
                        message_role: Some("tool".to_string()),
                        model: model.clone(),
                        reasoning_effort: effort.clone(),
                        tool_name: tc.name.clone(),
                        tool_use_id: tc.id.clone(),
                        tool_input: Self::grok_tool_input(&tc.arguments),
                        ..base.clone()
                    });
                }
                out
            }
            GrokMessage::ToolResult(t) => vec![ConversationRow {
                message_type: "tool_result".to_string(),
                message_role: Some("tool".to_string()),
                tool_use_id: t.tool_call_id,
                message_content: t.content.as_ref().map(utils::extract_text_content),
                ..base
            }],
            GrokMessage::System(s) => vec![ConversationRow {
                message_type: "system".to_string(),
                message_content: s.content.as_ref().map(utils::extract_text_content),
                ..base
            }],
        }
    }
}

// ─── TableFunc implementation ───

impl TableFunc for Conversations {
    type Row = ConversationRow;

    fn columns() -> Vec<ColDef> {
        vec![
            vtab::varchar("source"),
            vtab::varchar("session_id"),
            vtab::varchar("project_path"),
            vtab::varchar("project_dir"),
            vtab::varchar("file_name"),
            vtab::boolean("is_agent"),
            vtab::bigint("line_number"),
            vtab::varchar("message_type"),
            vtab::varchar("uuid"),
            vtab::varchar("parent_uuid"),
            vtab::varchar("timestamp"),
            vtab::varchar("message_role"),
            vtab::varchar("message_content"),
            vtab::varchar("model"),
            vtab::varchar("tool_name"),
            vtab::varchar("tool_use_id"),
            vtab::varchar("tool_input"),
            vtab::bigint("input_tokens"),
            vtab::bigint("output_tokens"),
            vtab::bigint("cache_creation_tokens"),
            vtab::bigint("cache_read_tokens"),
            vtab::bigint("reasoning_tokens"),
            vtab::varchar("slug"),
            vtab::varchar("git_branch"),
            vtab::varchar("cwd"),
            vtab::varchar("version"),
            vtab::varchar("stop_reason"),
            vtab::varchar("reasoning_effort"),
            vtab::varchar("repository"),
            vtab::varchar("record_id"),
            vtab::varchar("file_path"),
            vtab::bigint("byte_offset"),
            vtab::bigint("ordinal"),
            vtab::varchar("event_type"),
            vtab::varchar("client"),
            vtab::varchar("originator"),
            vtab::varchar("thread_source"),
            vtab::varchar("parent_session_id"),
            vtab::varchar("forked_from_session_id"),
            vtab::varchar("turn_id"),
            vtab::varchar("root_turn_id"),
            vtab::varchar("response_id"),
            vtab::varchar("model_provider"),
            vtab::varchar("git_commit"),
            vtab::varchar("agent_path"),
            vtab::varchar("agent_nickname"),
            vtab::varchar("agent_role"),
            vtab::varchar("channel"),
            vtab::varchar("status"),
            vtab::varchar("session_created_at"),
            vtab::varchar("session_updated_at"),
            vtab::boolean("archived"),
            vtab::varchar("usage_scope"),
            vtab::bigint("usage_source_line"),
            vtab::varchar("parse_error"),
            vtab::varchar("raw_event"),
            vtab::varchar("metadata"),
        ]
    }

    fn load_rows(path: Option<&str>, source: Option<&str>) -> Vec<ConversationRow> {
        let base_path = utils::resolve_data_path(path);
        match detect::resolve_provider(&base_path, source) {
            Provider::Claude => Self::load_claude_rows(&base_path),
            Provider::ClaudeDesktop => Self::load_claude_desktop_rows(&base_path),
            Provider::Copilot => Self::load_copilot_rows(&base_path),
            Provider::Cursor => Self::load_cursor_rows(&base_path),
            Provider::Codex => Self::load_codex_rows(path, false, true, true).unwrap_or_default(),
            Provider::Gemini => Self::load_gemini_rows(&base_path),
            Provider::Grok => Self::load_grok_rows(&base_path),
            Provider::Unknown => Vec::new(),
        }
    }

    fn supports_include_archived() -> bool {
        true
    }

    fn try_load_rows_with_options(
        path: Option<&str>,
        source: Option<&str>,
        include_archived: bool,
    ) -> duckdb::Result<Vec<ConversationRow>, Box<dyn std::error::Error>> {
        let base_path = utils::resolve_data_path(path);
        match detect::resolve_provider(&base_path, source) {
            Provider::Codex => Self::load_codex_rows(path, include_archived, true, true),
            _ => Ok(Self::load_rows(path, source)),
        }
    }

    fn supports_projection_pushdown() -> bool {
        true
    }

    fn try_load_rows_with_projection(
        path: Option<&str>,
        source: Option<&str>,
        include_archived: bool,
        projected_columns: &[usize],
    ) -> duckdb::Result<Vec<ConversationRow>, Box<dyn std::error::Error>> {
        let base_path = utils::resolve_data_path(path);
        match detect::resolve_provider(&base_path, source) {
            Provider::Codex => Self::load_codex_rows(
                path,
                include_archived,
                projected_columns.contains(&55),
                projected_columns.contains(&56),
            ),
            _ => Ok(Self::load_rows(path, source)),
        }
    }

    fn write_projected_row(
        output: &mut DataChunkHandle,
        idx: usize,
        row: &ConversationRow,
        projected_columns: &[usize],
    ) {
        for (output_col, source_col) in projected_columns.iter().copied().enumerate() {
            match source_col {
                0 => vtab::set_varchar(output, output_col, idx, &row.source),
                1 => vtab::set_varchar(output, output_col, idx, &row.session_id),
                2 => vtab::set_varchar_opt(
                    output,
                    output_col,
                    idx,
                    if row.source == "codex" && row.project_path.is_empty() {
                        None
                    } else {
                        Some(&row.project_path)
                    },
                ),
                3 => vtab::set_varchar_opt(
                    output,
                    output_col,
                    idx,
                    if row.source == "codex" && row.project_dir.is_empty() {
                        None
                    } else {
                        Some(&row.project_dir)
                    },
                ),
                4 => vtab::set_varchar(output, output_col, idx, &row.file_name),
                5 => vtab::set_bool(output, output_col, idx, row.is_agent),
                6 => vtab::set_i64(output, output_col, idx, row.line_number),
                7 => vtab::set_varchar(output, output_col, idx, &row.message_type),
                8 => vtab::set_varchar_opt(output, output_col, idx, row.uuid.as_deref()),
                9 => vtab::set_varchar_opt(output, output_col, idx, row.parent_uuid.as_deref()),
                10 => vtab::set_varchar_opt(output, output_col, idx, row.timestamp.as_deref()),
                11 => vtab::set_varchar_opt(output, output_col, idx, row.message_role.as_deref()),
                12 => {
                    vtab::set_varchar_opt(output, output_col, idx, row.message_content.as_deref())
                }
                13 => vtab::set_varchar_opt(output, output_col, idx, row.model.as_deref()),
                14 => vtab::set_varchar_opt(output, output_col, idx, row.tool_name.as_deref()),
                15 => vtab::set_varchar_opt(output, output_col, idx, row.tool_use_id.as_deref()),
                16 => vtab::set_varchar_opt(output, output_col, idx, row.tool_input.as_deref()),
                17 => vtab::set_i64_opt(output, output_col, idx, row.input_tokens),
                18 => vtab::set_i64_opt(output, output_col, idx, row.output_tokens),
                19 => vtab::set_i64_opt(output, output_col, idx, row.cache_creation_tokens),
                20 => vtab::set_i64_opt(output, output_col, idx, row.cache_read_tokens),
                21 => vtab::set_i64_opt(output, output_col, idx, row.reasoning_tokens),
                22 => vtab::set_varchar_opt(output, output_col, idx, row.slug.as_deref()),
                23 => vtab::set_varchar_opt(output, output_col, idx, row.git_branch.as_deref()),
                24 => vtab::set_varchar_opt(output, output_col, idx, row.cwd.as_deref()),
                25 => vtab::set_varchar_opt(output, output_col, idx, row.version.as_deref()),
                26 => vtab::set_varchar_opt(output, output_col, idx, row.stop_reason.as_deref()),
                27 => {
                    vtab::set_varchar_opt(output, output_col, idx, row.reasoning_effort.as_deref())
                }
                28 => vtab::set_varchar_opt(output, output_col, idx, row.repository.as_deref()),
                29 => vtab::set_varchar_opt(output, output_col, idx, row.record_id.as_deref()),
                30 => vtab::set_varchar_opt(output, output_col, idx, row.file_path.as_deref()),
                31 => vtab::set_i64_opt(output, output_col, idx, row.byte_offset),
                32 => vtab::set_i64_opt(output, output_col, idx, row.ordinal),
                33 => vtab::set_varchar_opt(output, output_col, idx, row.event_type.as_deref()),
                34 => vtab::set_varchar_opt(output, output_col, idx, row.client.as_deref()),
                35 => vtab::set_varchar_opt(output, output_col, idx, row.originator.as_deref()),
                36 => vtab::set_varchar_opt(output, output_col, idx, row.thread_source.as_deref()),
                37 => {
                    vtab::set_varchar_opt(output, output_col, idx, row.parent_session_id.as_deref())
                }
                38 => vtab::set_varchar_opt(
                    output,
                    output_col,
                    idx,
                    row.forked_from_session_id.as_deref(),
                ),
                39 => vtab::set_varchar_opt(output, output_col, idx, row.turn_id.as_deref()),
                40 => vtab::set_varchar_opt(output, output_col, idx, row.root_turn_id.as_deref()),
                41 => vtab::set_varchar_opt(output, output_col, idx, row.response_id.as_deref()),
                42 => vtab::set_varchar_opt(output, output_col, idx, row.model_provider.as_deref()),
                43 => vtab::set_varchar_opt(output, output_col, idx, row.git_commit.as_deref()),
                44 => vtab::set_varchar_opt(output, output_col, idx, row.agent_path.as_deref()),
                45 => vtab::set_varchar_opt(output, output_col, idx, row.agent_nickname.as_deref()),
                46 => vtab::set_varchar_opt(output, output_col, idx, row.agent_role.as_deref()),
                47 => vtab::set_varchar_opt(output, output_col, idx, row.channel.as_deref()),
                48 => vtab::set_varchar_opt(output, output_col, idx, row.status.as_deref()),
                49 => vtab::set_varchar_opt(
                    output,
                    output_col,
                    idx,
                    row.session_created_at.as_deref(),
                ),
                50 => vtab::set_varchar_opt(
                    output,
                    output_col,
                    idx,
                    row.session_updated_at.as_deref(),
                ),
                51 => match row.archived {
                    Some(value) => vtab::set_bool(output, output_col, idx, value),
                    None => output.flat_vector(output_col).set_null(idx),
                },
                52 => vtab::set_varchar_opt(output, output_col, idx, row.usage_scope.as_deref()),
                53 => vtab::set_i64_opt(output, output_col, idx, row.usage_source_line),
                54 => vtab::set_varchar_opt(output, output_col, idx, row.parse_error.as_deref()),
                55 => vtab::set_varchar_opt(output, output_col, idx, row.raw_event.as_deref()),
                56 => vtab::set_varchar_opt(output, output_col, idx, row.metadata.as_deref()),
                _ => unreachable!("unknown conversations column"),
            }
        }
    }

    fn write_row(output: &mut DataChunkHandle, idx: usize, row: &ConversationRow) {
        vtab::set_varchar(output, 0, idx, &row.source);
        vtab::set_varchar(output, 1, idx, &row.session_id);
        let project_path = if row.source == "codex" && row.project_path.is_empty() {
            None
        } else {
            Some(row.project_path.as_str())
        };
        let project_dir = if row.source == "codex" && row.project_dir.is_empty() {
            None
        } else {
            Some(row.project_dir.as_str())
        };
        vtab::set_varchar_opt(output, 2, idx, project_path);
        vtab::set_varchar_opt(output, 3, idx, project_dir);
        vtab::set_varchar(output, 4, idx, &row.file_name);
        vtab::set_bool(output, 5, idx, row.is_agent);
        vtab::set_i64(output, 6, idx, row.line_number);
        vtab::set_varchar(output, 7, idx, &row.message_type);
        vtab::set_varchar_opt(output, 8, idx, row.uuid.as_deref());
        vtab::set_varchar_opt(output, 9, idx, row.parent_uuid.as_deref());
        vtab::set_varchar_opt(output, 10, idx, row.timestamp.as_deref());
        vtab::set_varchar_opt(output, 11, idx, row.message_role.as_deref());
        vtab::set_varchar_opt(output, 12, idx, row.message_content.as_deref());
        vtab::set_varchar_opt(output, 13, idx, row.model.as_deref());
        vtab::set_varchar_opt(output, 14, idx, row.tool_name.as_deref());
        vtab::set_varchar_opt(output, 15, idx, row.tool_use_id.as_deref());
        vtab::set_varchar_opt(output, 16, idx, row.tool_input.as_deref());
        vtab::set_i64_opt(output, 17, idx, row.input_tokens);
        vtab::set_i64_opt(output, 18, idx, row.output_tokens);
        vtab::set_i64_opt(output, 19, idx, row.cache_creation_tokens);
        vtab::set_i64_opt(output, 20, idx, row.cache_read_tokens);
        vtab::set_i64_opt(output, 21, idx, row.reasoning_tokens);
        vtab::set_varchar_opt(output, 22, idx, row.slug.as_deref());
        vtab::set_varchar_opt(output, 23, idx, row.git_branch.as_deref());
        vtab::set_varchar_opt(output, 24, idx, row.cwd.as_deref());
        vtab::set_varchar_opt(output, 25, idx, row.version.as_deref());
        vtab::set_varchar_opt(output, 26, idx, row.stop_reason.as_deref());
        vtab::set_varchar_opt(output, 27, idx, row.reasoning_effort.as_deref());
        vtab::set_varchar_opt(output, 28, idx, row.repository.as_deref());
        vtab::set_varchar_opt(output, 29, idx, row.record_id.as_deref());
        vtab::set_varchar_opt(output, 30, idx, row.file_path.as_deref());
        vtab::set_i64_opt(output, 31, idx, row.byte_offset);
        vtab::set_i64_opt(output, 32, idx, row.ordinal);
        vtab::set_varchar_opt(output, 33, idx, row.event_type.as_deref());
        vtab::set_varchar_opt(output, 34, idx, row.client.as_deref());
        vtab::set_varchar_opt(output, 35, idx, row.originator.as_deref());
        vtab::set_varchar_opt(output, 36, idx, row.thread_source.as_deref());
        vtab::set_varchar_opt(output, 37, idx, row.parent_session_id.as_deref());
        vtab::set_varchar_opt(output, 38, idx, row.forked_from_session_id.as_deref());
        vtab::set_varchar_opt(output, 39, idx, row.turn_id.as_deref());
        vtab::set_varchar_opt(output, 40, idx, row.root_turn_id.as_deref());
        vtab::set_varchar_opt(output, 41, idx, row.response_id.as_deref());
        vtab::set_varchar_opt(output, 42, idx, row.model_provider.as_deref());
        vtab::set_varchar_opt(output, 43, idx, row.git_commit.as_deref());
        vtab::set_varchar_opt(output, 44, idx, row.agent_path.as_deref());
        vtab::set_varchar_opt(output, 45, idx, row.agent_nickname.as_deref());
        vtab::set_varchar_opt(output, 46, idx, row.agent_role.as_deref());
        vtab::set_varchar_opt(output, 47, idx, row.channel.as_deref());
        vtab::set_varchar_opt(output, 48, idx, row.status.as_deref());
        vtab::set_varchar_opt(output, 49, idx, row.session_created_at.as_deref());
        vtab::set_varchar_opt(output, 50, idx, row.session_updated_at.as_deref());
        match row.archived {
            Some(value) => vtab::set_bool(output, 51, idx, value),
            None => output.flat_vector(51).set_null(idx),
        }
        vtab::set_varchar_opt(output, 52, idx, row.usage_scope.as_deref());
        vtab::set_i64_opt(output, 53, idx, row.usage_source_line);
        vtab::set_varchar_opt(output, 54, idx, row.parse_error.as_deref());
        vtab::set_varchar_opt(output, 55, idx, row.raw_event.as_deref());
        vtab::set_varchar_opt(output, 56, idx, row.metadata.as_deref());
    }
}

#[cfg(test)]
mod codex_completion_tests {
    use super::*;

    fn generated(response_id: Option<&str>, turn_id: Option<&str>) -> ConversationRow {
        ConversationRow {
            message_type: "assistant".to_string(),
            response_id: response_id.map(str::to_string),
            turn_id: turn_id.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn response_usage_targets_the_matching_terminal_item_once() {
        let mut rows = vec![
            generated(Some("response-a"), Some("turn-a")),
            generated(Some("response-b"), Some("turn-b")),
        ];
        let parsed = CodexLine {
            line_type: "token_usage_record".to_string(),
            timestamp: None,
            payload: serde_json::json!({"scope": "response", "response_id": "response-a", "usage": {"input_tokens": 3, "output_tokens": 5}}),
        };
        assert!(Conversations::apply_codex_usage(&mut rows, &parsed, 7));
        assert_eq!(rows[0].output_tokens, Some(5));
        assert_eq!(rows[1].output_tokens, None);
        assert!(Conversations::apply_codex_usage(&mut rows, &parsed, 8));
        assert_eq!(rows[0].usage_source_line, Some(7));
    }

    #[test]
    fn inline_response_usage_stays_on_its_generated_item() {
        let parsed = CodexLine {
            line_type: "response_item".to_string(),
            timestamp: None,
            payload: serde_json::json!({
                "id": "answer-1", "type": "message", "role": "assistant",
                "content": [{"type": "output_text", "text": "done"}],
                "usage": {"input_tokens": 7, "output_tokens": 2, "cached_input_tokens": 3}
            }),
        };
        let row = Conversations::codex_line_to_row(
            &parsed,
            "{}",
            "session-1",
            "rollout.jsonl",
            Path::new("rollout.jsonl"),
            4,
            30,
            &CodexSessionMeta::default(),
            &CodexTurnContext::default(),
            Some(false),
            &serde_json::json!({}),
            false,
            false,
        )
        .unwrap();
        assert_eq!(row.uuid.as_deref(), Some("answer-1"));
        assert_eq!(row.message_content.as_deref(), Some("done"));
        assert_eq!(
            (row.input_tokens, row.output_tokens, row.cache_read_tokens),
            (Some(7), Some(2), Some(3))
        );
        assert_eq!(row.usage_scope.as_deref(), Some("response"));
        assert_eq!(row.usage_source_line, Some(4));
    }

    #[test]
    fn completion_ids_follow_the_native_item_or_call_identity() {
        let value = serde_json::json!({"item": {"id": "item-1", "call_id": "call-1"}});
        assert_eq!(
            Conversations::codex_item_id(&value).as_deref(),
            Some("item-1")
        );
        assert_eq!(
            Conversations::codex_call_id(&value).as_deref(),
            Some("call-1")
        );
    }
}
