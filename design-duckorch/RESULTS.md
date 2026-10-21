# duckorch ingest of Claude Code + Codex transcripts, redone

`ingest.sql` is three stages in about 60 lines:
1. `find`: the `files` table.
2. Stage: one COPY to one NDJSON per source.
3. Two `orch_ingest_run` calls with `disposition = 'merge', primary_key = '_file,_line'`.

The only tables are `files` and duckorch's own. Runs used a disposable file under
`/private/tmp/claude-501/agent_data_duckorch/`, with `threads = 4`, `memory_limit = '4GB'` and an RSS watchdog
at 6 GB. Dev was used only for read-only `read_conversations` (agent_data 54d875a).
Versions: duckorch d9bc95f (0.3.0), duck_diff 68ace70, DuckDB 1.5.6. Run on 2026-10-14.

## Verdict

**duckorch is now in the views approach's league for loading. It is not a better design than views.**

| | views (design/views.sql) | duckorch (this) |
|---|---|---|
| end to end, full history | 78 s | 86–109 s (staging 64–82 s, then loads of 12–18 s and 7.5–13 s) |
| peak RSS | 5.7 GiB | 2.7–3.7 GB |
| lines | 1.13 M | 1,146,523 (Claude 604,044, Codex 542,479) plus 5 held back |
| persistent | no, read live | yes: tables, ledger, lineage, merge |

- **Fixed from the first attempt:**
  - Every load succeeded (0 of 6 rejected), against 4 of 397 before.
  - Filenames and line numbers are kept.
  - The merge works and re-runs add nothing.
  - Full history loads, against 11% before.
- **Shape:** duckorch adds a persisted, versioned copy with a schema ledger. Its column list is not fixed in advance, and `merge` is
  incremental-ready.
- **What carried it:** the staging rule, not duckorch. Every top-level object or array is JSON text, so duckorch only
  types about 170 scalar columns. Its two features that need nested data do nothing here: child tables from arrays,
  and struct flattening. Content blocks are unnested at query time, which is what the views do anyway.

## The pipeline, and what duckorch forced

- **Stage 1.** One shellfs `find` over both roots, 1.0–1.6 s, 2,859 files.
- **Bounded scope.** `hash(path) % 50 = 0` and 1 KB–3 MB, giving 36 files.
- **Stage 2.**
  - `read_lines` (read_lines extension) streams each file and numbers its lines, so there is no window.
  - `.zst` files go through scalarfs `decompress+zstd:`, and the paths are handed over in `pathvariable:files`.
  - A file's source is decided from its first 20 lines: Codex if a line is `session_meta` or `response_item`, Claude
    if a line has `sessionId` and `type`. Plugins, jobs and history files are not staged.
  - Each line becomes `to_json(map_from_entries(...))` over `content::JSON::MAP(VARCHAR, JSON)`. Objects and arrays
    become JSON text, and `_file` and `_line` are added.
- **Stage 3.** `orch_ingest_run` with merge on `_file,_line`.

Things that had to be worked around, measured:

| problem | cause | fix |
|---|---|---|
| staging over 6 GB in 7–10 s | `row_number() OVER (PARTITION BY filename ORDER BY ordinality)` keeps every JSON line for the window. `json_each`, `GROUP BY` and `json_group_object` were worse | `read_lines` line numbers: no window, 0.7 GB RSS to read all 5.8 GB of text in 21.6 s (single-threaded) |
| staging over 6 GB at 73 s | partitioned COPY holds 524,288 rows per partition before a flush (~5 KB per line) | `SET partitioned_write_flush_threshold = 1024` |
| a corrupt temp-block error | `~/.duckdbrc` sets a temp directory that other processes share | `SET temp_directory` to the run folder |
| **the whole Codex load rejected** | duckorch reads `read_json('<path>', union_by_name = true, sample_size = -1)`. It cannot pass `maximum_object_size` (stays 16 MiB) or any other read option. Quotes in the path are escaped, so nothing can be passed through it | lines whose staged text is 16 MiB or more go to the `held=true` partition. That is **5 Codex `compacted` records**, 16.9–21.5 MB, in 2 rollouts. They are not loaded |
| two extra columns, `src` and `held` | duckorch's `read_json` auto-detects hive partitions in the staging paths | none: left as they are |
| partial staging files loaded after a failed COPY | `duckdb -f` carries on after errors | run with `-bail` |

## 1. Tables, ledger, rejected loads

| table | rows | columns |
|---|---|---|
| `claude` | 604,044 | 170 (4 `_orch_*`/key, `src`, `held`). Types: 18 UUID, 2 TIMESTAMP, BIGINT, BOOLEAN, DOUBLE; JSON text as VARCHAR |
| `codex` | 542,479 | 11: `timestamp`, `ordinal`, `type`, `payload` (JSON text), `metadata` (JSON text), `_file`, `_line`, … |

- **Child tables: none.** No top-level array reaches duckorch, because they are all JSON text.
- **Content blocks are unnested at query time.** `json_each(json_extract(message, '$.content'))` over every Claude line ran in
  1.06 s. Tool calls: Bash 42,698, Read 6,340, Edit 5,667, `mcp__dev__query` 5,519, Write 2,471.
- **Column names are lowercased** (`parentUuid` → `parentuuid`). `session_id` and `sessionid` are both present.
- **Ledger, fresh full run:** `claude` v1 and `codex` v1, both `table_created`.
- **Ledger evolution test** (bounded load, then full load into the same file):
  - `claude` went to v2 with 74 `column_added` and no type changes.
  - `codex` stayed at v1.
  - Columns that a later load lacks are not recorded, and take NULLs.
- **Rejected loads:** 0 of 6 across the fresh full run, its re-run, and the evolution test (bounded plus full). Before the
  `held` partition existed, the full Codex load was rejected once on the 16 MiB limit.

## 2. Coverage vs `read_conversations`

The comparison pulled `read_conversations()` and `read_conversations(source := 'codex', include_archived := true)`
from dev: 743,263 rows. That took 64–96 hash-partitioned `/sql` posts. Dev's `/sql` caps a response at 16 MiB, and
about a quarter of the posts failed with quack 9494 connect errors and had to be retried. The two passes held dev at
6.2–7.8 GB for about 15 minutes each. Next time, use an existing export instead, e.g.
`/private/tmp/claude-501/agent_data_selfdispatch/rc.parquet`.

`read_conversations` emits more than one row for some Claude lines, so the comparison uses DISTINCT
(`file`, `line`, `uuid`, `timestamp`). duck_diff `table_diff_summary` with `pk := ['file', 'line']`:

| source | identical | different | duckorch only | read_conversations only |
|---|---|---|---|---|
| Claude (uuid, timestamp compared) | 603,954 | 0 | 237 | 0 |
| Codex (timestamp compared) | 133,233 | 0 | 409,246 | 5 |

- **Claude, duckorch only:** all 237 are lines appended to 4 live transcripts (this session and its subagents) between
  the pull and the load.
- **Codex, duckorch only:**
  - 396,387 lines in 793 `.jsonl.zst` files, plus 46 lines in 1 archived file. `read_conversations` reads no `.zst`.
  - 4,508 `event_msg`, `turn_context` and `session_meta` lines in files it does read. It folds these into other rows.
  - 1,655 lines in 3 rollouts under `~/.codex/tmp/agent-linkage-probe`. They are Codex by content, but
    `read_conversations` does not read that directory.
- **Codex, read_conversations only:** the 5 held-back lines over 16 MiB.

## 3. Dead columns (`SUMMARIZE (SELECT nullif(trim(COLUMNS(*)::VARCHAR), '') FROM t)`)

| table | columns | never filled | one distinct value |
|---|---|---|---|
| claude | 170 | 1 (`stopreason`) | 36, including `src` and `held` |
| codex | 11 | 0 | 3: `_orch_load_id`, `src`, `held` |

`null_percentage` is rounded to two decimals, so it reports 100.00 for 33 sparse columns. "Never filled" here means
`approx_unique = 0`.

## 4. Merge re-run

| run | Claude inserted / deleted | Codex inserted / deleted |
|---|---|---|
| bounded, re-run | 6,053 / 6,053 | 7,384 / 7,384 |
| full, re-run | 604,191 / 604,044 (+147) | 542,479 / 542,479 (0) |

The ledger stayed at v1. The +147 Claude lines are lines appended to live transcripts between the two runs. The files
modified after the first load started were this session's and one other live session. Codex, which was idle, added 0.

Cost: a merge deletes through `EXISTS` on (`_file`, `_line`) against the whole table. It still re-stages and reloads
every line; only the delete is incremental.

## 5. Speed and peak RSS (`/usr/bin/time -l`, 4 threads, 4 GB limit)

| run | wall | stage 2 | Claude load | Codex load | peak RSS |
|---|---|---|---|---|---|
| bounded (36 files, 6,053 + 7,384 lines) | 2.5–3.8 s | 1.1 s | 0.4 s | 0.3 s | 0.76–1.1 GB |
| full | 86 s | 64.5 s | 12.6 s | 7.5 s | 3.7 GB |
| full, fresh, with the `held` partition (dev pull running at the same time) | 109 s | 76.7 s | 18.0 s | 12.9 s | 2.7 GB |
| full, merge re-run | 114 s | 81.7 s | 18.3 s | 11.1 s | 3.3 GB |
| evolution: full after bounded | 94 s | – | – | – | 3.2 GB |

Staging is the bottleneck.
- `read_lines` is single-threaded: 21.6 s just to read the 5.8 GB.
- The rest goes on the JSON→MAP→JSON rewrite and writing 5.8 GB of staged NDJSON (Claude 2.4 GB, Codex 3.4 GB).
- `read_ndjson_objects` is multi-threaded, about 6 s to read everything, but it numbers lines only through a window. That window is
  what used over 6 GB.

duckorch itself is fast here: about 20 s to type and load 1.15 M lines. Its source is a view with
`row_number() OVER ()`; each target table re-reads it, and there is one table per source. The resulting database is 5.7 GB.

## 6. Token check

| | output tokens |
|---|---|
| Claude, summed per line (as `read_conversations` does) | 128,356,025 (`read_conversations`: 128,324,344) |
| Claude, one row per `message.id` (last line) | **36,480,893**: per-line summing is **3.5× too high** over the full history |
| Codex, `read_conversations` summed over everything | 3,093,898,704 |
| Codex, of which `message_type = 'token_usage_total'` | 3,082,190,203: cumulative totals, equal to the sum of `total_token_usage` in the duckorch table |
| Codex, per turn (`last_token_usage`), on the files `read_conversations` reads | 5,805,890 (`read_conversations` `token_usage` rows: 5,975,627) |
| Codex, per turn, all files including `.zst` | 23,741,725 |

- The Claude fix: keep the last line per `message.id`.
- The Codex fix: sum `token_usage` rows only, never `token_usage_total`. A plain sum over `read_conversations` Codex rows is
  about 530× too high.

## Against the views approach

- **Choose duckorch if** a persisted, versioned copy with merge and lineage is wanted, on top of the same
  "nested stays JSON" rule. It costs a 5.8 GB staging copy, a 5.7 GB database, and 5 Codex lines it cannot hold.
- **Choose views if** the goal is to read the transcripts. The views are faster to first answer (6.8 s for all lines).
  They have no staging copy, no 16 MiB limit, and no hive columns. They declare the record shape once, which
  duckorch's inference otherwise replaces only for top-level scalars.

Both need the same query-time JSON work for content blocks and tokens.
