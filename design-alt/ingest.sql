-- ingest.sql: Claude + Codex transcripts, "ingest first, schema later".
-- One program, two loaders. Each runs in its own disposable process and file (never dev):
--   ALT_LANE=raw  ALT_SCOPE=bounded duckdb /private/tmp/claude-501/agent_data_alt/alt.duckdb      -f ingest.sql
--   ALT_LANE=orch ALT_SCOPE=bounded duckdb /private/tmp/claude-501/agent_data_alt/orchlane.duckdb -f ingest.sql
-- ALT_SCOPE: bounded (default) | full.  The program serves its own /sql (9515) and posts one statement
-- per directory to itself: a table function cannot take the directory from a column.

SET threads = 4;
SET memory_limit = '4GB';
LOAD shellfs;
LOAD hostfs;
LOAD http_client;
LOAD quack;
LOAD quackapi;
LOAD rawduck;
LOAD duckorch;
ATTACH IF NOT EXISTS 'rawduck:raw.db' AS raw;   -- the RawDuck store sits next to the run (cwd)
PRAGMA orch_init;
FROM quack_serve('quack:localhost:9514', token := 'alt-local');
CREATE OR REPLACE ROUTE sql POST '/sql' AS SELECT * FROM quack_query('quack:localhost:9514', $sql, token := 'alt-local');
FROM quackapi_serve(9515, host := '127.0.0.1', query_timeout_ms := 900000, threads := 4, memory_limit := '4GB');

-- stage 1: every .jsonl, .jsonl.zst and .meta.json under ~/.claude and ~/.codex
CREATE OR REPLACE TEMP TABLE stage1 AS
SELECT path, parse_dirpath(path) AS dir, file_size(path) AS bytes,
       CASE WHEN suffix(path, '.meta.json') THEN '*.meta.json' WHEN suffix(path, '.zst') THEN '*.jsonl.zst' ELSE '*.jsonl' END AS pattern,
       CASE WHEN starts_with(path, '/Users/aloksubbarao/.codex/') THEN 'codex_lines'
            WHEN suffix(path, '.meta.json') THEN 'claude_meta' ELSE 'claude_lines' END AS target
FROM read_csv($c$find /Users/aloksubbarao/.claude /Users/aloksubbarao/.codex -type f \( -name '*.jsonl' -o -name '*.jsonl.zst' -o -name '*.meta.json' \) |$c$,
              header = false, columns = {'path': 'VARCHAR'}, delim = '\t', quote = '', escape = '');

-- the bounded set: bounded_files.csv (smallest file per record kind, from the content scans, plus the
-- chosen sessions' subagent folders) and the 4 smallest compressed Codex rollouts. full = every folder.
CREATE OR REPLACE TEMP TABLE scope AS
SELECT s.* FROM stage1 s
WHERE CASE coalesce(nullif(getenv('ALT_SCOPE'), ''), 'bounded') WHEN 'full' THEN true
      ELSE s.path IN (SELECT path FROM read_csv('/private/tmp/claude-501/agent_data_alt/bounded_files.csv')
                      UNION ALL SELECT path FROM (SELECT path FROM stage1 WHERE pattern = '*.jsonl.zst' ORDER BY bytes LIMIT 4)) END;

-- one source per directory: the folder glob (full), or that folder's chosen files (bounded; duckorch
-- takes a single path string, so bounded duckorch gets one call per chosen file)
CREATE OR REPLACE TEMP TABLE dirs AS
SELECT CASE WHEN getenv('ALT_LANE') = 'orch' AND coalesce(nullif(getenv('ALT_SCOPE'), ''), 'bounded') = 'bounded'
            THEN path ELSE dir END AS dir,
       pattern, target, sum(bytes) AS bytes,
       CASE WHEN coalesce(nullif(getenv('ALT_SCOPE'), ''), 'bounded') = 'full' THEN printf('''%s/%s''', any_value(dir), pattern)
            WHEN getenv('ALT_LANE') = 'orch' THEN printf('''%s''', any_value(path))
            ELSE replace(to_json(list(path ORDER BY path))::VARCHAR, '"', '''') END AS src
FROM scope
GROUP BY CASE WHEN getenv('ALT_LANE') = 'orch' AND coalesce(nullif(getenv('ALT_SCOPE'), ''), 'bounded') = 'bounded'
              THEN path ELSE dir END, pattern, target;

-- one statement per directory, posted to this process's /sql one at a time: list_transform runs the
-- posts in list order on one thread (a plain SELECT over the rows would post from several threads at once)
CREATE OR REPLACE TEMP TABLE receipts AS
WITH program AS (
  SELECT list(struct_pack(dir := dir, pattern := pattern, target := target, bytes := bytes,
                          stmt := CASE getenv('ALT_LANE')
           WHEN 'orch' THEN printf($s$PRAGMA orch_ingest_run(%s, 'orch.%s', disposition = 'append')$s$, src, target)
           ELSE printf($s$INSERT INTO raw.ingest.%s SELECT to_json(t) FROM read_json(%s, format = 'newline_delimited', records = 'false', columns = {json: 'JSON'}, filename = true) t$s$, target, src)
           END) ORDER BY bytes, dir, pattern) AS steps
  FROM dirs
), posted AS (
  SELECT unnest(list_transform(steps, x -> struct_pack(dir := x.dir, pattern := x.pattern, target := x.target, bytes := x.bytes,
           receipt := http_post('http://127.0.0.1:9515/sql', MAP {'content-type': 'application/json'}, json_object('sql', x.stmt))))) AS r
  FROM program
)
SELECT r.dir, r.pattern, r.target, r.bytes, r.receipt FROM posted;

SELECT target, pattern, count(dir) AS dirs, round(sum(bytes) / 1e6, 1) AS mb,
       count(dir) FILTER (WHERE (receipt->>'status')::INT = 200) AS ok
FROM receipts GROUP BY target, pattern ORDER BY target, pattern;
SELECT dir, pattern, receipt->>'status' AS status, left(receipt->>'body', 300) AS body
FROM receipts WHERE (receipt->>'status')::INT <> 200;
CHECKPOINT;
