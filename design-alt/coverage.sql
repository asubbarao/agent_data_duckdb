-- coverage.sql: RawDuck tables vs dev's read_conversations (agent_data 54d875a), bounded set.
-- rc_bounded.parquet = read_conversations rows for the same .jsonl files, fetched read-only from dev /sql.
LOAD rawduck;
LOAD duck_diff;
ATTACH 'rawduck:/private/tmp/claude-501/agent_data_alt/raw.db' AS raw (READ_ONLY);

-- which fields each side has
SELECT 'read_conversations' AS side, count(column_name) AS columns FROM (DESCRIBE FROM '/private/tmp/claude-501/agent_data_alt/rc_bounded.parquet')
UNION ALL SELECT 'raw.claude_lines', count(column_name) FROM (DESCRIBE raw.claude_lines)
UNION ALL SELECT 'raw.codex_lines', count(column_name) FROM (DESCRIBE raw.codex_lines);

-- row-level agreement on a natural key: Claude uuid, Codex (file, line) with line = ordinal + 1
FROM table_diff_summary(
  $q$SELECT "json.uuid" AS uuid, "json.timestamp" AS ts, "json.message.model" AS model FROM raw.claude_lines
     WHERE "json.uuid" IS NOT NULL AND filename IN (FROM read_csv('/private/tmp/claude-501/agent_data_alt/bounded_files.csv') SELECT path)$q$,
  $q$SELECT DISTINCT uuid, timestamp::TIMESTAMP AS ts, model FROM '/private/tmp/claude-501/agent_data_alt/rc_bounded.parquet'
     WHERE source = 'claude' AND uuid IS NOT NULL$q$,
  pk := 'uuid');
FROM table_diff_summary(
  $q$SELECT filename AS file, "json.ordinal" + 1 AS line, "json.timestamp" AS ts FROM raw.codex_lines
     WHERE filename IN (FROM read_csv('/private/tmp/claude-501/agent_data_alt/bounded_files.csv') SELECT path)$q$,
  $q$SELECT DISTINCT file_path AS file, line_number AS line, timestamp::TIMESTAMP AS ts FROM '/private/tmp/claude-501/agent_data_alt/rc_bounded.parquet'
     WHERE source = 'codex'$q$,
  pk := ['file', 'line']);

-- Claude token overcount: read_conversations repeats a response's usage on every content-block line
WITH rc AS (SELECT sum(output_tokens) AS rc_output FROM '/private/tmp/claude-501/agent_data_alt/rc_bounded.parquet' WHERE source = 'claude'),
     per_response AS (SELECT "json.message.id" AS response_id, arg_max("json.message.usage.output_tokens", "json.timestamp") AS output_tokens
                      FROM raw.claude_lines WHERE "json.type" = 'assistant' AND "json.message.id" IS NOT NULL
                        AND filename IN (FROM read_csv('/private/tmp/claude-501/agent_data_alt/bounded_files.csv') SELECT path)
                      GROUP BY "json.message.id"),
     raw AS (SELECT sum(output_tokens) AS actual_output, count(response_id) AS responses FROM per_response)
SELECT rc_output, actual_output, round(rc_output / actual_output, 2) AS overcount, responses FROM rc CROSS JOIN raw;

-- fill rates: columns that are never filled (no non-empty value) per table
SELECT 'claude_lines' AS t, count(column_name) AS columns, count(column_name) FILTER (WHERE null_percentage = 100) AS dead
FROM (SUMMARIZE SELECT nullif(trim(COLUMNS(*)::VARCHAR), '') FROM raw.claude_lines)
UNION ALL
SELECT 'codex_lines', count(column_name), count(column_name) FILTER (WHERE null_percentage = 100)
FROM (SUMMARIZE SELECT nullif(trim(COLUMNS(*)::VARCHAR), '') FROM raw.codex_lines)
UNION ALL
SELECT 'claude_meta', count(column_name), count(column_name) FILTER (WHERE null_percentage = 100)
FROM (SUMMARIZE SELECT nullif(trim(COLUMNS(*)::VARCHAR), '') FROM raw.claude_meta);
