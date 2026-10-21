-- ingest.sql: Claude Code + Codex transcripts into RawDuck. Schema comes from the data.
-- Three stages: find -> stage -> ingest. Run it in its own folder and process, never on dev:
--   cd /private/tmp/claude-501/agent_data_rawduck/full && SCOPE=full duckdb work.duckdb -f ingest.sql
-- work.duckdb holds the two staging tables (files, stage); raw.db is the RawDuck store.
-- SCOPE=bounded (the default) takes 10 small files per kind of path, picked by hash(path).
-- A re-run reads only files whose (path, bytes, mtime) is new and adds only (file, line) keys not yet ingested.
SET threads = 4;
SET memory_limit = '2GB';  -- RSS runs ~1.5 GB above the limit and macOS keeps it: 4GB here took the process past 6 GB
SET temp_directory = '/private/tmp/claude-501/agent_data_rawduck/tmp';
LOAD shellfs;
LOAD hostfs;
LOAD read_lines;
LOAD rawduck;
SET rawduck_overlap_flush = true;  -- writes finished row groups during an insert: Codex alone went from OOM at 3 GB to 4.5 GB RSS
ATTACH IF NOT EXISTS 'rawduck:raw.db' AS raw;
-- the ledger is in the rows themselves: _file, _line, _bytes, _mtime
CREATE TABLE IF NOT EXISTS raw.claude (_file VARCHAR, _line BIGINT, _bytes BIGINT, _mtime TIMESTAMP);
CREATE TABLE IF NOT EXISTS raw.codex (_file VARCHAR, _line BIGINT, _bytes BIGINT, _mtime TIMESTAMP);

-- 1. find: every .jsonl / .jsonl.zst under both roots not already ingested at this size and mtime
CREATE OR REPLACE TABLE files AS
WITH found AS (
    SELECT path, file_size(path) AS bytes, file_last_modified(path) AS mtime
    FROM (SELECT rtrim(content, e'\n') AS path
          FROM read_lines($c$find /Users/aloksubbarao/.claude /Users/aloksubbarao/.codex -type f -name *.jsonl -o -type f -name *.jsonl.zst |$c$))
    WHERE bytes < CASE getenv('SCOPE') WHEN 'full' THEN 1e15 ELSE 4e6 END
    QUALIFY row_number() OVER (PARTITION BY starts_with(path, '/Users/aloksubbarao/.codex/'), suffix(path, '.zst'), contains(path, '/subagents/')
                               ORDER BY hash(path)) <= CASE getenv('SCOPE') WHEN 'full' THEN 1e9 ELSE 10 END
), ingested AS (
    SELECT DISTINCT _file AS path, _bytes AS bytes, _mtime AS mtime FROM raw.claude
    UNION SELECT DISTINCT _file, _bytes, _mtime FROM raw.codex
)
SELECT * FROM found ANTI JOIN ingested USING (path, bytes, mtime);

-- 2. stage: one row per line in file order (CTAS keeps insertion order, so line = rowid - file's first rowid + 1)
CREATE OR REPLACE TABLE stage AS
SELECT filename AS file, json
FROM read_json_objects(['/Users/aloksubbarao/.claude/**/*.jsonl', '/Users/aloksubbarao/.codex/**/*.jsonl', '/Users/aloksubbarao/.codex/**/*.jsonl.zst'],
                       format := 'newline_delimited', filename := true)
WHERE filename IN (SELECT path FROM files);

-- kind by content, per file: Codex has session_meta/response_item lines, Claude has sessionId + type.
-- before = text of the same kind's older files in this batch. The RawDuck insert holds what it is given until it
-- commits (3.5 GB of Codex text did not fit in 3 GB), so a run inserts files that start below 0.75 GB per kind,
-- oldest first; the rest wait for the next run.
CREATE OR REPLACE TABLE files AS
SELECT files.*, firsts.first, kinds.kind, kinds.text,
       sum(kinds.text) OVER (PARTITION BY kinds.kind ORDER BY files.mtime, files.path) - kinds.text AS before
FROM files
JOIN (SELECT file, rowid AS first FROM stage QUALIFY row_number() OVER (PARTITION BY file ORDER BY rowid) = 1) firsts ON firsts.file = files.path
JOIN (SELECT file, sum(length(json)) AS text,
             CASE WHEN bool_or((json->>'type') IN ('session_meta', 'response_item')) THEN 'codex'
                  WHEN bool_or((json->>'sessionId') IS NOT NULL AND (json->>'type') IS NOT NULL) THEN 'claude' END AS kind
      FROM stage GROUP BY file) kinds ON kinds.file = files.path;
CHECKPOINT;  -- without it the just-written stage stays in memory and the Codex insert runs out

-- 3. ingest. One rule: top-level scalars stay as they are, every top-level object or array becomes
--    JSON text (parsed later with json_each / ->>), plus _file, _line, _bytes, _mtime.
--    The ::MAP cast parses each line once (json_keys + json_extract per key parsed it once per key: 10.5 s vs 6.1 s).
-- Codex on one thread: its lines reach 21 MB (23 `compacted` lines of 5-21 MB in 3 rollouts; those 34k lines alone
-- take the sink to 5.6 GB RSS) and at 2+ threads the RawDuck sink runs out of memory. A run that fails here
-- leaves those files for the next run.
SET threads = 1;
SET memory_limit = '3GB';
INSERT INTO raw.ingest.codex
SELECT json_merge_patch(
           to_json(map_from_entries(list_transform(map_entries(s.json::MAP(VARCHAR, JSON)),
               e -> {k: e.key, v: CASE WHEN json_type(e.value) IN ('OBJECT', 'ARRAY') THEN to_json(e.value::VARCHAR) ELSE e.value END}))),
           json_object('_file', s.file, '_line', s.rowid - f.first + 1, '_bytes', f.bytes, '_mtime', f.mtime))
FROM stage s JOIN files f ON f.path = s.file AND f.kind = 'codex' AND f.before < 7.5e8
ANTI JOIN raw.codex r ON r._file = s.file AND r._line = s.rowid - f.first + 1;

SET threads = 2;  -- the RawDuck sink at 4 threads peaked at 5.6 GB on Claude
INSERT INTO raw.ingest.claude
SELECT json_merge_patch(
           to_json(map_from_entries(list_transform(map_entries(s.json::MAP(VARCHAR, JSON)),
               e -> {k: e.key, v: CASE WHEN json_type(e.value) IN ('OBJECT', 'ARRAY') THEN to_json(e.value::VARCHAR) ELSE e.value END}))),
           json_object('_file', s.file, '_line', s.rowid - f.first + 1, '_bytes', f.bytes, '_mtime', f.mtime))
FROM stage s JOIN files f ON f.path = s.file AND f.kind = 'claude' AND f.before < 7.5e8
ANTI JOIN raw.claude r ON r._file = s.file AND r._line = s.rowid - f.first + 1;

-- receipt: what this run read, and how much text was left for the next run
SELECT kind, count(path) AS files, sum(text) AS text, sum(text) FILTER (WHERE before >= 7.5e8) AS deferred
FROM files GROUP BY kind ORDER BY kind;
