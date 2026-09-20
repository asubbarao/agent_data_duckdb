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

/// Parse an explicit source string into a Provider.
///
/// Separators are normalised, so `claude_desktop`, `claude-desktop` and
/// `claude desktop` all name the same provider. The underscore spelling is the
/// one callers reach for, because every function this extension exposes is
/// named with underscores, and treating it as unknown was worse than useless:
/// `resolve_provider` fell through to auto-detection and quietly returned some
/// other agent's transcripts.
pub fn parse_source(source: &str) -> Provider {
    let normalised: String = source
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c == '_' || c == ' ' { '-' } else { c })
        .collect();
    match normalised.as_str() {
        "claude" | "claude-code" | "claudecode" => Provider::Claude,
        "claude-desktop" | "claudedesktop" | "cowork" => Provider::ClaudeDesktop,
        "copilot" | "github-copilot" => Provider::Copilot,
        "cursor" => Provider::Cursor,
        "codex" => Provider::Codex,
        "gemini" => Provider::Gemini,
        "grok" => Provider::Grok,
        _ => Provider::Unknown,
    }
}

/// Resolve provider: explicit source overrides auto-detection.
///
/// An explicit source that cannot be parsed is *not* silently replaced by
/// auto-detection. Asking for one agent and receiving another's history looks
/// like a working query, so the caller gets `Unknown` — an empty result — and
/// `validate_source` turns it into a bind-time error before the query runs.
pub fn resolve_provider(path: &Path, source: Option<&str>) -> Provider {
    match source {
        Some(s) => parse_source(s),
        None => detect_provider(path),
    }
}

/// Reject an explicit `source` that names no known provider.
///
/// Called from bind so the query fails immediately with the list of valid
/// names, rather than succeeding with zero rows and leaving the caller to
/// guess whether they have no data or the wrong spelling.
pub fn validate_source(source: Option<&str>) -> Result<(), String> {
    match source {
        Some(s) if parse_source(s) == Provider::Unknown => Err(format!(
            "unknown source {s:?}; expected one of: {}",
            KNOWN_SOURCES.join(", ")
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_separators_are_normalised() {
        // The underscore spelling is what callers type, because every function
        // this extension exposes is named with underscores.
        for s in ["claude_desktop", "claude-desktop", "claude desktop", "CLAUDE_DESKTOP", "  claude-desktop  "] {
            assert_eq!(parse_source(s), Provider::ClaudeDesktop, "failed for {s:?}");
        }
        assert_eq!(parse_source("claude_code"), Provider::Claude);
        assert_eq!(parse_source("github_copilot"), Provider::Copilot);
        assert_eq!(parse_source("cowork"), Provider::ClaudeDesktop);
    }

    #[test]
    fn unknown_source_is_unknown_not_a_silent_fallback() {
        assert_eq!(parse_source("nonsense"), Provider::Unknown);
        // The whole point of the change: an explicit source is never quietly
        // replaced by whatever the directory happens to look like.
        let desktop_like = Path::new("/nonexistent/does-not-matter");
        assert_eq!(resolve_provider(desktop_like, Some("nonsense")), Provider::Unknown);
    }

    #[test]
    fn explicit_source_wins_over_detection() {
        // No filesystem access needed: detection on a missing path yields
        // Unknown, so an explicit source is the only thing that can decide.
        let p = Path::new("/nonexistent");
        assert_eq!(resolve_provider(p, Some("codex")), Provider::Codex);
        assert_eq!(resolve_provider(p, None), Provider::Unknown);
    }

    #[test]
    fn validate_source_rejects_only_unknown_names() {
        assert!(validate_source(None).is_ok());
        assert!(validate_source(Some("claude_desktop")).is_ok());
        let err = validate_source(Some("nonsense")).unwrap_err();
        assert!(err.contains("unknown source"), "{err}");
        // The message has to say what the caller should have written.
        assert!(err.contains("claude-desktop"), "{err}");
    }
}
