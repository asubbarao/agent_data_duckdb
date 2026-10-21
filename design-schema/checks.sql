-- Checks over the proof output (out/<view>/batch=k/*.parquet written by prove.sql). Disposable duckdb only.
--   cd /private/tmp/claude-501/agent_data_schema/out && duckdb -markdown -f checks.sql
SET threads = 4; SET memory_limit = '4GB';

-- 1. rows per view per source
SELECT 'lines' AS view, source, count(line) AS rows, count(DISTINCT file) AS files FROM 'lines/*/*.parquet' GROUP BY ALL UNION ALL
SELECT 'tool_calls/' || level, source, count(line), count(DISTINCT file) FROM 'tool_calls/*/*.parquet' GROUP BY ALL UNION ALL
SELECT 'usage', source, count(line), count(DISTINCT file) FROM 'usage/*/*.parquet' GROUP BY ALL UNION ALL
SELECT 'messages', source, count(line), count(DISTINCT file) FROM 'messages/*/*.parquet' GROUP BY ALL UNION ALL
SELECT 'thinking', source, count(line), count(DISTINCT file) FROM 'thinking/*/*.parquet' GROUP BY ALL UNION ALL
SELECT 'attachments', 'claude', count(line), count(DISTINCT file) FROM 'attachments/*/*.parquet' UNION ALL
SELECT 'system_events', source, count(line), count(DISTINCT file) FROM 'system_events/*/*.parquet' GROUP BY ALL UNION ALL
SELECT 'sessions', source, count(file), count(DISTINCT file) FROM 'sessions/*/*.parquet' GROUP BY ALL
ORDER BY 1, 2;

-- 2. dead columns: per view, SUMMARIZE over trimmed text (empty string counts as NULL)
SELECT 'tool_calls' AS view, column_name, approx_unique, null_percentage FROM (SUMMARIZE SELECT nullif(trim(COLUMNS(* EXCLUDE (batch))::VARCHAR), '') FROM 'tool_calls/*/*.parquet');
SELECT 'usage' AS view, column_name, approx_unique, null_percentage FROM (SUMMARIZE SELECT nullif(trim(COLUMNS(* EXCLUDE (batch))::VARCHAR), '') FROM 'usage/*/*.parquet');
SELECT 'lines' AS view, column_name, approx_unique, null_percentage FROM (SUMMARIZE SELECT nullif(trim(COLUMNS(* EXCLUDE (batch))::VARCHAR), '') FROM 'lines/*/*.parquet');
SELECT 'messages' AS view, column_name, approx_unique, null_percentage FROM (SUMMARIZE SELECT nullif(trim(COLUMNS(* EXCLUDE (batch))::VARCHAR), '') FROM 'messages/*/*.parquet');
SELECT 'thinking' AS view, column_name, approx_unique, null_percentage FROM (SUMMARIZE SELECT nullif(trim(COLUMNS(* EXCLUDE (batch))::VARCHAR), '') FROM 'thinking/*/*.parquet');
SELECT 'attachments' AS view, column_name, approx_unique, null_percentage FROM (SUMMARIZE SELECT nullif(trim(COLUMNS(* EXCLUDE (batch))::VARCHAR), '') FROM 'attachments/*/*.parquet');
SELECT 'system_events' AS view, column_name, approx_unique, null_percentage FROM (SUMMARIZE SELECT nullif(trim(COLUMNS(* EXCLUDE (batch))::VARCHAR), '') FROM 'system_events/*/*.parquet');
SELECT 'sessions' AS view, column_name, approx_unique, null_percentage FROM (SUMMARIZE SELECT nullif(trim(COLUMNS(* EXCLUDE (batch))::VARCHAR), '') FROM 'sessions/*/*.parquet');

-- 3. call <-> result pairing, and the per-source fill of the analysis columns
SELECT source, level, count(line) AS calls, count(result_line) AS paired,
       round(100 * count(result_line) / count(line), 2) AS pct_paired,
       count(response_id) AS with_response, count(turn_id) AS with_turn, count(is_error) AS with_is_error,
       count(duration_ms) AS with_duration, sum(input_bytes) AS input_bytes, sum(result_bytes) AS result_bytes
FROM 'tool_calls/*/*.parquet' GROUP BY ALL ORDER BY 1, 2;

-- 4. copies (the cross-file view, here over the proof output): same call / response in several files
WITH copies AS (
    SELECT 'tool_call' AS kind, source, call_id AS id, file, line,
           row_number() OVER (PARTITION BY source, call_id ORDER BY ts, file, line) = 1 AS first_seen
    FROM 'tool_calls/*/*.parquet'
    UNION ALL
    SELECT 'response', source, response_id, file, line,
           row_number() OVER (PARTITION BY source, response_id ORDER BY ts, file, line) = 1
    FROM 'usage/*/*.parquet' WHERE response_id IS NOT NULL)
SELECT kind, source, count(id) AS rows, count(id) FILTER (WHERE first_seen) AS first_seen,
       count(id) FILTER (WHERE NOT first_seen) AS copies, count(DISTINCT file) FILTER (WHERE NOT first_seen) AS files_with_copies
FROM copies GROUP BY ALL ORDER BY 1, 2;

-- 5. tokens: Claude one row per (file, message.id); Codex one row per token_usage_record (or per-turn
--    last_token_usage fallback). all rows vs first_seen only.
WITH u AS (
    SELECT *, row_number() OVER (PARTITION BY source, coalesce(response_id, file || line) ORDER BY ts, file, line) = 1 AS first_seen
    FROM 'usage/*/*.parquet')
SELECT source, first_seen, count(line) AS responses, sum(total_input_tokens) AS total_input, sum(cache_read_tokens) AS cache_read,
       sum(cache_write_tokens) AS cache_write, sum(output_tokens) AS output, sum(reasoning_tokens) AS reasoning
FROM u GROUP BY ALL ORDER BY 1, 2;

-- 6. Codex usage source: token_usage_record vs token_count fallback
SELECT source, response_id IS NOT NULL AS from_token_usage_record, count(line) AS rows, count(DISTINCT file) AS files
FROM 'usage/*/*.parquet' GROUP BY ALL ORDER BY 1, 2;
