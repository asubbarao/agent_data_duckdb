# Ingest first, schema later: RawDuck and duckorch on Claude + Codex transcripts

An alternative to hand-declared views. All runs used disposable files under
`/private/tmp/claude-501/agent_data_alt/`, with `threads=4` and `memory_limit='4GB'`. A watchdog killed any
run whose RSS passed 6 GB. Dev was used read-only, for `read_conversations` (agent_data 54d875a).
Extensions: rawduck a14e474, duckorch d9bc95f (0.3.0), duck_diff 68ace70; DuckDB 1.5.6.

Files:
- `ingest.sql`: the program, one for both loaders.
- `coverage.sql`: comparison with `read_conversations`.
- `queries.sql`: the five ergonomics questions.

## Verdict

**Neither tool is better than hand-declared views for these transcripts, and neither lets you stop
maintaining the Rust on its own.**

- **RawDuck is a good raw "lines" layer, but only on small inputs.**
  - It types every field with no field list.
  - It agreed with `read_conversations` on 100% of Claude uuids.
  - It read `.jsonl.zst` natively.
  - The five questions take 41 lines of SQL.
- **But it turns map-shaped objects into columns.** These are objects keyed by tool_use_id, file path or
  model name. That gave 1,825 columns from 40 MB, and 4,812 columns after 2% of the history.
- **Its memory grows with table width, not with bytes.** The bounded set (40 MB) peaked at 2.0–2.45 GB.
  The full-history run was killed at 6.7 GB after 76 MB.
- **duckorch is weaker.**
  - It reads a path itself, so a record carries no filename and no line.
  - Its own inference rejects whole loads: 1 of 60 bounded, 4 of 397 in the full run.
  - Content blocks stay JSON text: 10 child rows out of 2,234 content-bearing lines.
  - Through the quackapi `/sql` route every load ran twice.

The useful part of RawDuck is its type lattice. It would work if the four map-shaped paths were kept as
JSON. But that is a hand-written list, which is the declared-views design again.

## The approach, and what each tool needed beyond "point it at the folder"

`ingest.sql` runs in its own process and file. It does four things:
1. Setup once at the top (SET, LOAD, ATTACH), then it starts its own quack plus quackapi `/sql` on 9514/9515.
2. Stage 1: `find ~/.claude ~/.codex -name '*.jsonl' -o -name '*.jsonl.zst' -o -name '*.meta.json'`, read
   through shellfs `read_csv` (1.3 s).
3. It groups the files into one source per directory.
4. It posts one plain statement per directory to its own `/sql`. `list_transform` over an ordered list
   makes the posts sequential. A plain SELECT posted from several threads at once, so several large
   folders ingested in parallel.

| | RawDuck | duckorch |
|---|---|---|
| statement per folder | `INSERT INTO raw.ingest.<t> SELECT to_json(t) FROM read_json('<dir>/*.jsonl', format='newline_delimited', records='false', columns={json:'JSON'}, filename=true) t` | `PRAGMA orch_ingest_run('<dir>/*.jsonl', 'orch.<t>', disposition='append')` |
| `.jsonl.zst` | native (read_json) | native (it calls read_json) |
| filename | yes, but the ingest lane takes exactly **one** JSON column ("table has 1 columns but 2 values"), so the row is packed with `to_json(t)` and columns come out as `json.<path>` plus `filename` | **impossible**: it builds `read_json(path, union_by_name, sample_size=-1)` itself, with no filename |
| line number / per-line key | none from read_json. Natural keys: Claude `uuid` (3,446 of 4,585 lines; state records have none), Codex `ordinal` (= line − 1, every line) | none. `_orch_id` is `row_number()` over the whole read |
| content blocks as rows | `explode := 'message.content'` fails on untouched data ("payload mixes JSON objects with non-object values", because content is a string or an array). Blocks are unnested at query time | a child table only when every content value in that load is an array, otherwise a JSON text column |
| incremental / re-load | no merge, no key. Re-running unchanged files doubled the rows (4,585 → 9,170) | merge needs `primary_key`, and there is no file or line key to give it. `append` only |
| `raw_ingest_file(table, path)` | one file, no glob or directory ("Cannot open file …/*.jsonl") | n/a |
| other | none | opened as the startup database, its WAL failed to replay ("GetDefaultDatabase with no default database set"); it needs `CHECKPOINT` at the end. Through the quackapi route, each PRAGMA ran twice (2 load_ids, double rows); direct and via `quack_query` it ran once |

Content classification is done after ingest, in queries (sessionId+type → Claude; session_meta /
response_item → Codex). Stage 1 also returns plugins, jobs and history files, which land in the same tables.

The bounded set is 16 Claude sessions, 11 subagent transcripts plus 11 meta.json, 18 Codex `.jsonl` and
the 4 smallest `.jsonl.zst`. Total 39.6 MB. It was chosen by query: the smallest file per record kind found
by a content scan (`scan_features.sql`, `scan_codex.sql`, `pick_bounded.sql` → `bounded_files.csv`).
It covers subagents, thinking blocks with empty text (there are no `redacted_thinking` blocks anywhere in
the corpus), attachments, `stop_hook_summary`, agent-name, pr-link, mode, last-prompt, continued-in,
assistant and system `api_error`, Codex forks and subagent threads, and `turn_aborted`.

## 1. Schema from the data, and is it stable?

**RawDuck (bounded)**: three flat tables.

| table | columns | JSON-typed | of which map-keyed |
|---|---|---|---|
| claude_lines | 1,825 | 50 | 1,348 `json.wireToolInputs.<toolu_id>.<arg>`; 85 `snapshot.trackedFileBackups.<path>.*`; 50 `modelUsage.<model>.*` |
| codex_lines | 345 | 17 | `payload.item.changes./Users/…/<file>.move_path`, … |
| claude_meta | 8 | 0 | |

- Typed leaves: `json.message.usage.output_tokens` BIGINT, `json.timestamp` TIMESTAMP,
  `json.isApiErrorMessage` BOOLEAN, `json.ordinal` BIGINT.
- `json.message.content`, `json.toolUseResult` and `json.payload.content` are JSON, because they are
  structurally mixed. tool_use, tool_result and thinking blocks are therefore not tables: they are unnested
  at query time with `json_extract(content, '$[*]')`.

**Stability (append a later batch):**
- RawDuck: batch A (files before 2026-10-11) then all.
  - `claude_lines` went from 1,007 to 1,832 columns (20 → 50 JSON), `codex_lines` from 232 to 345.
  - Nothing failed, so evolution works.
  - But the width is unbounded: the full run reached 4,812 columns at 2% of the bytes.
- duckorch ledger (bounded, per file):
  - 47 tables, `orch.claude_lines` at 19 versions.
  - 362 `column_added`, 4 `type_widened`, 11,374 `column_removed` entries (one per column a later
    file lacked).
  - One load was rejected outright: `claude_lines__attachment__tools.schema__input_schema__additionalproperties: BOOLEAN → JSON`.
  - Full run: 4 rejected loads, so whole folders were dropped. The other three:
    - `codex_lines.payload__model_context_window`, in two Codex day folders.
    - `claude_lines.timestamp`, in `~/.claude/history.jsonl` (not a transcript).
  - It also creates map-as-columns (`modelusage__fire_extinguisher__*`, trackedFileBackups).
  - 78 tables, `claude_lines` at 723 columns after 11% of the history.

## 2. Incremental

Neither tool provides it.
- RawDuck: no merge and no key.
- duckorch: `merge` needs a `primary_key`, and with no filename or line there is nothing usable. Codex
  `ordinal` repeats across files, and forked rollouts replay the parent's lines.
- The only file-level watermark would be your own `(path, size, mtime)` plus a delete-by-filename.
  That works in RawDuck, whose tables keep `filename`, but not in duckorch.

## 3. Coverage vs `read_conversations` (bounded, `coverage.sql`, duck_diff `table_diff_summary`)

| check | result |
|---|---|
| Claude, key `uuid` (timestamp, model compared) | 3,446 identical, 0 different, 0 one-sided |
| Codex, key (file, line) with line = ordinal + 1 | 4,554 identical, **353 only in RawDuck**: `event_msg/item_completed` 207, `turn_context` 123, `session_meta` 23 (records read_conversations folds into other rows) |
| rows | RawDuck = independent read_json count exactly (Claude 4,585, Codex 4,907, meta 11) |
| `.jsonl.zst` | 44 lines from 4 compressed rollouts; read_conversations reads 0 of the 739 `.zst` files |
| duckorch (first load per source) | Claude 4,549 of 4,585 rows (the rejected file is missing), 3,431 of 3,446 uuids; Codex 4,951 of 4,951 |

**Claude token overcount.** `read_conversations` sums 733,170 output tokens. The actual figure is
283,931, over 611 API responses (**2.58×**). Neither tool's schema avoids this by itself: both keep one
row per line, and the usage repeats on each line. In RawDuck it is one `GROUP BY "json.message.id"` with
`arg_max(..., "json.timestamp")`. duckorch has `message__id` and the usage columns, so the same fix applies.

## 4. Dead columns (`SUMMARIZE (SELECT nullif(trim(COLUMNS(*)::VARCHAR), '') FROM t)`)

| table | columns | never filled |
|---|---|---|
| claude_lines | 1,825 | 16 (e.g. `json.message.container`, `json.message.stop_sequence`, `json.stopReason`, `json.wireToolInputs.<id>.new_string`) |
| codex_lines | 345 | 23 (e.g. `json.payload.rate_limits.*`, `json.payload.item.changes.<path>.move_path`) |
| claude_meta | 8 | 0 |

These columns exist because the key appears with a null or empty value. The real waste is the map-keyed
columns: they are filled but each is filled only once.

## 5. Speed and peak memory (`/usr/bin/time -l`, 4 threads, 4 GB limit)

| run | wall | peak RSS | result |
|---|---|---|---|
| stage 1 `find` | 1.3 s | – | 2,858 candidate files, 3,776 MB |
| RawDuck bounded (39.6 MB, 24 folder statements) | 3.1–3.6 s | 2.0–2.45 GB | complete |
| duckorch bounded (60 per-file calls, each executed twice by the route) | 13.4 s | 0.86 GB | 59 of 60 ok |
| RawDuck full, sequential, smallest folders first | killed at 22 s | 6.7 GB | 366 of 423 folders, 1,119 files, **76 MB of 3,776 (2%)**, claude_lines at 4,812 columns |
| duckorch full, sequential | killed at 68 s | 6.0 GB | 396 of 423 folders, 1,566 files, **424 MB of 3,776 (11%)**, 4 rejected |
| Alok's (a): one `INSERT … read_json('~/.claude/**/*.jsonl')` | killed at 3 s | 7.0 GB | nothing landed (one transaction) |
| RawDuck parallel dispatch (a plain SELECT of `http_post` per row) | killed at 4 s | 7.1 GB | several folders in flight at once; fixed with `list_transform` |

For comparison, ten RawDuck inserts of a small file ran in 51 MB, and 60 dispatched no-ops in 74 MB.
Neither the dispatch nor RawDuck itself leaks per statement. Memory follows the target table's width.

## 6. Query ergonomics (`queries.sql`, RawDuck tables, 0.08 s for all five on the bounded set)

41 SQL lines in total:
1. **Tokens per model per day.** A Claude CTE keeps one row per `json.message.id`; a Codex CTE ASOF-joins
   `token_count` to the preceding `turn_context` on (filename, ordinal). 13 lines. Example row:
   gpt-6-astra, 2026-10-14: 13.3 M input.
2. **Tool calls by name with error rate.** Blocks are unnested from `json.message.content` and joined
   tool_use → tool_result. 7 lines. Bash: 292 calls, 10.6% errors; `mcp__dev__query`: 104 calls, 21.2%.
3. **Subagent sessions with parent.** Claude from the path plus `claude_meta.agentType`; Codex from
   `session_meta.payload.source.subagent.thread_spawn.parent_thread_id`. 7 lines.
4. **Reminders by kind.** `html_extract_text(parse_html(text), '//system-reminder')` over user text blocks.
   7 lines.
5. **Turns per session.** Distinct `json.promptId`, or `task_started` per rollout. 4 lines.

The queries are short because RawDuck gave typed, named columns. They are not shorter than the same
queries over declared views, and they need quoted dotted names (`"json.message.usage.output_tokens"`).

## What each tool did well and badly

**RawDuck**
- Good:
  - Zero declarations.
  - Correct types (timestamps, ints, booleans).
  - Lossless: every record, `.zst` included.
  - 100% uuid agreement with `read_conversations`.
  - Ordinary DuckDB tables.
- Bad:
  - Map-keyed objects become columns.
  - Memory scales with width.
  - One JSON column per insert, so file identity has to be packed into the payload.
  - `explode` requires uniform arrays.
  - No merge or key.
  - `raw_ingest_file` takes no globs.

**duckorch**
- Good:
  - A real schema ledger (versions and changes, with DDL).
  - Clean parent/child linking (`_orch_parent_id`, `_orch_index`).
  - Asset and lineage bookkeeping.
  - Lowest memory of the two on the bounded set.
- Bad:
  - No filename or line.
  - Per-load read_json inference makes shapes depend on which files are in a load, and incompatible
    changes reject whole folders.
  - Content, `toolUseResult` and attachment fall back to JSON text.
  - WAL replay failure when the file is opened as the startup database.
  - Double execution through the quackapi route. That is likely a quackapi/quack prepare-then-execute
    interaction with side-effecting PRAGMAs; it is worth an issue on quackapi.

## Caveats

- quackapi's server defaults set route connections to all cores. The program passes `threads := 4,
  memory_limit := '4GB'` to `quackapi_serve` explicitly.
- One early full run wrote its first 58 small folders into the bounded store, because the store path was
  hardcoded; it is now relative to the run's folder. The bounded store was rebuilt, and every bounded
  number above comes from the clean store.
