//! Discovery for Codex rollout files.
use crate::codex_index::{self, CodexIndex};
use crate::utils;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct CodexDiscoveredFile {
    pub fallback_session_id: String,
    pub file_path: PathBuf,
    pub archived: bool,
    pub index_metadata: Value,
}
#[derive(Debug, Clone)]
pub struct CodexDiscovery {
    pub home: PathBuf,
    pub files: Vec<CodexDiscoveredFile>,
    pub index: CodexIndex,
    pub diagnostics: Vec<String>,
}
pub fn resolve_codex_home(path: Option<&str>) -> PathBuf {
    if let Some(path) = path {
        return utils::expand_user_path(path);
    }
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".codex")
        })
}
pub fn discover_codex_rollouts(
    path: Option<&str>,
    include_archived: bool,
) -> Result<CodexDiscovery, Box<dyn std::error::Error>> {
    let requested = resolve_codex_home(path);
    if !requested.exists() {
        return Err(format!("Codex path '{}' does not exist", requested.display()).into());
    }
    let requested = requested.canonicalize()?;
    let home = infer_home(&requested);
    let index = codex_index::load_codex_index(&home);
    let mut diagnostics = index.diagnostics().to_vec();
    let mut found = Vec::new();
    let mut visited_dirs = HashSet::new();
    let mut seen_files = HashSet::new();
    if requested.is_file() {
        if is_rollout(&requested) {
            add_file(
                &requested,
                is_archived_path(&requested),
                &index,
                &mut seen_files,
                &mut found,
            );
        } else {
            diagnostics.push(format!(
                "Codex path '{}' is not a rollout JSONL file",
                requested.display()
            ));
        }
    } else {
        let name = requested.file_name().and_then(|n| n.to_str());
        if matches!(name, Some("sessions" | "archived_sessions")) {
            let archived = name == Some("archived_sessions");
            if !archived || include_archived {
                walk(
                    &requested,
                    archived,
                    &index,
                    &mut visited_dirs,
                    &mut seen_files,
                    &mut found,
                    &mut diagnostics,
                );
            }
        } else {
            walk(
                &requested.join("sessions"),
                false,
                &index,
                &mut visited_dirs,
                &mut seen_files,
                &mut found,
                &mut diagnostics,
            );
            if include_archived {
                walk(
                    &requested.join("archived_sessions"),
                    true,
                    &index,
                    &mut visited_dirs,
                    &mut seen_files,
                    &mut found,
                    &mut diagnostics,
                );
            }
        }
    }
    found.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    Ok(CodexDiscovery {
        home,
        files: found,
        index,
        diagnostics,
    })
}
fn infer_home(path: &Path) -> PathBuf {
    for ancestor in path.ancestors() {
        if matches!(
            ancestor.file_name().and_then(|n| n.to_str()),
            Some("sessions" | "archived_sessions")
        ) {
            return ancestor.parent().unwrap_or(ancestor).to_path_buf();
        }
    }
    if path.is_file() {
        path.parent().unwrap_or(path).to_path_buf()
    } else {
        path.to_path_buf()
    }
}
fn is_rollout(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
}
fn is_archived_path(path: &Path) -> bool {
    path.ancestors()
        .any(|p| p.file_name().and_then(|n| n.to_str()) == Some("archived_sessions"))
}
fn walk(
    dir: &Path,
    archived: bool,
    index: &CodexIndex,
    visited_dirs: &mut HashSet<PathBuf>,
    seen_files: &mut HashSet<PathBuf>,
    out: &mut Vec<CodexDiscoveredFile>,
    diagnostics: &mut Vec<String>,
) {
    let canonical = match dir.canonicalize() {
        Ok(path) => path,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            diagnostics.push(format!(
                "cannot inspect Codex directory '{}': {e}",
                dir.display()
            ));
            return;
        }
    };
    if !visited_dirs.insert(canonical.clone()) {
        return;
    }
    let entries = match std::fs::read_dir(&canonical) {
        Ok(entries) => entries,
        Err(e) => {
            diagnostics.push(format!(
                "cannot list Codex directory '{}': {e}",
                canonical.display()
            ));
            return;
        }
    };
    let mut entries = entries.filter_map(Result::ok).collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        match path.metadata() {
            Ok(metadata) if metadata.is_dir() => walk(
                &path,
                archived,
                index,
                visited_dirs,
                seen_files,
                out,
                diagnostics,
            ),
            Ok(metadata) if metadata.is_file() && is_rollout(&path) => {
                add_file(&path, archived, index, seen_files, out)
            }
            Ok(_) => {}
            Err(e) => diagnostics.push(format!(
                "cannot inspect Codex path '{}': {e}",
                path.display()
            )),
        }
    }
}
fn add_file(
    path: &Path,
    archived: bool,
    index: &CodexIndex,
    seen: &mut HashSet<PathBuf>,
    out: &mut Vec<CodexDiscoveredFile>,
) {
    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(_) => return,
    };
    if !seen.insert(canonical.clone()) {
        return;
    }
    let fallback_session_id = utils::fallback_session_id(&canonical);
    let mut index_metadata = index.metadata_for(&fallback_session_id);
    if let Value::Object(ref mut object) = index_metadata {
        object.insert("archived".to_string(), Value::Bool(archived));
    }
    out.push(CodexDiscoveredFile {
        fallback_session_id,
        file_path: canonical,
        archived,
        index_metadata,
    });
}
