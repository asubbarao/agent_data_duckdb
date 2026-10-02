//! Best-effort metadata enrichment from Codex's JSONL index and SQLite state.
use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
#[derive(Debug, Clone, Default)]
pub struct CodexIndex {
    metadata: HashMap<String, Value>,
    diagnostics: Vec<String>,
}
impl CodexIndex {
    pub fn metadata_for(&self, id: &str) -> Value {
        self.metadata
            .get(id)
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()))
    }
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
}
pub fn load_codex_index(home: &Path) -> CodexIndex {
    let mut index = CodexIndex::default();
    load_session_index(home, &mut index);
    for db in state_databases(home) {
        load_sqlite(&db, &mut index);
    }
    index
}
fn load_session_index(home: &Path, index: &mut CodexIndex) {
    let path = home.join("session_index.jsonl");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            index.diagnostics.push(format!(
                "cannot read Codex session index '{}': {e}",
                path.display()
            ));
            return;
        }
    };
    for (number, raw) in text.lines().enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(raw) {
            Ok(Value::Object(mut record)) => match record
                .get("id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
            {
                Some(id) => {
                    if let Some(name) = record.get("thread_name").cloned() {
                        record.entry("title".to_string()).or_insert(name);
                    }
                    merge(
                        index
                            .metadata
                            .entry(id)
                            .or_insert_with(|| Value::Object(Map::new())),
                        Value::Object(record),
                    );
                }
                None => index.diagnostics.push(format!(
                    "session_index.jsonl:{} has no string id",
                    number + 1
                )),
            },
            Ok(_) => index.diagnostics.push(format!(
                "session_index.jsonl:{} is not an object",
                number + 1
            )),
            Err(e) => index
                .diagnostics
                .push(format!("session_index.jsonl:{}: {e}", number + 1)),
        }
    }
}
fn state_databases(home: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for dir in [home.to_path_buf(), home.join("sqlite")] {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("sqlite" | "db")
                )
            {
                paths.push(path);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}
fn load_sqlite(path: &Path, index: &mut CodexIndex) {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = match Connection::open_with_flags(path, flags) {
        Ok(conn) => conn,
        Err(e) => {
            index.diagnostics.push(format!(
                "Codex SQLite enrichment unavailable for '{}': {e}",
                path.display()
            ));
            return;
        }
    };
    let _ = conn.busy_timeout(std::time::Duration::from_millis(250));
    if let Err(e) = conn.execute_batch("PRAGMA query_only = ON; BEGIN;") {
        index.diagnostics.push(format!(
            "Codex SQLite enrichment unavailable for '{}': {e}",
            path.display()
        ));
        return;
    }
    match table_exists(&conn, "threads") {
        Ok(true) => {}
        Ok(false) => {
            index.diagnostics.push(format!(
                "Codex SQLite '{}' has no threads table",
                path.display()
            ));
            return;
        }
        Err(e) => {
            index.diagnostics.push(format!(
                "Codex SQLite enrichment unavailable for '{}': {e}",
                path.display()
            ));
            return;
        }
    }
    let columns = columns(&conn, "threads");
    if !columns.iter().any(|column| column == "id") {
        index.diagnostics.push(format!(
            "Codex SQLite '{}' threads table has no id column",
            path.display()
        ));
        return;
    }
    let mut statement = match conn.prepare("SELECT * FROM threads") {
        Ok(statement) => statement,
        Err(e) => {
            index.diagnostics.push(format!(
                "cannot read Codex threads from '{}': {e}",
                path.display()
            ));
            return;
        }
    };
    let names = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    let rows = match statement.query_map([], |row| {
        let mut object = Map::new();
        for (i, name) in names.iter().enumerate() {
            object.insert(name.clone(), sqlite_value(row.get_ref(i)?));
        }
        Ok(object)
    }) {
        Ok(rows) => rows,
        Err(e) => {
            index.diagnostics.push(format!(
                "cannot query Codex threads from '{}': {e}",
                path.display()
            ));
            return;
        }
    };
    for row in rows {
        match row {
            Ok(mut object) => {
                if let Some(id) = object
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                {
                    if object.get("title").map_or(true, Value::is_null) {
                        if let Some(name) = object.get("thread_name").cloned() {
                            object.insert("title".to_string(), name);
                        }
                    }
                    object.insert(
                        "_sqlite_path".to_string(),
                        Value::String(path.to_string_lossy().into_owned()),
                    );
                    merge(
                        index
                            .metadata
                            .entry(id)
                            .or_insert_with(|| Value::Object(Map::new())),
                        Value::Object(object),
                    );
                }
            }
            Err(e) => index.diagnostics.push(format!(
                "cannot decode Codex thread from '{}': {e}",
                path.display()
            )),
        }
    }
}
fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |_| Ok(()),
    )
    .map(|_| true)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(false),
        _ => Err(e),
    })
}
fn columns(conn: &Connection, table: &str) -> Vec<String> {
    let escaped = table.replace('"', "\"\"");
    let Ok(mut statement) = conn.prepare(&format!("PRAGMA table_info(\"{escaped}\")")) else {
        return Vec::new();
    };
    statement
        .query_map([], |row| row.get::<_, String>(1))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default()
}
fn sqlite_value(value: ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(v) => Value::from(v),
        ValueRef::Real(v) => Value::from(v),
        ValueRef::Text(v) => String::from_utf8_lossy(v).into_owned().into(),
        ValueRef::Blob(v) => Value::Array(v.iter().map(|b| Value::from(*b)).collect()),
    }
}
fn merge(target: &mut Value, source: Value) {
    let (Value::Object(target), Value::Object(source)) = (target, source) else {
        return;
    };
    for (key, value) in source {
        if !value.is_null() {
            target.insert(key, value);
        }
    }
}
