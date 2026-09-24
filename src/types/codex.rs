use serde::{Deserialize, Serialize};

// ─── Codex rollout JSONL (the transcript) ───
//
// Path: ~/.codex/sessions/YYYY/MM/DD/rollout-<ISO-ts>-<session-uuid>.jsonl
// Every line: { type, timestamp, payload }. Top-level `type`:
//   session_meta  (1×, first line: id, cwd, git, cli_version, model_provider)
//   turn_context  (model, cwd, effort, ... — carry-forward for model)
//   response_item (message | reasoning | function_call | function_call_output | web_search_call)
//   event_msg     (user_message | agent_message | token_count | task_* )
//
// Rollouts carry the transcript. The session index and versioned state database
// supply thread metadata that is absent from the rollout records.

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct CodexLine {
    #[serde(rename = "type")]
    pub line_type: String,
    pub timestamp: Option<String>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CodexSessionMeta {
    pub id: Option<String>,
    pub cwd: Option<String>,
    pub git: Option<CodexGit>,
    pub cli_version: Option<String>,
    pub model_provider: Option<String>,
    pub originator: Option<String>,
    /// Codex has shipped both a string source and a structured source object.
    /// Keeping it as JSON prevents a newer source variant from making the
    /// entire optional metadata record unreadable.
    pub source: Option<serde_json::Value>,
    pub session_id: Option<String>,
    pub parent_thread_id: Option<String>,
    pub forked_from_id: Option<String>,
    pub thread_source: Option<String>,
    pub agent_path: Option<String>,
    pub agent_nickname: Option<String>,
    pub agent_role: Option<String>,
    pub timestamp: Option<String>,
    pub title: Option<String>,
    #[serde(flatten)]
    pub additional: serde_json::Map<String, serde_json::Value>,
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub raw_source: Option<serde_json::Value>,
}

impl CodexSessionMeta {
    pub fn from_value_tolerant(value: &serde_json::Value) -> (Self, Vec<String>) {
        let Some(source) = value.as_object() else {
            return (
                Self::default(),
                vec!["session_meta payload is not an object".to_string()],
            );
        };
        let mut clean = source.clone();
        let mut diagnostics = Vec::new();
        for key in [
            "id",
            "cwd",
            "cli_version",
            "model_provider",
            "originator",
            "session_id",
            "parent_thread_id",
            "forked_from_id",
            "thread_source",
            "agent_path",
            "agent_nickname",
            "agent_role",
            "timestamp",
            "title",
        ] {
            if let Some(field) = clean.get(key) {
                if !field.is_null() && !field.is_string() {
                    diagnostics.push(format!("session_meta.{key} has an unexpected type"));
                    clean.remove(key);
                }
            }
        }
        if let Some(git) = clean.get_mut("git") {
            if let Some(fields) = git.as_object_mut() {
                for key in ["branch", "commit_hash", "repository_url"] {
                    if let Some(field) = fields.get(key) {
                        if !field.is_null() && !field.is_string() {
                            diagnostics
                                .push(format!("session_meta.git.{key} has an unexpected type"));
                            fields.remove(key);
                        }
                    }
                }
            } else if !git.is_null() {
                diagnostics.push("session_meta.git has an unexpected type".to_string());
                clean.remove("git");
            }
        }
        let mut meta: Self =
            serde_json::from_value(serde_json::Value::Object(clean)).unwrap_or_default();
        meta.raw_source = Some(value.clone());
        (meta, diagnostics)
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CodexGit {
    pub branch: Option<String>,
    pub commit_hash: Option<String>,
    pub repository_url: Option<String>,
}

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CodexTurnContext {
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub turn_id: Option<String>,
    pub root_turn_id: Option<String>,
    pub response_id: Option<String>,
    pub reasoning_effort: Option<String>,
    #[serde(alias = "effort")]
    pub effort: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox_policy: Option<serde_json::Value>,
    pub runtime_workspace_roots: Option<serde_json::Value>,
}

/// `payload` of a `response_item` line. `type` discriminates the variant.
#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CodexResponseItem {
    #[serde(rename = "type")]
    pub item_type: Option<String>,
    pub role: Option<String>,
    /// list of `{type: input_text|output_text|text, text}` blocks
    pub content: Option<serde_json::Value>,
    // reasoning
    pub summary: Option<serde_json::Value>,
    // function_call
    pub name: Option<String>,
    pub arguments: Option<serde_json::Value>,
    pub call_id: Option<String>,
    pub id: Option<String>,
    pub item_id: Option<String>,
    pub parent_id: Option<String>,
    pub parent_item_id: Option<String>,
    pub response_id: Option<String>,
    pub status: Option<String>,
    pub channel: Option<String>,
    pub error: Option<serde_json::Value>,
    pub usage: Option<serde_json::Value>,
    // function_call_output
    pub output: Option<serde_json::Value>,
    /// `custom_tool_call` puts its arguments here rather than in `arguments`,
    /// as either a raw string (e.g. an apply_patch body) or a JSON object.
    pub input: Option<serde_json::Value>,
}

/// `payload` of an `event_msg` line.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CodexEventMsg {
    #[serde(rename = "type")]
    pub event_type: Option<String>,
    pub message: Option<String>,
    pub phase: Option<String>,
    pub last_agent_message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::CodexSessionMeta;

    #[test]
    fn malformed_optional_metadata_keeps_valid_fields_and_source() {
        let source = serde_json::json!({
            "id": "native-session",
            "cwd": "/work/project",
            "originator": 42,
            "git": {"branch": "main", "commit_hash": false},
            "future_field": {"value": 7}
        });
        let (meta, diagnostics) = CodexSessionMeta::from_value_tolerant(&source);
        assert_eq!(meta.id.as_deref(), Some("native-session"));
        assert_eq!(meta.cwd.as_deref(), Some("/work/project"));
        assert_eq!(
            meta.git.as_ref().and_then(|git| git.branch.as_deref()),
            Some("main")
        );
        assert!(meta.originator.is_none());
        assert_eq!(
            meta.additional.get("future_field"),
            source.get("future_field")
        );
        assert_eq!(meta.raw_source.as_ref(), Some(&source));
        assert_eq!(diagnostics.len(), 2);
    }
}
