# Codex transcript reader

`read_conversations(source := 'codex')` reads Codex CLI, Desktop, exec, editor,
browser, and subagent rollouts. `source` remains `codex`; `client`, `originator`,
and `thread_source` identify the producer. Unknown clients remain readable.

```sql
-- Uses CODEX_HOME when set, then ~/.codex.
FROM read_conversations(source := 'codex');

-- A Codex home, sessions directory, or rollout file is valid.
FROM read_conversations(path := '~/.codex/sessions', source := 'codex');

-- Archived streams are opt-in for directory discovery.
FROM read_conversations(path := '~/.codex', source := 'codex', include_archived := true);
```

Discovery deduplicates canonical file paths and skips directory cycles. The native
`session_meta.id` is the session ID; filename parsing is a fallback. The reader
enriches rollouts from `session_index.jsonl` and read-only SQLite state when
available. Missing, locked, or older indexes leave transcript rows readable and
expose diagnostics. Explicitly named rollout files are read even when archived.

`read_events` uses the same discovery and preserves malformed, unknown,
truncated, and non-UTF-8 physical records with their bytes and provenance.

The first 29 `read_conversations` columns retain their names, order, and types.
The remaining columns are appended in this order:

| Column | Type | Codex mapping |
|---|---|---|
| `source` | VARCHAR | `codex`. |
| `session_id` | VARCHAR | Native session ID, then filename fallback. |
| `project_path` | VARCHAR | Session workspace path, or NULL. |
| `project_dir` | VARCHAR | Provider-specific directory value; normally NULL for Codex. |
| `file_name` | VARCHAR | Transcript basename. |
| `is_agent` | BOOLEAN | Structured subagent or spawn-linked session. |
| `line_number` | BIGINT | 1-based physical source line. |
| `message_type` | VARCHAR | Normalized item or event kind. |
| `uuid` | VARCHAR | Native item ID verbatim, when present. |
| `parent_uuid` | VARCHAR | Native parent item or message ID, when present. |
| `timestamp` | VARCHAR | Recorded event timestamp. |
| `message_role` | VARCHAR | Actual transport role; tool output uses `tool`. |
| `message_content` | VARCHAR | Text projection; structured blocks remain in `metadata`. |
| `model` | VARCHAR | Historical turn model. |
| `tool_name` | VARCHAR | Call name or matching call name for output. |
| `tool_use_id` | VARCHAR | Native call ID. |
| `tool_input` | VARCHAR | String input verbatim; object/array serialized once. |
| `input_tokens` | BIGINT | Input tokens for the row's `usage_scope`. |
| `output_tokens` | BIGINT | Output tokens for the row's `usage_scope`. |
| `cache_creation_tokens` | BIGINT | Cache creation tokens. |
| `cache_read_tokens` | BIGINT | Cache read tokens. |
| `reasoning_tokens` | BIGINT | Reasoning tokens. |
| `slug` | VARCHAR | Title from index/state, when recorded. |
| `git_branch` | VARCHAR | Session git branch. |
| `cwd` | VARCHAR | Turn cwd, then session cwd. |
| `version` | VARCHAR | Recorded Codex version. |
| `stop_reason` | VARCHAR | Associated completion/interruption reason. |
| `reasoning_effort` | VARCHAR | Historical turn effort. |
| `repository` | VARCHAR | Recorded repository URL. |
| `record_id` | VARCHAR | Derived identity stable across moves and appends. |
| `file_path` | VARCHAR | Canonical transcript path. |
| `byte_offset` | BIGINT | Physical byte offset. |
| `ordinal` | BIGINT | Zero-based physical source-line ordinal. |
| `event_type` | VARCHAR | Native top-level rollout event type. |
| `client` | VARCHAR | CLI, Desktop, exec, editor, browser, or unknown producer. |
| `originator` | VARCHAR | Native originator label. |
| `thread_source` | VARCHAR | Native source/thread channel. |
| `parent_session_id` | VARCHAR | Parent session/thread relationship. |
| `forked_from_session_id` | VARCHAR | Fork origin. |
| `turn_id` | VARCHAR | Native turn ID. |
| `root_turn_id` | VARCHAR | Native root turn ID. |
| `response_id` | VARCHAR | Native response ID. |
| `model_provider` | VARCHAR | Recorded model provider. |
| `git_commit` | VARCHAR | Recorded git commit. |
| `agent_path` | VARCHAR | Structured subagent path. |
| `agent_nickname` | VARCHAR | Structured subagent nickname. |
| `agent_role` | VARCHAR | Structured subagent role. |
| `channel` | VARCHAR | Item channel. |
| `status` | VARCHAR | Item/terminal status; tool failure remains separate. |
| `session_created_at` | VARCHAR | Session creation timestamp. |
| `session_updated_at` | VARCHAR | Index/state update timestamp. |
| `archived` | BOOLEAN | Archive state, when known. |
| `usage_scope` | VARCHAR | Response, turn, thread total, or source-defined scope. |
| `usage_source_line` | BIGINT | Physical line supplying usage. |
| `parse_error` | VARCHAR | Parse or optional-enrichment diagnostic. |
| `raw_event` | VARCHAR | Source event text for the normalized row. |
| `metadata` | VARCHAR | Session, turn, item, index, structured content, and diagnostics JSON. |

Matching `item_completed` events enrich canonical `response_item` records.
Unmatched messages, compaction, realtime, history references, and future event
forms remain readable. Response usage attaches once to a terminal generated
item when a match is known; otherwise it has an explicit usage row. Turn and
thread totals retain their scopes and must not be summed as response usage.

For a reproducible old/new comparison against one frozen corpus, see
[`codex-comparison.sql`](codex-comparison.sql). Its source-backed denominator is
reported only where the exact source value can be checked; SQL NULL remains
NULL when source availability is unknown.
