-- ingest.sql: Claude Code + Codex transcripts through duckorch's own JSON ingest.
-- find -> stage (one NDJSON per source: top-level scalars as they are, every top-level object/array
-- as JSON text, plus _file/_line) -> orch_ingest_run, merged on (_file, _line).
-- Its own disposable process and file, never dev:
--   cd /private/tmp/claude-501/agent_data_duckorch && SCOPE=bounded duckdb -bail bounded.duckdb -f <this file>
--   cd /private/tmp/claude-501/agent_data_duckorch && SCOPE=full    duckdb -bail full.duckdb    -f <this file>
-- -bail matters: a failed staging COPY leaves partial files that the PRAGMAs would otherwise load.

SET threads = 4;
SET memory_limit = '4GB';
SET preserve_insertion_order = false;
SET temp_directory = '/private/tmp/claude-501/agent_data_duckorch/spill';   -- ~/.duckdbrc's is shared
SET partitioned_write_flush_threshold = 1024;   -- rows held per partition before a flush (default 524,288 x ~5 KB lines)
LOAD shellfs;
LOAD hostfs;
LOAD scalarfs;
SET scalarfs_max_decompressed_bytes = 1073741824;                          -- one .zst rollout, inflated
LOAD read_lines;
LOAD duckorch;
PRAGMA orch_init;

-- stage 1: every .jsonl / .jsonl.zst under both roots
CREATE OR REPLACE TABLE files AS
SELECT rtrim(content, e'\n') AS path, file_size(path) AS bytes, file_last_modified(path) AS modified
FROM read_lines($c$find /Users/aloksubbarao/.claude /Users/aloksubbarao/.codex -type f -name *.jsonl -o -type f -name *.jsonl.zst |$c$);

-- this run's paths: all, or the bounded set (about 1 in 50 by path hash, 1 KB to 3 MB).
-- read_lines numbers lines per file as it streams; .zst goes through scalarfs' zstd wrapper.
COPY (SELECT CASE WHEN suffix(path, '.zst') THEN 'decompress+zstd:' || path ELSE path END
      FROM files
      WHERE getenv('SCOPE') = 'full' OR (bytes BETWEEN 1000 AND 3000000 AND hash(path) % 50 = 0))
TO 'variable:files' (FORMAT variable, LIST scalar);

-- stage 2: one rule for every line. A file is a transcript by content, read from its first 20 lines
-- (Codex: session_meta / response_item; Claude: sessionId + type); plugins, jobs, history are not staged.
COPY (
WITH kind AS (
    SELECT file_path,
           CASE WHEN bool_or(json_extract_string(content, 'type') IN ('session_meta', 'response_item')) THEN 'codex'
                WHEN bool_or(json_extract_string(content, 'sessionId') IS NOT NULL
                             AND json_extract_string(content, 'type') IS NOT NULL) THEN 'claude' END AS src
    FROM read_lines('pathvariable:files', lines := '1-20', "trim" := 'endings')
    GROUP BY file_path
)
SELECT src, rec,
       -- duckorch's read_json keeps maximum_object_size at 16 MiB and takes no read options, so one
       -- longer line fails a whole load: those lines (5 Codex `compacted` records) wait in held=true
       strlen(rec::VARCHAR) >= 16777216 AS held
FROM (
    SELECT kind.src,
           to_json(map_from_entries(
             list_transform(map_entries(l.content::JSON::MAP(VARCHAR, JSON)),
                            e -> {k: e.key, v: CASE WHEN json_type(e.value) IN ('OBJECT', 'ARRAY') THEN to_json(e.value::VARCHAR) ELSE e.value END})
             || [{k: '_file', v: to_json(replace(l.file_path, 'decompress+zstd:', ''))}, {k: '_line', v: to_json(l.line_number)}])) AS rec
    FROM read_lines('pathvariable:files', "trim" := 'endings') l
    JOIN kind USING (file_path)
    WHERE kind.src IS NOT NULL AND json_type(l.content) = 'OBJECT'
)
) TO '/private/tmp/claude-501/agent_data_duckorch/stage'
  (FORMAT csv, HEADER false, QUOTE '', ESCAPE '', DELIMITER e'\x1f', PARTITION_BY (src, held),
   FILE_EXTENSION 'jsonl', OVERWRITE_OR_IGNORE true);

-- stage 3: duckorch types the columns, versions the schema and merges on the line key.
-- Its read_json auto-detects hive partitions, so src and held arrive as two extra columns.
PRAGMA orch_ingest_run('/private/tmp/claude-501/agent_data_duckorch/stage/src=claude/held=false/*.jsonl', 'main.claude',
                       disposition = 'merge', primary_key = '_file,_line');
PRAGMA orch_ingest_run('/private/tmp/claude-501/agent_data_duckorch/stage/src=codex/held=false/*.jsonl', 'main.codex',
                       disposition = 'merge', primary_key = '_file,_line');
CHECKPOINT;
