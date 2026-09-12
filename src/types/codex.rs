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
// NOTE: rollout files remain the canonical Codex CLI transcript. The separate
// Codex Work/Remote/Chat providers read the thread projection in state_5.sqlite
// plus thread_history_1.sqlite; unrelated logs, goals, and browser caches stay
// out of the reader.

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
    pub source: Option<String>,
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
    /// Reasoning effort for the turn (for example, "low" / "medium" / "high").
    pub effort: Option<String>,
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

/// `arguments` of an `update_plan` function call.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CodexPlanUpdate {
    pub explanation: Option<String>,
    pub plan: Vec<CodexPlanStep>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CodexPlanStep {
    pub step: String,
    pub status: String,
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

/// One user prompt from `~/.codex/history.jsonl`.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(default)]
pub struct CodexHistoryEntry {
    pub session_id: Option<String>,
    /// Codex stores this as Unix seconds, unlike Claude's millisecond field.
    pub ts: Option<i64>,
    pub text: Option<String>,
}
