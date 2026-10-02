use std::path::Path;

/// Supported data providers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Provider {
    Claude,
    ClaudeDesktop,
    Copilot,
    Cursor,
    Codex,
    Gemini,
    Grok,
    Unknown,
}

/// Auto-detect provider from directory structure.
/// - `local-agent-mode-sessions/` directory → Claude Desktop
/// - `projects/` directory → Claude
/// - `session-state/` directory → Copilot
/// - `state.vscdb` file (passed directly or found in the directory) → Cursor
/// - `sessions/` with `YYYY/` date partitions (rollout files) → Codex
/// - `tmp/` directory + `installation_id` file → Gemini CLI (`~/.gemini`)
/// - `sessions/` directory containing url-encoded cwd dirs (`%...`) → Grok
pub fn detect_provider(path: &Path) -> Provider {
    if path.join("local-agent-mode-sessions").is_dir() {
        return Provider::ClaudeDesktop;
    }
    if path.join("projects").is_dir() {
        return Provider::Claude;
    }
    if path.join("session-state").is_dir() {
        return Provider::Copilot;
    }
    // Cursor: the vscdb file may be passed directly, or its parent directory.
    if path.extension().map_or(false, |e| e == "vscdb") || path.join("state.vscdb").is_file() {
        return Provider::Cursor;
    }
    // Codex partitions transcripts by date: sessions/YYYY/MM/DD/rollout-*.jsonl.
    let sessions = path.join("sessions");
    if sessions.is_dir() {
        let has_year_dir = std::fs::read_dir(&sessions)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                e.path().is_dir()
                    && e.file_name()
                        .to_string_lossy()
                        .chars()
                        .all(|c| c.is_ascii_digit())
            });
        if has_year_dir {
            return Provider::Codex;
        }
        // Grok encodes the cwd as the session subdir name (starts with '%').
        let looks_grok = std::fs::read_dir(&sessions)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().starts_with('%'));
        if looks_grok {
            return Provider::Grok;
        }
    }
    // Gemini CLI keeps chats under `tmp/<project-hash>/chats/`. The `tmp/` name
    // alone is too generic, so require the Gemini-specific `installation_id`
    // file (written by the CLI to `~/.gemini`) as a corroborating marker.
    if path.join("tmp").is_dir() && path.join("installation_id").is_file() {
        return Provider::Gemini;
    }
    Provider::Unknown
}

/// The source names accepted by `parse_source`, for error messages.
pub const KNOWN_SOURCES: &[&str] = &[
    "claude", "claude-desktop", "copilot", "cursor", "codex", "gemini", "grok",
];

/// Parse an explicit source string into a Provider. `_` and spaces are
/// treated as `-`, so `claude_desktop` names the same provider as
/// `claude-desktop`.
pub fn parse_source(source: &str) -> Provider {
    let normalised: String = source
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c == '_' || c == ' ' { '-' } else { c })
        .collect();
    match normalised.as_str() {
        "claude" => Provider::Claude,
        "claude-desktop" => Provider::ClaudeDesktop,
        "copilot" => Provider::Copilot,
        "cursor" => Provider::Cursor,
        "codex" => Provider::Codex,
        "gemini" => Provider::Gemini,
        "grok" => Provider::Grok,
        _ => Provider::Unknown,
    }
}

/// Resolve provider: an explicit source is used as given, never replaced by
/// auto-detection. `validate_source` rejects unknown names at bind time.
pub fn resolve_provider(path: &Path, source: Option<&str>) -> Provider {
    match source {
        Some(s) => parse_source(s),
        None => detect_provider(path),
    }
}

/// Reject an explicit `source` that names no known provider, listing the
/// valid names.
pub fn validate_source(source: Option<&str>) -> Result<(), String> {
    match source {
        Some(s) if parse_source(s) == Provider::Unknown => Err(format!(
            "unknown source {s:?}; expected one of: {}",
            KNOWN_SOURCES.join(", ")
        )),
        _ => Ok(()),
    }
}
