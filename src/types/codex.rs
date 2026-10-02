use serde::Deserialize;

// ─── Codex rollout JSONL (the transcript) ───
//
// Path: ~/.codex/sessions/YYYY/MM/DD/rollout-<ISO-ts>-<session-uuid>.jsonl
// Every line: { type, timestamp, payload }. Top-level `type`:
//   session_meta  (1×, first line: id, cwd, git, cli_version, model_provider)
//   turn_context  (model, cwd, effort, ... — carry-forward for model)
//   response_item (message | reasoning | function_call | function_call_output | web_search_call)
//   event_msg     (user_message | agent_message | token_count | task_* )
//
// NOTE: the conversation lives ONLY in these rollout files. The *.sqlite files
// under ~/.codex (codex-dev.db, logs_*, state_*, goals_*, memories_*) are app
// automation / logging / inbox data and are intentionally NOT read here.

#[derive(Deserialize, Debug, Clone)]
pub struct CodexLine {
    #[serde(rename = "type")]
    pub line_type: String,
    pub timestamp: Option<String>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CodexSessionMeta {
    pub id: Option<String>,
    pub cwd: Option<String>,
    pub git: Option<CodexGit>,
    pub cli_version: Option<String>,
    pub model_provider: Option<String>,
    pub originator: Option<String>,
    /// `"cli"`, `"exec"`, `"vscode"`, ... for a top-level session; an object such
    /// as `{"subagent": {"thread_spawn": {...}}}` for a sub-agent's session.
    pub source: Option<serde_json::Value>,
    pub parent_thread_id: Option<String>,
}

impl CodexSessionMeta {
    /// Codex marks the sessions it spawns itself in `source.subagent`.
    pub fn is_subagent(&self) -> bool {
        self.source.as_ref().and_then(|s| s.get("subagent")).is_some()
    }

    pub fn parent_session_id(&self) -> Option<String> {
        self.parent_thread_id.clone().or_else(|| {
            self.source.as_ref()?
                .pointer("/subagent/thread_spawn/parent_thread_id")?
                .as_str()
                .map(String::from)
        })
    }
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CodexGit {
    pub branch: Option<String>,
    pub commit_hash: Option<String>,
    pub repository_url: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CodexTurnContext {
    pub model: Option<String>,
    pub cwd: Option<String>,
}

/// `payload` of a `response_item` line. `type` discriminates the variant.
#[derive(Deserialize, Debug, Clone, Default)]
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
    // function_call_output
    pub output: Option<serde_json::Value>,
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
