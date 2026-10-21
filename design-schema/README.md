# Unified Claude Code + Codex transcript schema (design for review; nothing created on dev)

Small schema: a base `lines` view (file, line, a few top-level columns, `record` = the whole line as JSON),
`tool_calls` built for token and inefficiency analysis, `usage` one row per API response, and thin views for
the rest. Tool inputs and results stay JSON; parse them later.

| file | what |
|---|---|
| `schema.sql` | the views (`CREATE OR REPLACE VIEW`), run only in a disposable duckdb |
| `mapping.md` | Claude ↔ Codex concept table, source-only fields, map-shaped objects |
| `inventory.sql` → `inventory.csv` | every key path per source × record type, JSON types, fill, map flag, home column |
| `prove.sql` | materializes every view per batch of files (the proof run) |
| `checks.sql` → `checks.md` | row counts, dead columns, pairing, copies, tokens, coverage |

## Views

| view | cols | grain | notes |
|---|---:|---|---|
| `lines` | 13 | file, line | source, session_id, agent_id, type, payload_type, record_type, ts, uuid, parent_uuid, turn_id, record |
| `tool_calls` | 22 | one call | level (model/nested), session, agent, turn, ts, response_id, call_id, parent_call_id, tool_name, namespace, input + input_bytes, result + result_bytes, result_line/ts, is_error, duration_ms, result_detail |
| `usage` | 16 | one API response | response_id, request_id, model, stop_reason, total_input / cache_read / cache_write / output / reasoning tokens, turn_id |
| `copies` | 6 | one call or response copy | the only cross-file view: first_seen marks the earliest copy (sum WHERE first_seen) |
| `messages` | 9 | text block | role, text |
| `thinking` | 10 | thinking block / summary part | text (usually empty), opaque_bytes, response_id (Claude) |
| `attachments` | 11 | Claude attachment line | kind, hook_event, tool_use_id, attachment JSON, rendered, reminders (webbed `//system-reminder`) |
| `system_events` | 8 | every other line | record_type, record |
| `sessions` | 14 | file | session, agent, is_subagent, parent/fork, cwd, branch, version, title, started/ended, lines |
| helpers | | | `turn_marks` (3), `blocks` (13, Claude content blocks), `codex_items` (8, Codex payload parsed once), `lines_input` |

Token attribution: `tool_calls.response_id` → `usage.response_id` (Claude `message.id`; Codex the
`token_usage_record` written right after the call). A response that issues several calls is shared by them.

## Ingest shape (the decision: backfill once, then whole-file replace)

The file is the unit and (file, line) the key. Every view except `copies` depends on one file only, so a
modified file is replaced whole: delete its rows, re-read it, and every derived row follows. `lines_input`
is the single seam: stored closed files `UNION ALL BY NAME` a live read of files modified since the last run
(hostfs `file_last_modified`). The proof ran in 8 whole-file batches for this reason (an all-files run spilled
10 GB); derived views over a stored table would be materialized per changed file the same way.

## Open questions for Alok

1. **Names.** `tool_calls.level` 'model'/'nested'; `total_input_tokens` (cache-inclusive, comparable across
   sources); `result_detail` (Codex item JSON for matched/nested calls; Claude `toolUseResult` left in
   `lines.record` at `result_line` because it duplicates `result` and doubled the memory). Keep or rename?
2. **What gets stored.** Store only `lines` (record JSON, ~5.9 GB of text, 2.7 GB Parquet) and keep the rest as
   views, or also store `tool_calls` per file (1.1 GB Parquet) since every analysis reads it? Views over all
   files at once need batching (see above).
3. **Codex nested calls.** In current Codex the model calls `exec` (JS) 50,675 times, and the real work is
   59,357 nested items. Count both (as now, with `level`), or only model-level calls for "tokens the model
   saw"? Only `exec` output reaches the context; nested output is what the script received.
4. **Wasted-token proxy.** Per-call tokens do not exist; per call we have bytes (`input_bytes`,
   `result_bytes`) and the response's tokens. Is bytes ÷ ~4 acceptable, or should the next step attribute the
   following response's `cache_write_tokens`/`total_input_tokens` delta to the results that preceded it?
5. **is_error.** Claude sets it on 62 % of results; Codex model-level outputs have no flag (nested items do:
   status/exit_code). Derive Codex errors from output text later (FTS), or leave NULL?
6. **Copies.** 33,866 Claude call rows (33 %) are copies in resumed/forked files. Keep them as rows with
   `copies.first_seen`, or drop copies at ingest?
7. **Closed day vs live today.** With whole-file replace, a "closed day" split is unnecessary: a file is
   stored once it stops changing and re-read whole when its mtime moves. Confirm the hourly live view (files
   modified in the last hour read directly) is the only live part.
8. **Scope.** history.jsonl, plugins, jobs, agent-linkage-probe and task-handoffs (10,405 lines) are excluded
   by folder. Any of them wanted?
