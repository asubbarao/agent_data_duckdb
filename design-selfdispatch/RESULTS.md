# Self-dispatch reader: find → directories → CASE reader → one dispatched read per directory piece

`reader.sql` (55 lines, one statement after the LOADs, no staging table, nothing created on dev).
Runs in a disposable `duckdb :memory:` client; every read runs on dev (pid 68384, `/sql` on 9495).
DuckDB 1.5.6, quackapi on dev, agent_data 54d875a (dev's binary, loaded locally for the comparison).
Work folder: `/private/tmp/claude-501/agent_data_selfdispatch/` (output Parquet, RSS samples, rc.parquet).

## Verdict

- **It reads everything in 82 s with nothing on dev but SELECT/COPY:** 1,155,760 lines from 2,154 files
  (1,317 Claude `.jsonl`, 837 Codex incl. 739 `.jsonl.zst`), 0 failed pieces, 0 unparseable lines.
  Views: 78 s, 5.7 GiB. RawDuck: 7 runs, ~100 s, 5.2 GB.
- **Peak dev RSS: 5.72 GiB, from an idle 3.4–3.9 GiB baseline (+1.8 GiB).** The cost is one big piece at a
  time; 383 small pieces at 8 in flight added only 0.9 GiB. The client never passed 80 MB.
- **Rows cannot come back over `/sql`.** quackapi caps a response at 16 MiB (`quackapi_max_response_bytes`
  = 16777216); 8 of the first 35 bounded pieces got 507 "Response exceeds configured byte limit", and single
  Codex `compacted` lines reach 21 MB. So each dispatched statement is a `COPY … TO` Parquet on dev and the
  wire carries only the receipt `[{"Count": n}]`. The reader's result is
  `read_parquet('lines/*/*.parquet', hive_partitioning := true)`: 2.7 GB on disk.
- **Line numbers are exact by construction:** `read_lines` returns `line_number`, so no row_number and no
  order dependence (dev runs `preserve_insertion_order = false`, which would make a row_number unsafe).
- **The method is simple; dev is the constraint.** Dev is shared, and another agent's sharded
  `read_conversations` job held it at 6.2–7.8 GB on its own for ~15 minutes, twice. The measured runs
  were taken only when dev was idle (no foreign request over 500 ms in the window, checked in
  `quackapi_requests()`).

## The program

1. **find** (shellfs, ~2 s): `find ~/.claude ~/.codex -type f -name *.jsonl -o -type f -name *.jsonl.zst`
   through `read_lines`, `rtrim` the newline.
2. **entries** = PR #1576's columns from hostfs scalars on the find rows: `ext, parent (parse_dirpath), name,
   stem (parse_filename(path, true)), parts (path_split), depth, type (path_type), bytes (file_size),
   modified (file_last_modified), reader, sql_statement`, with 1576's `readers` CTE (catalog `read_<ext>`).
   **reader** is a CASE first: `.jsonl` → `read_lines(path)`, `.jsonl.zst` → `read_lines('zstd -dcq "path" |')`,
   else the catalog reader, else `read_blob`. `read_lines` does not decompress `.zst` (it fails "not valid
   UTF-8"), so zst goes through the zstd pipe. `read_ndjson_objects`/`read_json` were not used: they have no
   line number.
3. **dirs**: the directories are `parent` from entries, cut into pieces at 64 MB of text by a running sum
   (`.zst` bytes × 3, the measured ratio). 2,154 files → 372 directories → 415 pieces.
4. **statements**: per piece, the files' `sql_statement`s `UNION ALL`'d inside one
   `COPY (SELECT file, line, record, top-level columns …) TO 'lines' (PARTITION_BY (scope), FILENAME_PATTERN
   'piece_<md5(parent||piece)>_{i}', ROW_GROUP_SIZE 2048)`, built with printf and dollar quotes.
5. **dispatch**: Shape A, curl in `read_lines_lateral` with a quoted heredoc body, one receipt per piece.
   `SET threads = WIDTH` sets how many are in flight.

Output columns: `file, line, parent, piece, source, record (JSON), bad, session_id, type, payload_type,
timestamp, uuid, parent_uuid, role, model, message_id, usage (JSON), tool_name, tool_id, scope`.
Nested values stay JSON; Claude tool names/ids are the `$.message.content[*]` lists as text.

## Runs (dev idle, RSS sampled every 0.25 s)

| run | pieces | rows | wall | dev RSS (GiB) | client RSS |
|---|---|---|---|---|---|
| bounded (33 dirs, 127 files, 220 MB), WIDTH 16 | 35 ok | 66,987 | 5.4 s | 4.12 → 4.54 | 89 MB |
| small: pieces ≤ 64 MB, WIDTH 8 | 383 ok | 482,808 | 22.6 s | 3.30 → 4.22 | 80 MB |
| big: pieces > 64 MB, WIDTH 1 | 32 ok | 672,952 | 59.2 s | 3.39 → **5.72** | 60 MB |
| **full = small + big** | **415 ok** | **1,155,760** | **81.8 s** | **5.72 peak** | |

Each wall includes its own find (~2 s). Big runs serially because one piece is expensive (next section);
its 3.26 GB of text is 59 s on one dev thread per piece, while small pieces spread over dev's 10 threads.

### What a piece costs (the 325 MB Claude file, standalone duckdb at dev's settings: threads 10, 7.4 GiB, no insertion order)

| statement | peak RSS | time |
|---|---|---|
| `read_lines` only | 80 MB | 0.66 s |
| + `content::JSON ->> 'type'` | 0.24 GB | 0.8 s |
| full projection, aggregated (no write) | 0.40 GB | 1.1 s |
| COPY to Parquet, default row group | 2.50 GB | 2.7 s |
| COPY to Parquet, `ROW_GROUP_SIZE 2048` (kept) | 1.69 GB | 2.7 s |
| COPY to CSV | 1.63 GB | 4.0 s |
| COPY Parquet of lines 1–20,000 / 1–40,000 (60 / 120 MB) | 0.93 / 1.22 GB | 0.5 / 1.1 s |

The writer, not the read, holds the memory. `DICTIONARY_SIZE_LIMIT`, `ROW_GROUP_SIZE_BYTES`, smaller row
groups and no compression did not move it below ~1.7 GB. `read_lines(path, lines := 'a-b')` works, so
splitting big files into line ranges would let big pieces run wide; that needs the line counts first
(another dispatch stage), so it was not added.

## Coverage vs `read_conversations` (duck_diff `table_diff_summary`, key (file, line))

`read_conversations` rows were exported locally (not on dev) 18 min before the read: 18.7 s, 3.8 GB RSS.

| source | identical | different | here only | read_conversations only |
|---|---|---|---|---|
| Claude (uuid, timestamp) | 603,902 | 0 | 7,462 | 0 |
| Codex (timestamp) | 133,238 | 0 | 411,158 | 0 |

Rows only here:
- Codex `.jsonl.zst` rollouts: 401,877 lines in 739 files (read_conversations does not read them).
- Codex lines that read_conversations folds into other rows, in files it reads: `event_msg/item_completed`
  4,726, `turn_context` 932, `session_meta` 93.
- Non-transcript `.jsonl` (the find has no folder rules): `~/.claude/history.jsonl` 4,465,
  `~/.codex/history.jsonl` 1,753, `~/.claude/plugins` (.trash + synced eval trials) 1,549, `~/.claude/jobs`
  ~870, `~/.codex/tmp/agent-linkage-probe` 1,618, `session_index.jsonl` 72, task-handoffs, state.
- Growth after the export: 310 lines in 3 live Claude files and one new file (277).

**Line numbers.** All 603,902 shared Claude keys and 133,238 Codex keys agree with
`read_conversations.line_number`. Codex writes its own `ordinal`: `ordinal + 1 = line` on 527,151 of 542,484
lines (826 files). The other 15,333 lines are 5 files where the offset is constant (3663, 533, 7417, 350,
114): those files start at a later ordinal, and the numbering inside them is still consecutive.

## Token check (Claude output tokens)

| count | output tokens | rows |
|---|---|---|
| read_conversations, assistant (one row per content block) | 128,318,076 | 190,043 |
| here, every assistant line | 128,394,911 | 190,273 |
| here, one per (file, message.id), last line | 53,868,824 | 80,091 → read_conversations is **2.38×** |
| here, one per message.id across files | 36,449,016 | 58,303 → **3.52×** |

Same overcounts RawDuck found (usage repeats on every content-block line; resumed/forked sessions copy
history across files). The small difference in the first two rows is the 18 minutes of live growth.

## Compared

| | self-dispatch (this) | views | RawDuck |
|---|---|---|---|
| program | 55 lines, 1 statement, 0 tables | 549 lines of views + derivation pass | 85 lines, 2 staging tables |
| full history | 82 s, 2 batches | 78 s, 1 pass | ~100 s, 7 runs |
| peak memory | dev 5.72 GiB (+1.8), client 80 MB | 5.7 GiB | 5.2 GB |
| where it runs | reads on dev, Parquet per piece | one process | disposable process |
| line number | `read_lines` native | rowid order | rowid order |
| `.zst` | yes (zstd pipe) | yes | yes |
| storage | 2.7 GB Parquet | none | 8.1 GB |
| incremental | not built; a piece's file name is md5(parent‖piece), so re-running a directory overwrites it | max(modified) per dir | ledger in rows |

## Incidents (dev never went down; no restart was needed)

1. First full attempt, all 415 pieces at ~13 in flight: dev went 4.5 → 6.96 GB, overlapping another agent's
   14 s `read_conversations` request. The kill switch then killed my own shell (its `pkill -f` pattern matched
   its own command line). In-flight pieces finished; `SELECT 42` kept answering.
2. Second attempt, big pieces serially: dev reached 6.4–7.0 GB while the other agent's 7 s sharded job ran
   (112 shards, about 15 minutes, twice); the kill switch stopped my client. Measured runs waited for dev
   to go idle (CPU < 40 %, RSS < 5.2 GB, three samples in a row).
3. A killed client leaves an in-flight piece writing on dev. One partial Parquet file
   ("No magic bytes") came from that; the final runs replaced the batch folders.

## Rules kept / deviations

- No CREATE on dev; dev ran SELECTs and `COPY … TO` under `/private/tmp/claude-501/agent_data_selfdispatch/`.
  No .py/.sh, no jq/awk, no regex on backend data, no extra server, no intermediate JSON files.
- Deviation: the dispatched reads write Parquet, because rows on the wire hit the 16 MiB cap. Raising
  `quackapi_max_response_bytes` would change dev's guard for everyone, so it was left alone.
- Deviation: `read_lines` instead of `read_ndjson_objects`/`read_json`, for exact line numbers.
