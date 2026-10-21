# Claude + Codex transcripts as SQL views (design, for Alok's review)

Scope: Claude and Codex only. Nothing below has been created on dev: no views, macros or tables.
The measurements used inline SELECTs on dev and bounded files. These were 5 Claude transcripts
(5.6 MB, 1,844 lines, including one subagent file) and 2 Codex rollouts (one of them a forked child).
The Rust comparison is dev's community `agent_data` **54d875a**. The fork-main build ran only over the
`test/` fixtures.

## Verdict

For Claude and Codex, `agent_data` provides nothing that DuckDB plus `hostfs`, `read_lines`,
`json_transform` and `quackapi_post` cannot do. Every structural step the Rust reader hand-codes has a
one-clause SQL equivalent (table at the end). The views ran faster on the same files and lost no fact
the reader produces. The extension stays only for gemini, copilot, cursor and grok. It could also stay
as a convenience `read_conversations()` for people without dev, but that is a choice, not a need.

## Stage 1: `entries` (the PR #1576 queries.sql shape)

- **Discovery.** `hostfs` `ls('~/.claude/projects')` and `ls('~/.codex/sessions')`. Prune by name first
  (`memory`, `tool-results`, `file-history`, hidden). Then self-dispatch `ls` on each surviving `is_dir`
  row: project → session folder → `subagents/`, and Codex year → month → day. No `**` globs.
- **Columns.** One row per file:
  - `source`, `path`, `project_dir`
  - `session_id`: the file stem, or for a subagent the grandparent folder
  - `is_subagent`: parent folder is `subagents`
  - `parent_session_id`: the session folder above `subagents/`
  - `agent_id`: the stem after `agent-`
  - `kind`: transcript, meta.json or session_index
  - `file_size`, `modified`
  - `reader`: `read_lines`, or `read_json_objects` for meta.json and session_index
  - `sql_statement`
- **Incremental reads.** `modified` and `file_size` against the stored maximum per path. Only changed
  files get a statement.
- **Measured.** `ls` of one project folder (67 files, 977 MB) takes 8–9 ms. A `/sql` round trip takes
  4 ms, so per-file dispatch costs about 4 ms per changed file.

## Stage 2: two base views (1576's `reads`: each `sql_statement` posted through `quackapi_post`, rows back)

```sql
-- lines: the one raw relation, key (path, line_number); r = typed record, j = full JSON for the long tail
SELECT e.source, e.session_id, e.is_subagent, e.parent_session_id, e.path, l.line_number, l.byte_offset,
       json_transform(l.content, '<RECORD>') AS r,  l.content::JSON AS j
FROM read_lines('<path>') l, entries e WHERE e.path = '<path>'       -- one literal statement per file
-- blocks: key (path, line_number, block_index)
SELECT lines.*, b.block_index, b.block.type AS block_type, b.block
FROM lines, unnest(from_json(r.message.content, '<BLOCK[]>')) WITH ORDINALITY AS b(block, block_index)
```

`<RECORD>` and `<BLOCK>` are structure literals generated from `mapping.csv`. Each `json_paths` entry is
a leaf with its `type`, rendered once with tera when the view is created. They are declared rather than
inferred because inference is unstable across real files:

- `read_ndjson` found 34, 51, 60, 69 and 84 columns in the five files, and 111 with
  `union_by_name := true`.
- `message.stop_reason` was VARCHAR in some files and JSON in others.
- `toolUseResult` was a STRUCT in one file and JSON in the rest.
- `message.content` was always inferred as JSON, because it is a string or an array.

**Adding a field takes one `mapping.csv` row.** That row adds the struct leaf and the concept column.
Example: Claude API-error lines carry `error: rate_limit` and `apiErrorStatus: 429`, and the current
reader leaves `status` NULL. The fix is
`claude,assistant,usage,error,error|apiErrorStatus,VARCHAR,,row`.

Fields that are not mapped still sit in `j`. The coverage check is `json_keys(j)` minus mapped paths
minus `ignore.csv`, which should leave nothing.

## Concept views (short SELECTs over `lines` and `blocks`, same names for both sources)

| view | key | columns |
|---|---|---|
| sessions | source, session_id | parent/forked_from session, is_subagent, agent_name/role/path, title, custom_title, mode, permission_mode, pr_repository/url, channel, last_prompt, cwd, git_branch/commit, repository, version, client, thread_source, model_provider, started_at, ended_at |
| turns | session_id, turn_id | root_turn_id, model, effort, cwd, started_at, ended_at, status, last_message |
| messages | path, line_number[, block_index] | uuid, parent_uuid, role, author, prompt_source, turn_id, ts, text, phase/recipient (Codex) |
| tool_calls | tool_use_id | path, line_number, turn_id, name, input JSON, server/namespace |
| tool_results | tool_use_id | path, line_number, content, is_error/status, exit_code, result JSON (Claude `toolUseResult`) |
| thinking | path, line_number, block_index | response_id, text, redacted |
| usage | source, session_id, response_id (Claude) / usage line (Codex) | model, input, output, cache_creation, cache_read, thinking tokens, stop_reason, effort, error |
| attachments | path, line_number | kind, hook_event, tool_use_id, text, reminder = `html_extract_text(parse_html(text), '//system-reminder')` |
| system_events | path, line_number | subtype, content, payload JSON |

## Structural steps the Rust reader hand-codes, and their SQL equivalents

| step | SQL |
|---|---|
| one line becomes several rows (content blocks) | `blocks` = `unnest(from_json(...)) WITH ORDINALITY` |
| Claude usage repeated on every line of a response | `usage` keyed by `response_id` (see below) |
| Codex `event_msg` / `item_completed` copies of `response_item` | anti-join on (path, item id), or keep only `response_item` |
| Codex `token_count` belongs to the response before it | `ASOF JOIN ... usage_line > line_number` |
| subagent lineage | path parts in `entries`; `meta.json` is its own `entries` row joined on path |
| session facts and carried turn state | `last_value(... IGNORE NULLS)` windows; Codex takes the **first** `session_meta`, because forked children replay the parent's (seen in the 2026-10-13 rollouts) |
| `continued-in` (cross-session) | join on `continuedInSessionId` |

## Measurements on bounded files (dev, wall time per query, median of 3)

| | Rust `read_conversations` (54d875a) | `read_lines` + declared `json_transform` | `read_ndjson` with `union_by_name` |
|---|---|---|---|
| Claude, 5 files | 18–28 ms, 1,856 rows | **10–13 ms**, 1,844 lines | 37–49 ms, 111 columns |
| Codex, 2 files | 18–22 ms, 331 rows | **5 ms**, 368 lines | not run |

**Coverage on Claude file 028d0caf (keyed by tool_use_id / uuid):**

- All 132 tool calls and 132 tool results are present in both, with none extra.
- No uuids are missing.
- Tool names match. Inputs differ on 31 calls: 28 differ only in key order (serde_json sorts keys; the
  view keeps raw order) and 3 in nested key order.

**Coverage on Codex:**

- No call ids and no output ids are missing.
- Per-response input tokens match exactly: 2,980,508 in both.

**Finding: the current reader overcounts Claude tokens.** Claude writes one content block per line and
repeats the response's `usage` on each line. Across 3 files, 299 assistant lines covered 141 API
responses. On every line, `read_conversations` puts a copy of the response's tokens (267 rows for 124
responses in 028d0caf). `SUM(output_tokens)` gives 174,299 against 70,895 actual, which is 2.5×. The
`usage` view keyed by `response_id` fixes this.

**Memory was not measured.** Probing `custom_profiling_settings` (`SYSTEM_PEAK_BUFFER_MEMORY`) with
`EXPLAIN ANALYZE` hit a DuckDB internal error ("GetValueInternal on a value that is NULL"). That
invalidated dev, and dev was restarted. Plan: measure peak RSS per query in a disposable agent-owned
DuckDB on the same bounded file list. The `read_lines` path streams line by line, and `json_transform`
keeps only the declared leaves.

## Is the table approach right?

1. **Expressible as rows.** 131 `mapping.csv` rows across 11 concepts, plus 98 `ignore.csv` rows. The 7
   structural steps above are each one SQL clause. By `sitting_duck` function spans, the fork's
   `conversations.rs` has about 1,010 Claude lines and 1,620 Codex lines. Roughly 400 of those (15%) are
   structural; the rest is path-to-column mapping.
2. **Equivalence.** For each concept, `duck_diff table_diff` against the matching facts from
   `read_conversations`, keyed by tool_use_id, uuid, response_id or (path, line_number). Plus
   `SUMMARIZE` to show there are no dead columns. Run on fixtures and the bounded real files.
3. **Size.** Estimated:

   | | before | after |
   |---|---|---|
   | Rust for Claude + Codex | ≈ 3,500 lines (`conversations.rs` ≈ 2,630 + `types/claude.rs`, `types/codex.rs`, `codex_index.rs`, `codex_discovery.rs`) | 0 |
   | SQL (`entries`, 2 base views, 9 concept views) | | ≈ 250 lines |
   | `mapping.csv` | | 131 rows |

   Measured once built.
4. **Adding a field** takes one row (example above).
5. **Speed.** Measured above; memory is pending.

**Alternative evaluated: keep Rust, with the mapping table embedded as data.** This is the earlier plan:
an upstream refactor PR, then Codex, then Claude.

- **Reach.** The same 131 rows, plus about 380 lines of engine code.
- **Equivalence.** Same plan.
- **Size.** Net −250 lines upstream.
- **Adding a field.** One row plus a rebuild and release.
- **Speed.** Expected similar.

It loses on every axis except "works without dev". **Recommendation:** the SQL views, built after your
yes. They would go in `setup.sql` as views, and any incremental `lines` store is one table that also
needs your yes. Leave `read_conversations` in place until each concept passes its `duck_diff` coverage.
