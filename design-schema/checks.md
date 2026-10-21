# Checks (numbers from `checks.sql` over the `prove.sql` output, 2026-10-14)

Input: the self-dispatch reader's Parquet, `scope <> 'bounded'` (the bounded run is a copy of the same
files): 1,155,760 lines. The views keep transcript folders only: 604,489 Claude lines (1,044 files) +
540,866 Codex lines (827 files) = 1,145,355. The other 10,405 lines are `history.jsonl`, plugins, jobs,
agent-linkage-probe, session_index, task-handoffs, state (not transcripts).

Run: disposable local duckdb 1.5.6, `threads 2, memory_limit 3GB`, 8 batches of whole files
(`hash(file) % 8`), 17–20 s each, peak RSS 3.5–4.6 GB. A single all-files run spilled past 10 GB of temp and
reached 10 GB RSS, so the proof runs per file batch; that is exact because every view except `copies`
depends on one file only (the same property whole-file replace needs).

## 1. Rows per view

| view | source | rows | files |
|---|---|---:|---:|
| lines | claude | 604,489 | 1,044 |
| lines | codex | 540,866 | 827 |
| tool_calls (model) | claude | 101,801 | 1,029 |
| tool_calls (model) | codex | 58,467 | 751 |
| tool_calls (nested) | codex | 59,357 | 750 |
| usage | claude | 80,087 | 1,038 |
| usage | codex | 60,871 | 799 |
| messages | claude | 36,038 | 1,044 |
| messages | codex | 61,092 | 824 |
| thinking | claude | 67,295 | 1,035 |
| thinking | codex | 64,342 | 752 |
| attachments | claude | 156,712 | 1,038 |
| system_events | claude | 140,924 | 118 |
| system_events | codex | 23,671 | 827 |
| sessions | claude / codex | 1,044 / 827 | |

## 2. Dead columns (SUMMARIZE over `nullif(trim(col::VARCHAR), '')`)

No column is 100 % NULL. One was, and was removed: `system_events.agent_id` (0 %). Sparse but live
(null %): `tool_calls.parent_call_id` 73.0 (only nested rows), `namespace` 69.7, `result_detail` 71.7,
`is_error` 43.2, `agent_id` 73.7; `usage.stop_reason` 58.0 (Claude only), `request_id` 43.2 (Claude only),
`reasoning_tokens` 14.9; `thinking.text` 92.6 (redacted/encrypted); `sessions.title` 95.4,
`forked_from_id` 91.8, `parent_session_id` 85.0; `attachments.hook_event` 85.9, `tool_use_id` 85.2.
Full table: run `checks.sql` §2.

## 3. Tool call ↔ result pairing (same file, same id, next result after the call)

| source | level | calls | paired | % | with response_id | with turn_id | with is_error | input bytes | result bytes |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| claude | model | 101,801 | 101,795 | 99.99 | 101,801 | 101,532 | 63,044 | 95.2 MB | 387.1 MB |
| codex | model | 58,467 | 58,457 | 99.98 | 58,467 | 58,467 | 2,458 | 43.7 MB | 361.2 MB |
| codex | nested | 59,357 | 59,357 | 100 | 59,356 | 59,357 | 59,357 | 50.8 MB | 590.4 MB |

Unpaired: 6 Claude and 10 Codex calls (sessions cut off mid-call). An earlier equi-join gave 128,003
Claude rows: 9,177 (file, call id) pairs occur twice in the same file, so pairing is an ASOF to the next
result. Codex nested results are execution output inside `exec`; only the `exec` output (model level) enters
the model's context.

## 4. Copies across files (`copies` view)

| kind | source | rows | first_seen | copies | files with copies |
|---|---|---:|---:|---:|---:|
| response | claude | 80,087 | 58,303 | 21,784 | 266 |
| tool_call | claude | 101,801 | 67,935 | 33,866 | 265 |
| response | codex | 60,838 | 60,838 | 0 | 0 |
| tool_call | codex | 117,824 | 117,824 | 0 | 0 |

Claude resumed/forked sessions copy earlier lines into the new file; Codex forks (157 `forked_from_id`)
did not repeat any call id or response id in these files.

## 5. Tokens (Claude one row per response; Codex one row per token_usage_record)

| source | first_seen | responses | total input | cache read | cache write | output | reasoning |
|---|---|---:|---:|---:|---:|---:|---:|
| claude | true | 58,303 | 15,829,446,542 | 15,468,385,598 | 360,792,280 | 36,508,189 | 13,643,548 |
| claude | false (copies) | 21,784 | 6,527,547,754 | 6,392,785,720 | 134,643,622 | 17,360,635 | 6,628,694 |
| codex | true | 60,871 | 7,213,011,526 | 7,003,385,088 | 0 | 24,179,779 | 9,668,697 |

Claude output tokens, first copy only: 36.5 M (self-dispatch RESULTS.md: 36.4 M by message.id across
files; `read_conversations` sums 128.3 M, 3.5×). Codex: 60,838 rows from `token_usage_record`
(770 files), 33 from the `token_count` `last_token_usage` fallback (29 older files).

## 6. Inventory coverage (`inventory.csv`)

| source | rows (record type × path × types) | distinct paths | promoted to a view column | kept in `lines.record` | unaccounted |
|---|---:|---:|---:|---:|---:|
| claude | 3,348 | 2,011 | 840 | 1,171 | 0 |
| codex | 1,356 | 888 | 245 | 643 | 0 |

"Promoted" counts subtree paths under a JSON column (e.g. 321 Claude tool-input keys live in
`tool_calls.input`). Map-shaped objects are collapsed to `<path>.{*}` (11 maps; Claude raw had 231,039
distinct paths before collapsing, 67,926 of them tool ids under `wireToolInputs`). Homes are assigned by
path, not record type: `$.payload.item` points at `tool_calls`, but Reasoning/AgentMessage/UserMessage items
are read by no view and stay in `lines.record`.
