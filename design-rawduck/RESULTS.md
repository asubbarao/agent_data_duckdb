# RawDuck, redone: Claude Code + Codex transcripts, schema from the data

Run in disposable files under `/private/tmp/claude-501/agent_data_rawduck/{bounded,full}/`
(`work.duckdb` for the staging tables, `raw.db` the RawDuck store), one duckdb process per run, never on dev.
Dev was used read-only once, to export `read_conversations` rows to `rc.parquet` for the coverage check.
DuckDB 1.5.6, rawduck a14e474 (no newer build in community), duck_diff 68ace70, agent_data on dev.
A watchdog killed any run above 6 GB RSS. `/usr/bin/time -l` and the watchdog measured every run.

## Verdict

**The views approach is still the better design for these transcripts.** RawDuck now gets through the whole
history, but it needs seven runs, and the schema it infers says little.

- **Seven runs, not one pass.** The full history (1.14 M lines, 5.8 GB of text) took 7 runs and 100 s in
  total, peaking at 5.2 GB RSS. The views reader does it in one pass: 78 s end to end, 5.7 GiB peak.
  - RawDuck's INSERT lane keeps everything it is given in memory until it commits. 3.5 GB of Codex text
    does not fit in 3 GB.
  - So each run ingests at most 0.75 GB of text per kind, oldest first.
  - Two of the seven runs hit a Codex out-of-memory error. The insert rolled back and the next run picked
    those files up again.
- **Codex has to run on one thread.** 23 `compacted` lines of 5–21 MB (in 3 rollouts) take the sink to
  5.6 GB RSS on their own.
- **The schema says little.** Following the one rule, nested values stay JSON text:
  - Claude gets 168 typed columns. These are the top-level keys: timestamps, booleans, durations,
    `totalCostUSD`.
  - Codex gets 9 columns, because every Codex record keeps its content in `payload`. For Codex, RawDuck
    is a typed envelope around one JSON string, which is what a view over `read_json_objects` already gives.
- **The store is large.** `raw.db` is 8.1 GB, against 3.8 GB of source on disk.
  - Escaped JSON strings account for some of it.
  - Rolled-back optimistic writes from the failed runs account for some too.
- **What works well:**
  - Columns appear with no DDL.
  - Rows agree with `read_conversations` on (file, line), apart from live writes.
  - `.zst` is read natively.
  - The ledger inside the rows makes re-runs cheap: 4.2 s, 0.8 GB RSS.
  - Nothing was dropped, and every line is kept.

Compared with the previous attempt, the previous run stopped after 2% of the history at 6.7 GB with 4,812
columns. This one ingested all of it, with 168 and 9 columns.

## The pipeline (`ingest.sql`, 85 lines: 3 stages, 2 staging tables)

1. **find.** One shellfs `find` over both roots, with `file_size` and `file_last_modified` from hostfs. It
   anti-joins the ledger, which is `DISTINCT _file, _bytes, _mtime` read from the RawDuck tables themselves.
   - So there is no ledger table. A file is read again only when its size or mtime changed.
   - `SCOPE=bounded` keeps the 10 files per path kind that come first by `hash(path)`. The pick is stable
     across re-runs.
2. **stage.**
   - One `read_json_objects` over three globs (`.claude/**/*.jsonl`, `.codex/**/*.jsonl`,
     `.codex/**/*.jsonl.zst`), semi-joined to `files`. The semi-join prunes the glob to the selected files:
     one file out of the whole tree took 2.5 s.
   - **line** is `rowid` minus the file's first `rowid`, plus 1. CTAS keeps insertion order.
     - It matched Codex `ordinal + 1` on every line.
     - It matched `read_conversations.line_number` on all 602,970 shared Claude keys.
   - **kind** is decided by content, per file: Codex if any line has `type` `session_meta` or
     `response_item`; Claude if any line has both `sessionId` and `type`.
   - The 279 other `.jsonl` files (plugins, jobs, codex state) get kind NULL and are not ingested.
   - `CHECKPOINT` comes next. Without it, the newly written stage stays in memory and the Codex insert runs
     out.
3. **ingest.** One `INSERT INTO raw.ingest.<claude|codex>` per kind. Each line becomes:
   - its top-level keys, with every OBJECT or ARRAY value turned into JSON text
     (`s.json::MAP(VARCHAR, JSON)` → `map_entries` → `to_json(map_from_entries(...))`);
   - merged with `json_object('_file', '_line', '_bytes', '_mtime')`.
   - `ANTI JOIN raw.<t>` on (`_file`, `_line`) skips lines already ingested, so a file that grew adds only
     its new lines.
   - The `::MAP` cast parses each line once. `json_keys` plus a `json_extract` per key took 10.5 s,
     against 6.1 s for the cast.
   - `json_each` plus `json_group_object` grouped 1.14 M groups. The scalar form streams.

There are no map-path detection, no `to_json(t)` prefixing, no COPY'd .sql or .json files, no extra servers
and no self-dispatch. `CREATE TABLE IF NOT EXISTS raw.claude (_file, _line, _bytes, _mtime)` pins the key
types; RawDuck evolves the table from there.

## 1. Columns, and new fields becoming columns

| table | bounded (33 files) | full | typed (not VARCHAR) in full |
|---|---|---|---|
| raw.claude | 106 | 168 | 48 (TIMESTAMP, BOOLEAN, BIGINT, DOUBLE) |
| raw.codex | 9 | 9 | 5 (`_line`, `_bytes`, `_mtime`, `timestamp`, `ordinal`) |

- Claude grew from 106 to 168 columns over the seven full runs, with no DDL and the same INSERT.
- Codex: `_file, _line, _bytes, _mtime, timestamp, ordinal, type, payload, metadata`.
- Nested fields are read with `->>` on the text columns, for example `message->'usage'->>'output_tokens'`
  and `payload->>'type'`.
- Nothing wide was flattened, so no map detection was needed.

## 2. Coverage vs `read_conversations` (duck_diff `table_diff_summary`, key (file, line))

`DESCRIBE`: `read_conversations` has 59 columns; raw.claude has 168; raw.codex has 9. (The read_conversations
rows were exported from dev about 20 minutes after the ingest, so live sessions differ.)

| source | identical | different | RawDuck only | read_conversations only |
|---|---|---|---|---|
| Claude (compared on uuid, timestamp) | 602,970 | 0 | 12 (1 live file) | 28 (3 live files) |
| Codex (compared on timestamp) | 128,565 | 0 | 409,019 | 123 (4 live files) |

Codex rows that only RawDuck has:
- 401,877 lines from the 739 `.jsonl.zst` rollouts, which `read_conversations` does not read.
- 7,142 lines in `.jsonl` files:
  - 5,431 in files it reads but folds into other rows: `event_msg/item_completed` 4,505, `turn_context` 926.
  - The rest are in 4 `.jsonl` files it did not list.

```sql
FROM table_diff_summary(
  $q$SELECT _file AS file, _line AS line, uuid, timestamp FROM raw.claude$q$,
  $q$SELECT DISTINCT file_path AS file, line_number AS line, uuid, timestamp::TIMESTAMP AS timestamp
     FROM 'rc.parquet' WHERE source = 'claude'$q$,
  pk := ['file', 'line']);
```

## 3. Dead columns (`SUMMARIZE (SELECT nullif(trim(COLUMNS(*)::VARCHAR), '') FROM t)`)

- **raw.claude:** 1 never filled (`stopReason`).
  - 32 more show `null_percentage = 100` only because the value is rounded. Each holds 1–9 distinct
    values, for example `continuedInSessionId`, `originalModel`/`fallbackModel` and the
    `apiRefusal*` fields.
  - So check `min IS NULL`, not `null_percentage`.
- **raw.codex:** 0.

## 4. Incremental

- **Bounded:** run 2 added 0 rows (3,994 Claude, 7,989 Codex both times). It re-read only the 7 non-transcript
  files.
- **Bounded, a simulated grown file:** a file's ledger `_bytes` was set off by one. The file was read again
  and 0 rows were added.
- **Full:** runs 8 and 9 read only the live sessions (3 Claude and 4 Codex files).
  - They added only their new lines: 42 and 90, then 0 and 29.
  - `count(DISTINCT (_file, _line))` stayed equal to the row count, so there were no duplicates.
- **Known edges:**
  - A grown file whose new lines were all present already keeps its old `_bytes`, so it is read again until
    a new line lands.
  - A Codex `.jsonl` that is later compressed to `.jsonl.zst` is a new path, so its lines appear under both
    names.

## 5. Speed and peak RSS

| run | wall | peak RSS | result |
|---|---|---|---|
| find (both roots) | 1.2–1.9 s | – | 2,152 files |
| bounded, run 1 | 4.8 s | 0.65 GB | 33 transcripts, 11,983 lines, 55 MB of text |
| bounded, re-run | 3.6 s | 0.46 GB | +0 rows |
| full, run 1 | 18.2 s | 4.7 GB | stage 5.8 GB of text in 5–9 s; 0.75 GB inserted per kind |
| full, runs 2–7 | 18.4, 14.1*, 19.9, 7.9*, 11.9, 9.5 s | 5.2 GB max | *Codex out of memory, rolled back, done next run |
| **full, total** | **100 s over 7 runs** | **5.2 GB** | 602,940 Claude + 537,465 Codex lines |
| incremental | 4.1–4.2 s | 0.67–0.82 GB | live files only |
| query: every Claude tool_use, unnested from `message` at query time | 0.38 s | 2.4 GB | 101,534 blocks |

Memory notes from the attempts:
- At `memory_limit=4GB`, RSS ran about 1.5–2.5 GB above the limit and passed 6 GB. This happened in the
  JSON reader, in the RawDuck sink, and because macOS keeps freed memory.
- `rawduck_overlap_flush = true` cut Codex from out-of-memory at 3 GB to 4.5 GB RSS. Its description
  says it raises peak memory.
- One CTAS of every line as JSON is a 6.5 GB file. A row_number window over it carrying the payload
  passed 6 GB.
- A stale `temp_directory` from a killed run raised "Corrupt temporary file" in the next process. That
  includes the shared `~/.duck/tmp/local`, so the program uses its own temp folder.

**Content blocks:** keep line grain and unnest at query time. One pass over every Claude `message` takes
0.38 s. `explode := 'message.content'` would need string content wrapped into arrays first. It would also
copy the line's `usage` onto every block, which is the same overcount `read_conversations` has.

## 6. Token check (Claude output tokens)

| count | output tokens | rows |
|---|---|---|
| `read_conversations` (one row per content block) | 128,194,217 | 609,063 rows |
| RawDuck, summed per assistant line (same overcount) | 128,194,167 | 189,705 lines |
| RawDuck, one per (file, message.id), last line | 53,792,642 | 79,834 responses → `read_conversations` is **2.38×** |
| RawDuck, one per message.id across files | 36,418,990 | 58,050 responses → **3.52×** |

- `usage` repeats on every content-block line of a response, which gives the 2.38×.
- A message.id also repeats across files, because resumed and forked sessions copy history. Across files
  the overcount is 3.52×.
- Query: `arg_max((message->'usage'->>'output_tokens')::BIGINT, _line)` grouped by `message->>'id'`, over
  rows with `type = 'assistant'`.

## Compared with `design/views.sql`

| | RawDuck (this) | views |
|---|---|---|
| declared fields | none (top-level inferred) | listed columns literal from a derivation pass |
| full history | 7 runs, 100 s, 5.2 GB, 2 retries | 1 pass, 78 s, 5.7 GiB |
| Codex structure | 1 JSON `payload` column | `payload` STRUCT plus concept views |
| new top-level field | a column on its next insert | re-run the derivation (§2 of views.sql) |
| incremental | built in (ledger in rows, (file, line) anti-join) | re-read directories by max(modified) |
| storage | materialized: 8.1 GB | none (live reads) |

The parts of this design worth keeping for the views approach:
- The (file, line) key from `rowid` order.
- The ledger kept inside the rows.
- Classifying files by content.
- `read_json_objects` over globs semi-joined to the find rows.
