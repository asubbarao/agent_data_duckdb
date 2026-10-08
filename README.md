# agent_data — DuckDB Extension for AI Agent Session Data

A [DuckDB extension](https://duckdb.org/community_extensions/list_of_extensions) written in Rust for querying, analysing and inspecting AI coding agents history. Read conversations, plans, todos, history, and usage stats directly from your local agent data directories — or drop to `read_events()` for the raw, lossless JSONL lines behind them.

**Supported agents:** [Claude Code](https://docs.anthropic.com/en/docs/claude-code) (`~/.claude`), Claude Desktop ("Cowork", `~/Library/Application Support/Claude`), [GitHub Copilot CLI](https://docs.github.com/en/copilot/github-copilot-in-the-cli) (`~/.copilot`), [Cursor](https://cursor.com) (`~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`, `source='cursor'`), [OpenAI Codex CLI](https://openai.com/codex) (`~/.codex`, `source='codex'`), [Gemini CLI](https://github.com/google-gemini/gemini-cli) (`~/.gemini`) and [xAI Grok CLI](https://x.ai) (`~/.grok`, `source='grok'`).

Written in 🦀 Rust.

## Quickstart

<table>
<tr>
<td>

<h4> Load Extension </h4>

<pre><code class="language-sql">INSTALL agent_data FROM community;
LOAD agent_data;
</code></pre>

<h4> Run Query</h4>

<pre><code class="language-sql">SELECT c.*, h.display
FROM read_conversations(path='~/.claude') c
JOIN read_history(path='~/.claude') h
ON c.session_id = h.session_id;
</code></pre>

</td>
<td>

<h4>Or try the <a href="https://github.com/axsaucedo/agent_data_duckdb/tree/main/examples/explorer">Streamlit Example</a></h4>

<a href="https://github.com/axsaucedo/agent_data_duckdb/tree/main/examples/explorer">
<img src="docs/streamlit.gif">
</a>

<h4>Or the <a href="https://github.com/axsaucedo/agent_data_duckdb/tree/main/examples/tui">Terminal TUI</a></h4>

<a href="https://github.com/axsaucedo/agent_data_duckdb/tree/main/examples/tui">
<img src="docs/tui.gif">
</a>

</td>
</tr>
</table>

## Overview

All functions read from the default agent directory (`~/.claude` for Claude Code, `~/.copilot` for Copilot) when no `path` is provided. 

The provider is **auto-detected** from the directory structure.

```sql
-- How many conversations have I had with Claude?
SELECT COUNT(DISTINCT session_id) AS sessions,
       COUNT(*) AS total_messages
FROM read_conversations();

-- What did I work on this week?
SELECT date, message_count, tool_call_count
FROM read_stats()
ORDER BY date DESC
LIMIT 7;

-- Which tools does github copilot use most?
SELECT tool_name, COUNT(*) AS uses
FROM read_conversations('~/.copilot')
WHERE tool_name IS NOT NULL
GROUP BY tool_name
ORDER BY uses DESC
LIMIT 10;

-- What are my active todos in my custom claude path?
SELECT content, status
FROM read_todos('~/work_folder/.claude')
WHERE status != 'completed'
ORDER BY item_index;

-- Which tools does Gemini CLI use most?
SELECT tool_name, COUNT(*) AS uses
FROM read_conversations('~/.gemini')
WHERE message_type = 'tool_call'
GROUP BY tool_name
ORDER BY uses DESC
LIMIT 10;

-- Compare activity across Claude, Copilot, and Gemini
SELECT source, COUNT(DISTINCT session_id) AS sessions, COUNT(*) AS messages
FROM (
    SELECT * FROM read_conversations(path='~/.claude')
    UNION ALL
    SELECT * FROM read_conversations(path='~/.copilot')
    UNION ALL
    SELECT * FROM read_conversations(path='~/.gemini')
)
GROUP BY source;
```

### Default Behavior

When called **without arguments**, each function reads from its provider's default path:

| Function | Default path | Detected as |
|----------|-------------|-------------|
| `read_conversations()` | `~/.claude` | Claude Code |
| `read_events()` | `~/.claude` | Claude Code |
| `read_plans()` | `~/.claude` | Claude Code |
| `read_todos()` | `~/.claude` | Claude Code |
| `read_history()` | `~/.claude` | Claude Code |
| `read_stats()` | `~/.claude` | Claude Code |

To read Claude Desktop, Copilot, or Gemini data, pass the path explicitly:

```sql
FROM read_conversations(path='~/Library/Application Support/Claude');  -- detected as Claude Desktop
FROM read_conversations(path='~/.copilot');  -- detected as Copilot
FROM read_conversations(path='~/.gemini');  -- detected as Gemini CLI
```

### Available Functions

All functions accept two optional parameters:
- **`path`** — data directory path (default: `~/.claude` for legacy no-argument calls; `source='codex'` uses `CODEX_HOME` or `~/.codex`). Auto-detected from folder structure (`local-agent-mode-sessions/` → Claude Desktop, `projects/` → Claude, `session-state/` → Copilot, a `state.vscdb` file → Cursor, `sessions/<YYYY>/` → Codex, `tmp/` + `installation_id` → Gemini CLI, `sessions/<%encoded-cwd>/` → Grok). An explicit Codex path can name its home, sessions directory, or one rollout file. Grok lives at `~/.grok`, so pass `path='~/.grok'` (or `source='grok'`).
- **`source`** — explicit provider override: `'claude'`, `'claude-desktop'`, `'copilot'`, `'cursor'`, `'codex'`, `'gemini'`, or `'grok'`. Use when auto-detection fails or for non-standard directory layouts.
- **`include_archived`** — for Codex directory discovery, include `archived_sessions/` (default `false`).

Every table includes a **`source`** column (`'claude'`, `'claude-desktop'`, `'copilot'`, `'cursor'`, `'codex'`, `'gemini'`, or `'grok'`) as the first column.

> **`read_events()` is the exception to the "empty result" convention.** It is the
> raw, lossless relation (Claude and Codex only) and it *errors* on an unusable
> path or an unsupported provider instead of returning zero rows — see
> [`read_events`](#read_eventspath-source) below.

> **Cursor** support is gated behind the default-on `cursor` cargo feature. It reads `state.vscdb` with a self-contained, pure-Rust, read-only SQLite reader (`src/vscdb.rs`) — no external dependency and no bundled C SQLite, so every target arch (including `windows_amd64_mingw`) builds with negligible size overhead. Build with `--no-default-features` to drop it. Only `read_conversations()` is implemented for Cursor; the other tables return no rows for `source='cursor'`.

> **Codex** conversation data is read from active rollouts under `CODEX_HOME` (default `~/.codex`) and, with `include_archived := true`, archived rollouts. `session_index.jsonl` and the versioned state SQLite database enrich titles, thread relationships, and other metadata; transcript reading still works if either index is unavailable. See [the Codex column mapping](docs/codex.md).

> **Grok** has no extra build dependencies. Transcripts live at
> `~/.grok/sessions/<%encoded-cwd>/<session-uuid>/chat_history.jsonl` with
> session metadata in a sibling `summary.json` (and optional `signals.json` for
> `read_stats`). Grok fills shared columns plus Grok-only `reasoning_effort` and
> `reasoning_tokens` (see field map). `encrypted_content` is never read.
> Token usage is **not** on `chat_history` lines — it comes from the sibling
> `updates.jsonl` (`sessionUpdate == "turn_completed"` → `usage`).

### `read_conversations([path (opt)], [source (opt)])`

Reads conversation/event data.
- **Claude:** JSONL files from `projects/<project>/<session>.jsonl` (including nested sub-agent transcripts at `projects/<project>/<session>/subagents/agent-*.jsonl`)
- **Claude Desktop:** JSONL files from `local-agent-mode-sessions/**/.claude/projects/<project>/<session>.jsonl` (same schema as Claude Code)
- **Copilot:** JSONL events from `session-state/<uuid>/events.jsonl`
- **Cursor:** `composerData:*` / `bubbleId:*` rows from `state.vscdb` (read with the pure-Rust `src/vscdb.rs` reader; one composer = one session)
- **Codex:** JSONL rollout streams from `sessions/<YYYY>/<MM>/<DD>/rollout-*.jsonl`
- **Gemini:** JSON chat checkpoints from `tmp/<project-hash>/chats/session-<ts>-<id>.json` (one file = one session; each tool call is also emitted as a `tool_call` row)
- **Grok:** JSONL transcripts from `sessions/<%encoded-cwd>/<session-uuid>/chat_history.jsonl`, with session metadata from sibling `summary.json`. **Timestamps** and **token** columns come from sibling `updates.jsonl` (wire event stream — chat_history itself has no time/usage fields). Subagent children linked via `…/<parent>/subagents/<child>/meta.json` set `is_agent=true` (and session-level `parent_uuid`).

| Column | Type | Description |
|--------|------|-------------|
| `source` | VARCHAR | `'claude'`, `'claude-desktop'`, `'copilot'`, `'cursor'`, `'codex'`, `'gemini'`, or `'grok'` |
| `session_id` | VARCHAR | Session UUID |
| `project_path` | VARCHAR | Project/working directory path |
| `project_dir` | VARCHAR | Raw encoded directory name (Claude / Grok cwd dir) |
| `file_name` | VARCHAR | Source filename |
| `is_agent` | BOOLEAN | Child session launched in-process by another agent: Claude `subagents/agent-*.jsonl`; Codex threads with any native parent pointer (`source.subagent.*`, `parent_thread_id`, `thread_source = subagent`); Grok via subagent meta linkage |
| `line_number` | BIGINT | Line number within file (1-based) |
| `message_type` | VARCHAR | See message type mappings below |
| `uuid` | VARCHAR | Message/event UUID |
| `parent_uuid` | VARCHAR | Parent message/event UUID (Grok: parent session id on subagent rows) |
| `timestamp` | VARCHAR | ISO 8601 timestamp (Claude/Copilot per-message; **Grok:** from `updates.jsonl` event clock, else summary session stamp) |
| `message_role` | VARCHAR | `user`, `assistant`, `tool`, or NULL |
| `message_content` | VARCHAR | Text content |
| `model` | VARCHAR | AI model used |
| `tool_name` | VARCHAR | Tool called |
| `tool_use_id` | VARCHAR | Tool use/call identifier |
| `tool_input` | VARCHAR | Tool input as JSON string |
| `input_tokens` | BIGINT | Input token count (Claude/Gemini per-message, Copilot truncation; **Grok: last `updates.jsonl` turn_completed `inputTokens`**, session aggregate duplicated on every row) |
| `output_tokens` | BIGINT | Output token count (**Grok: last turn_completed `outputTokens`**) |
| `cache_creation_tokens` | BIGINT | Cache creation tokens (Claude only; Grok always NULL) |
| `cache_read_tokens` | BIGINT | Cache read tokens (Claude; Gemini `cached`; **Grok: last turn_completed `cachedReadTokens`**) |
| `reasoning_tokens` | BIGINT | Reasoning token count (**Grok only:** last turn_completed `reasoningTokens`; other providers NULL) |
| `slug` | VARCHAR | Session slug (Claude; Grok `summary.generated_title`) |
| `git_branch` | VARCHAR | Git branch |
| `cwd` | VARCHAR | Working directory |
| `version` | VARCHAR | Agent CLI version (Grok: `summary.chat_format_version` as string) |
| `stop_reason` | VARCHAR | Claude API stop reason (NULL for Grok) |
| `reasoning_effort` | VARCHAR | Grok-only: per-message `reasoning_effort` (`low`/`medium`/`high`/…), else session-level `summary.reasoning_effort` backfill; NULL for other sources |
| `repository` | VARCHAR | GitHub repository (Copilot; Grok from `summary.git_remotes[0]`) |
| `record_id` | VARCHAR | Provider record identifier when available |
| `file_path` | VARCHAR | Transcript file path for the physical source line (Claude, Codex) |
| `byte_offset` | BIGINT | Byte offset of the physical source line (Claude, Codex) |
| `ordinal` | BIGINT | Zero-based physical line ordinal (Claude, Codex) |
| `parent_session_id` | VARCHAR | Session that launched this one, when the source records it (Claude nested subagents; Codex `parent_thread_id` / `thread_spawn`) |
| `client` | VARCHAR | Front end that wrote the session: Claude `entrypoint` (`cli`, `claude-desktop`, `sdk-cli`); Codex `cli`, `desktop`, `exec`, `editor`, `browser` |
| `agent_path` | VARCHAR | Structured child-agent path; Claude uses the nested transcript path or flat agent filename |
| `parse_error` | VARCHAR | Parser diagnostic for a retained unsupported or malformed source record |
| `raw_event` | VARCHAR | Exact valid UTF-8 JSONL line text without its terminator (Claude, Codex) |
| `author` | VARCHAR | Who wrote the row: `user`, `caller`, `agent`, `tool` or `system` (see below); never NULL |

Claude and Claude Desktop retain `file_path`, `byte_offset`, and `ordinal` on
every non-blank JSONL row, including unsupported and malformed records. For
valid UTF-8 lines, `raw_event` is the exact line text without its terminator;
invalid UTF-8 fidelity is available from `read_events()` via `raw_bytes`,
addressed by `file_path` and `byte_offset`. Such rows use
`message_type = '_parse_error'` and carry the parser diagnostic in
`parse_error`. Full reads and projections that select `raw_event` populate
that field; projections that omit it avoid retaining the line text while
preserving the other evidence fields. Nested Claude subagents derive
`parent_session_id` from `projects/<project>/<parent>/subagents/`; their
`agent_path` is `<parent>/subagents/agent-*.jsonl`. Flat legacy `agent-*.jsonl`
files use an explicit `sessionId` as `parent_session_id` when present and leave
it NULL otherwise. `session_id` keeps its existing file/native behavior.

**Who wrote the row (`author`):** `message_role` is the provider's own role, and
a subagent's brief arrives there as `user`. `author` is the derived answer:

| `author` | Meaning |
|----------|---------|
| `user` | A person typed it (Claude `promptSource` typed/queued, Desktop `turnOrigin = human`, legacy records without flags; Codex interactive threads) |
| `caller` | Written into the user slot by the program that launched the session: Claude nested subagents and `sdk-cli` (`claude -p`, `turnOrigin = sdk`) turns; Codex child threads (`is_agent`) and `codex exec` runs (`client = exec`), replayed parent prompts included |
| `agent` | Assistant text, thinking/reasoning, plans and tool calls |
| `tool` | Tool results (Claude `tool_result`; Codex `*_output` and Desktop tool items whose content is the output) |
| `system` | Harness and lifecycle records: Claude `system`, `summary`, snapshots, attachments and user-slot records flagged `isMeta`, `isCompactSummary` or `promptSource = system`; Codex `developer` messages, usage/lifecycle events, and user-slot injections wrapped in Codex's fixed envelopes (`<environment_context>`, `# AGENTS.md instructions`, `<turn_aborted>`, …) |

Copilot, Cursor, Gemini and Grok carry no launch evidence, so their `author`
follows `message_role`. A `codex exec` or `claude -p` run started by a person
from a shell is still `caller`: the transcript only shows that a program
supplied the prompt. Those shell-launched sessions have no native parent
pointer; `docs/lineage.sql` recovers the caller from the launching session's
tool call when the brief text or cwd and start time match.

**Message type mappings:**

| Claude | Copilot | Gemini | Grok | Description |
|--------|---------|--------|------|-------------|
| `user` | `user` | `user` | `user` | User message |
| `assistant` | `assistant` | `assistant` (from `gemini`) | `assistant` | Assistant response |
| `system` | — | — | `system` | System prompt |
| `summary` | — | — | — | Conversation summary |
| — | `reasoning` | — | `reasoning` | Assistant reasoning (summary text only) |
| — | `turn_start` / `turn_end` | — | — | Assistant turn boundaries |
| — | `tool_start` / `tool_result` | `tool_call` | `tool_call` / `tool_result` | Tool execution events |
| — | `session_start` / `session_resume` | — | — | Session lifecycle |
| — | `session_info` / `session_error` | `info` / `error` | — | Session info/errors |
| — | `truncation` / `model_change` | — | — | Context management |
| — | `compaction_start` / `compaction_complete` | — | — | Context compaction |
| — | `abort` | — | — | User cancellation |

> **Gemini tool calls:** each assistant (`gemini`) turn may embed multiple
> `toolCalls`. The first is surfaced inline on the assistant row (`tool_name` /
> `tool_use_id` / `tool_input`), and every call additionally gets its own
> `tool_call` row whose `parent_uuid` links back to the assistant message and
> whose `message_content` holds the call status (`success` / `error` / `cancelled`).

> **Grok field map:**
>
> | Grok source | Column | Notes |
> |-------------|--------|--------|
> | `type` | `message_type` | `system` / `user` / `reasoning` / `assistant` / `tool_result`; each `tool_calls[]` → `tool_call` |
> | `content` / `summary[].text` | `message_content` | User may be string or `{type,text}` blocks; reasoning uses summary text only (never `encrypted_content`) |
> | `model_id` (else `summary.current_model_id`) | `model` | |
> | `tool_calls[].name/id/arguments` | `tool_name` / `tool_use_id` / `tool_input` | One row per call; string or object args → stable string |
> | `reasoning_effort` (message, else summary) | `reasoning_effort` | Grok-only nullable varchar; `stop_reason` stays NULL |
> | `summary.generated_title` | `slug` | Session title |
> | `summary.chat_format_version` | `version` | Stringified |
> | `updates.jsonl` `timestamp` (+ kind) | `timestamp` | Unix sec → ISO; cursor-aligned to chat types (`user_message_chunk`→user, `agent_thought_chunk`→reasoning, …). Fallback: summary activity stamp |
> | `summary.head_branch` / `git_remotes[0]` / `git_root_dir` | `git_branch` / `repository` / `project_path` | |
> | `reasoning.id`, else synthetic | `uuid` | Prefer real id; else `{session_id}:{line_number}` so uuid is never NULL |
> | subagent `meta.json` | `is_agent` / `parent_uuid` | Child session → true; parent session id |
> | `updates.jsonl` `turn_completed.usage.inputTokens` | `input_tokens` | Last usable snapshot; **session/prompt aggregate duplicated on every row** (not per-line) |
> | `…outputTokens` | `output_tokens` | same |
> | `…cachedReadTokens` | `cache_read_tokens` | same |
> | `…reasoningTokens` | `reasoning_tokens` | Grok-only column; other providers NULL |
>
> **Token source note:** Usage is cumulative within one user-prompt agent loop
> (`numTurns` 1→N then resets). v1 stamps the **last** `turn_completed` usage
> in `updates.jsonl` onto all conversation rows for that session. Do not sum
> token columns across rows for a session — pick any row (or `MAX`/`ANY_VALUE`).
> Sessions without `updates.jsonl` (or without `usage`) keep token columns NULL.
> `cache_creation_tokens` is never set for Grok.
>
> **Synthetic uuid form:** `{session_id}:{line_number}` (1-based chat_history line).
> Multi-row fan-out from one assistant line (text + tool_call rows) shares that
> line's uuid. Prefer real `reasoning.id` when present.
>
> **Timestamps:** `chat_history` has no time fields. The CLI clocks live in
> `updates.jsonl`. The parser walks chat lines and assigns the next matching
> wire event's ISO time (same `timestamp` column as Claude). No `updates.jsonl`
> → summary session stamp only.

### `read_events([path (opt)], [source (opt)])`

The **lossless raw JSONL relation**: one row per *physical line* of every
transcript file, exactly as it sits on disk. Where `read_conversations()`
normalizes seven providers into one shared schema (and therefore drops whatever
it has no column for), `read_events()` drops nothing — unknown event types,
malformed JSON, blank lines and a half-written final line all come back as rows,
addressed by `file_path` + `line_number` + `byte_offset`.

- **Claude** (`source='claude'`): `projects/<project>/<session>.jsonl`, including nested sub-agent transcripts at `projects/<project>/<session>/subagents/agent-*.jsonl` (same discovery walk as `read_conversations()`)
- **Codex** (`source='codex'`): `sessions/<YYYY>/<MM>/<DD>/rollout-*.jsonl`

Other providers are **not** supported by `read_events()` and raise an error
(use `read_conversations()` for those).

| Column | Type | Description |
|--------|------|-------------|
| `source` | VARCHAR | `'claude'` or `'codex'` |
| `session_id` | VARCHAR | File-derived session id (Claude: file stem or parent session directory for subagents; Codex: rollout filename UUID). Event-level ids remain in `raw`; use file/line provenance when they differ. |
| `file_path` | VARCHAR | Absolute path of the transcript file the line came from |
| `file_name` | VARCHAR | File name only |
| `line_number` | BIGINT | 1-based **physical** line number in that file (counts blank and malformed lines) |
| `byte_offset` | BIGINT | Byte offset of the line's first byte from the start of the file |
| `byte_length` | BIGINT | Length of the line in **bytes**, excluding its terminator |
| `line_ending` | VARCHAR | `'LF'`, `'CRLF'`, or NULL when the line has no terminator (final, possibly partial, line) |
| `raw` | VARCHAR | The line verbatim, terminator excluded — nothing parsed, reordered or re-serialized |
| `raw_is_exact` | BOOLEAN | `false` if invalid UTF-8 needed U+FFFD in the text representation; `raw_bytes` always retains the original bytes |
| `is_valid_json` | BOOLEAN | Whether the line parses as JSON |
| `parse_error` | VARCHAR | The JSON parser's own message when it does not, else NULL |
| `event_type` | VARCHAR | Top-level `"type"` **verbatim** — no mapping table, so future/unknown types pass through (NULL if absent or non-string) |
| `timestamp` | VARCHAR | Top-level `"timestamp"` verbatim (NULL if absent or non-string) |
| `raw_bytes` | BLOB | Exact line bytes, excluding the separately recorded terminator; authoritative even for invalid UTF-8 or embedded NUL |

`event_type` and `timestamp` are conveniences, not a schema: everything else
stays in `raw`, so nested payloads are queried from there (e.g. with the `json`
extension).

**Newline handling.** `\n` (reported as `LF`) and `\r\n` (`CRLF`) terminate a
line; the terminator is excluded from `raw` and from `byte_length` and recorded
in `line_ending`. A bare `\r` is **not** a terminator and stays inside `raw`. An
empty line is a row with `raw = ''` and `byte_length = 0`. A file whose last
line has no terminator — an agent still writing, or a truncated log — yields a
final row with `line_ending IS NULL`, kept whether or not it parses. Because
nothing is normalized in `raw_bytes`, it and `line_ending` reconstruct the original bytes.
For UTF-8 logs (`raw_is_exact` is true on every row), the text form is:

```sql
SELECT string_agg(
           raw || CASE line_ending WHEN 'CRLF' THEN chr(13) || chr(10)
                                   WHEN 'LF'   THEN chr(10)
                                   ELSE '' END,
           '' ORDER BY line_number)
FROM read_events(path='~/.claude')
WHERE file_path = '...';   -- byte-identical to the file
```

**Errors.** A raw reader that silently returns nothing is indistinguishable from
a lossless read of an empty directory, so `read_events()` fails loudly:

| Condition | Behavior |
|-----------|----------|
| `path` does not exist / is not a directory | error (`read_events: path '…' does not exist`) |
| provider is not Claude or Codex (explicit `source` or auto-detected) | error (`read_events: unsupported provider …`) |
| a transcript or discovery directory cannot be read | error naming the path and the OS error |
| supported provider, no transcripts found | **0 rows** (an empty tree is a legitimate answer) |

```sql
-- Which raw lines does the normalized view not account for?
SELECT file_name, line_number, event_type, parse_error, raw
FROM read_events(path='~/.codex', source='codex')
WHERE NOT is_valid_json OR event_type NOT IN ('session_meta', 'turn_context', 'response_item', 'event_msg');

-- Re-read one exact line from disk by its address
SELECT file_path, byte_offset, byte_length, raw
FROM read_events(path='~/.claude')
WHERE session_id = '…' AND line_number = 42;
```

### `read_plans([path], [source])`

Reads plan files.
- **Claude:** `plans/*.md`
- **Copilot:** `session-state/<uuid>/plan.md`
- **Codex:** no standalone plan files exist (plans live inline in the rollout
  stream as `update_plan` tool calls); `source='codex'` returns zero rows with
  this schema.

| Column | Type | Description |
|--------|------|-------------|
| `source` | VARCHAR | `'claude'`, `'claude-desktop'`, or `'copilot'` |
| `session_id` | VARCHAR | Parent session UUID (Copilot only, NULL for Claude) |
| `plan_name` | VARCHAR | Plan name (filename stem or workspace summary) |
| `file_name` | VARCHAR | Full filename |
| `file_path` | VARCHAR | Absolute file path |
| `content` | VARCHAR | Full markdown content |
| `file_size` | BIGINT | File size in bytes |

### `read_todos([path], [source])`

Reads todo/checklist items.
- **Claude:** `todos/<session>-agent-<agent>.json`
- **Copilot:** Checkpoint markdown checklists from `session-state/<uuid>/checkpoints/*.md`
- **Codex:** no `todos/` store exists; `source='codex'` returns zero rows with
  this schema.

| Column | Type | Description |
|--------|------|-------------|
| `source` | VARCHAR | `'claude'`, `'claude-desktop'`, or `'copilot'` |
| `session_id` | VARCHAR | Parent session UUID |
| `agent_id` | VARCHAR | Agent UUID (Claude only, NULL for Copilot) |
| `file_name` | VARCHAR | Source filename |
| `item_index` | BIGINT | 0-based index (-1 for parse errors) |
| `content` | VARCHAR | Todo item text |
| `status` | VARCHAR | `pending`, `in_progress`, `completed`, or `_parse_error` |
| `active_form` | VARCHAR | Active form description (Claude only) |

### `read_history([path], [source])`

Reads command history.
- **Claude:** `history.jsonl` (structured JSONL)
- **Copilot:** `command-history-state.json` (simple string array)
- **Codex:** `<CODEX_HOME>/history.jsonl` (default `~/.codex`; `path` may also
  name the file). Each line is `{session_id, ts, text}`: `ts` (Unix seconds) →
  `timestamp_ms` (× 1000), `session_id` → `session_id` (the rollout thread id,
  joinable to `read_conversations`), `text` → `display`. Codex records no
  project or pasted content here, so `project` and `pasted_contents` are NULL
  (join `read_conversations().cwd` on `session_id` for the project).
  A malformed line becomes a row whose `display` starts with `Parse error:`.

| Column | Type | Description |
|--------|------|-------------|
| `source` | VARCHAR | `'claude'`, `'claude-desktop'`, `'copilot'`, or `'codex'` |
| `line_number` | BIGINT | Line/entry number (1-based) |
| `timestamp_ms` | BIGINT | Unix timestamp in ms (Claude, Codex) |
| `project` | VARCHAR | Project path (Claude only) |
| `session_id` | VARCHAR | Session UUID (Claude, Codex) |
| `display` | VARCHAR | Command/prompt text |
| `pasted_contents` | VARCHAR | Pasted content as JSON (Claude only) |

### `read_stats([path], [source])`

Reads daily activity stats.
- **Claude:** `stats-cache.json` daily activity
- **Grok:** rolls up per-session `signals.json` (+ `summary` fallbacks) by
  `summary.created_at` date into the same columns (no new table function).
  `message_count` = `userMessageCount + assistantMessageCount` (else
  `summary.num_messages`); `tool_call_count` = `signals.toolCallCount`;
  `session_count` = sessions on that date.
- **Codex:** rolls up active rollouts (the rows `read_conversations` emits) by
  the UTC date of each row's timestamp. `message_count` = `user`, `assistant`
  and `agent_message` rows whose `author` is not `system` (harness envelopes
  such as `<environment_context>` are excluded; event_msg copies are already
  de-duplicated by the loader); `tool_call_count` = `function_call`,
  `custom_tool_call`, `local_shell_call` and `web_search_call` rows;
  `session_count` = sessions on the date of their first timestamped row.
  Archived rollouts are not included.

Other providers return empty (derive from `read_conversations()` in SQL instead).

| Column | Type | Description |
|--------|------|-------------|
| `source` | VARCHAR | `'claude'`, `'grok'`, or `'codex'` |
| `date` | VARCHAR | Date (YYYY-MM-DD) |
| `message_count` | BIGINT | Messages sent that day |
| `session_count` | BIGINT | Sessions started that day |
| `tool_call_count` | BIGINT | Tool calls made that day |

## Provider Detection

The extension auto-detects the data source by examining the directory structure:
- **Claude Desktop:** contains `local-agent-mode-sessions/` directory
- **Claude:** contains `projects/` directory
- **Copilot:** contains `session-state/` directory
- **Gemini CLI:** contains a `tmp/` directory plus an `installation_id` file
- **Unknown:** returns empty results (or use `source` parameter to force)

```sql
-- Auto-detect
FROM read_conversations(path='~/.claude');   -- detected as Claude
FROM read_conversations(path='~/Library/Application Support/Claude');  -- detected as Claude Desktop
FROM read_conversations(path='~/.copilot');  -- detected as Copilot
FROM read_conversations(path='~/.gemini');   -- detected as Gemini CLI

-- Override detection
FROM read_conversations(path='custom/dir', source='gemini');
```

## Join Keys

Tables can be joined within the same source:

```sql
-- Conversations ↔ History (via session_id)
SELECT c.*, h.display
FROM read_conversations(path='~/.claude') c
JOIN read_history(path='~/.claude') h ON c.session_id = h.session_id;

-- Cross-source: always filter by source
SELECT * FROM (
    SELECT * FROM read_conversations(path='~/.claude')
    UNION ALL
    SELECT * FROM read_conversations(path='~/.copilot')
) WHERE source = 'copilot';
```

| Join | Left Key | Right Key | Notes |
|------|----------|-----------|-------|
| conversations ↔ history | `session_id` | `session_id` | Same source only |
| conversations ↔ todos | `session_id` | `session_id` | Same source only |
| conversations ↔ plans | `slug` | `plan_name` | Claude only |
| conversations ↔ history | `project_path` | `project` | Claude only |

## Parse Error Policy

When a JSONL line or JSON file cannot be parsed, the extension emits a row with:
- `message_type = '_parse_error'` (conversations)
- `status = '_parse_error'` (todos)
- `display = 'Parse error: ...'` (history)

Filter them with `WHERE message_type != '_parse_error'`.

`read_events()` does not summarize parse failures this way: the offending line
is returned verbatim in `raw` with `is_valid_json = false` and the parser's own
message in `parse_error`, so the bytes that failed are still inspectable.

## Examples

### Terminal TUI (Agent Chronicle)

Keyboard-driven terminal explorer with session browser, overview dashboard, and SQL editor:

```bash
cd examples/tui
uv sync
uv run python -m agent_chronicle
```

See [examples/tui/README.md](examples/tui/README.md) for details.

### Marimo Notebook

Interactive notebook for exploring agent data:

```bash
cd examples/marimo
uv sync
marimo edit explore.py
```

### Streamlit Explorer

Multi-page web application with session browser and SQL query interface:

```bash
cd examples/explorer
uv sync
streamlit run app.py
```

See [examples/explorer/README.md](examples/explorer/README.md) for details.

## Testing

```bash
# Build and run all SQLLogicTest assertions
make test
```

437 pinned assertions across 21 test files covering row counts, column validation, cross-source queries, join invariants, edge cases, Grok stats, and parse error handling.

## Building from Source

```bash
# First time: configure build environment
make configure

# Build debug extension
make debug

# Run tests
make test
```

The compiled extension is at `build/debug/agent_data.duckdb_extension` (or `build/release/` for `make release`).

```bash
# Load directly from a local build
duckdb -unsigned -c "LOAD 'build/debug/agent_data.duckdb_extension'; FROM read_conversations();"
```

## Community Extension Release Checklist

When DuckDB publishes a new stable release, keep the community package aligned with
the latest stable target:

1. Check whether the repo is still aligned with DuckDB latest stable:

   ```bash
   python3 scripts/update_duckdb_release.py --check
   ```

2. If a new stable release exists, update all local release surfaces:

   ```bash
   python3 scripts/update_duckdb_release.py --apply
   ```

   This updates `duckdb-release.toml`, `Cargo.toml`, `Cargo.lock`,
   `Makefile`, `.github/workflows/MainDistributionPipeline.yml`, and
   example DuckDB Python constraints.

3. Run the full local validation entrypoint:

   ```bash
   scripts/validate_duckdb_release.sh
   ```

   This runs Rust/build/test checks and an exact-version DuckDB Python smoke
   test that loads `build/debug/agent_data.duckdb_extension`.

4. Update `duckdb/community-extensions` after the source commit is validated:

   ```bash
   scripts/prepare_community_extension_pr.py --open-pr
   ```

   Keep upstream `repo.ref` immutable. A commit SHA is preferred; an immutable
   tag is also acceptable when maintainers intentionally release by tag. Do not
   use `main` as the stable ref because community builds should be reproducible.

5. After the upstream community-extension workflow deploys from a trusted
   context, verify publication:

   ```bash
   uv run --with duckdb==$(python3 - <<'PY'
import tomllib
print(tomllib.load(open("duckdb-release.toml", "rb"))["duckdb"]["python_version"])
PY
) python scripts/verify_community_publication.py
   ```

   Pull-request deploys may build artifacts without publishing public binaries
   when upstream secrets are unavailable.

The scheduled `DuckDB Release Monitor` workflow checks for stable-release drift
and opens a PR when the updater and validation pass. The `DuckDB Next
Compatibility` workflow can be run on schedule or manually to test DuckDB
`main`/next without publishing binaries. For DuckDB feature-freeze branches,
use upstream `repo.ref_next` for pre-release validation, then update `repo.ref`
after the stable source commit is merged.

## License

MIT — see [LICENSE](LICENSE).
