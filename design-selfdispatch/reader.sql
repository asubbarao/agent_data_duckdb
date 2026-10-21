-- Self-dispatch transcript reader: find -> PR #1576 entries -> CASE reader -> one statement per
-- directory piece (<= 64 MB of text) sent to dev's /sql. Dev reads the piece and writes its rows
-- to Parquet (quackapi caps a /sql response at 16 MB, so rows do not come back on the wire).
-- Run from a disposable client:  SCOPE=bounded|small|big|full WIDTH=<n> duckdb :memory: -f reader.sql
LOAD shellfs; LOAD hostfs; LOAD read_lines;
SET threads = getenv('WIDTH');  -- read_lines_lateral width = dev requests in flight (~WIDTH, up to ~13)
WITH readers AS (    -- 1576: a file's reader is the catalog's read_<ext>
    SELECT DISTINCT '.' || substr(function_name, 6) AS ext, function_name AS catalog_reader
    FROM duckdb_functions() WHERE function_type = 'table' AND starts_with(function_name, 'read_')),
found AS (SELECT rtrim(content, e'\n') AS path, file_extension(path) AS ext FROM read_lines($c$find /Users/aloksubbarao/.claude /Users/aloksubbarao/.codex -type f -name *.jsonl -o -type f -name *.jsonl.zst |$c$)),
entries AS (         -- 1576's entries columns, from hostfs scalars on the find rows
    SELECT path, ext, parse_dirpath(path) AS parent, file_name(path) AS name,
           parse_filename(path, true) AS stem, path_split(path) AS parts, len(parts) AS depth, path_type(path) AS type,
           file_size(path) AS bytes, file_last_modified(path) AS modified,
           CASE WHEN ends_with(name, '.jsonl.zst') THEN 'read_lines_zstd' WHEN ends_with(name, '.jsonl') THEN 'read_lines'
                ELSE coalesce(catalog_reader, 'read_blob') END AS reader,
           bytes * CASE reader WHEN 'read_lines_zstd' THEN 3 ELSE 1 END AS text_bytes,   -- zstd ratio measured ~3
           CASE reader       -- the statement that reads this file as (file, line, content)
             WHEN 'read_lines'      THEN printf($s$SELECT '%s' AS file, line_number AS line, content FROM read_lines('%s')$s$, path, path)
             WHEN 'read_lines_zstd' THEN printf($s$SELECT '%s' AS file, line_number AS line, content FROM read_lines('zstd -dcq "%s" |')$s$, path, path)
             ELSE printf($s$SELECT '%s' AS file, 1 AS line, to_json(r)::VARCHAR AS content FROM %s('%s') r$s$, path, reader, path) END AS sql_statement
    FROM found LEFT JOIN readers USING (ext)),
dirs AS (            -- directories, cut at 64 MB of text by each file's start (a piece can overrun by its last file)
    SELECT (sum(text_bytes) OVER (PARTITION BY parent ORDER BY path) - text_bytes) // 67108864 AS piece, entries.*
    FROM entries),
statements AS (      -- the piece's program: read, record = the line as JSON, top-level fields as columns
    SELECT parent, piece, printf($s$COPY (SELECT '%s' AS scope, file, line, '%s' AS parent, %d AS piece,
       CASE WHEN contains(file, '/.codex/') THEN 'codex' ELSE 'claude' END AS source,
       CASE WHEN json_valid(content) THEN content::JSON END AS record,
       CASE WHEN NOT json_valid(content) THEN content END AS bad,
       coalesce(record ->> 'sessionId', CASE source WHEN 'codex' THEN right(replace(parse_filename(file, true), '.jsonl', ''), 36) END) AS session_id,
       record ->> 'type' AS type, record -> 'payload' ->> 'type' AS payload_type, record ->> 'timestamp' AS timestamp,
       record ->> 'uuid' AS uuid, record ->> 'parentUuid' AS parent_uuid,
       coalesce(record -> 'message' ->> 'role', record -> 'payload' ->> 'role') AS role,
       coalesce(record -> 'message' ->> 'model', record -> 'payload' ->> 'model') AS model,
       record -> 'message' ->> 'id' AS message_id,
       coalesce(record -> 'message' -> 'usage', record -> 'payload' -> 'info' -> 'last_token_usage') AS usage,
       coalesce(record -> 'payload' ->> 'name', json_extract_string(record, '$.message.content[*].name')::VARCHAR) AS tool_name,
       coalesce(record -> 'payload' ->> 'call_id', json_extract_string(record, '$.message.content[*].id')::VARCHAR) AS tool_id
    FROM (%s)) TO '/private/tmp/claude-501/agent_data_selfdispatch/lines'
    (FORMAT parquet, PARTITION_BY (scope), OVERWRITE_OR_IGNORE, FILENAME_PATTERN 'piece_%s_{i}', ROW_GROUP_SIZE 2048)$s$,
           getenv('SCOPE'), parent, piece, string_agg(sql_statement, ' UNION ALL ' ORDER BY path), md5(parent || piece)) AS sql
    FROM dirs GROUP BY parent, piece
    HAVING CASE getenv('SCOPE') WHEN 'bounded' THEN hash(parent) % 10 = 0 AND sum(text_bytes) < 50000000
                                WHEN 'small' THEN sum(text_bytes) <= 67108864     -- run wide
                                WHEN 'big'   THEN sum(text_bytes) > 67108864      -- run one at a time
                                ELSE true END)
SELECT parent, piece, l.content AS receipt   -- self-dispatch: one curl per piece; receipt [{"Count": rows}] or {"detail": error}
FROM statements CROSS JOIN LATERAL read_lines_lateral(printf($c$curl -sS -H content-type:application/json --data-binary @- http://127.0.0.1:9495/sql <<'BODY'
%s
BODY
|$c$, json_object('sql', sql)::VARCHAR)) l;

-- the reader's result: roughly read_conversations, one row per transcript line
-- FROM read_parquet('/private/tmp/claude-501/agent_data_selfdispatch/lines/*/*.parquet', hive_partitioning := true);
