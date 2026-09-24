-- Reproducible comparison for one frozen Codex corpus.
--
-- Run in a fresh DuckDB process with the rebuilt extension loaded. The old
-- extension is represented only by its existing frozen Parquet result because
-- DuckDB cannot load both extension artifacts in one process. Replace the
-- paths, then run with `duckdb -unsigned -f` when the local build is unsigned.
-- This program emits SELECT results only; it writes no comparison data artifact.
--
-- SQL NULL remains NULL. Empty strings are separately counted and JSON null is
-- never treated as the string 'null'.

SET memory_limit = '16GB';
INSTALL read_lines FROM community;
LOAD read_lines;
LOAD '/absolute/path/to/after/agent_data.duckdb_extension';

-- The new reader is projected to the legacy schema. file_path, client, and
-- raw_event are retained solely for identity and direct-source verification.
CREATE OR REPLACE TEMP TABLE comparison_before AS
SELECT *
FROM read_parquet('/absolute/path/to/artifacts/codex-before.parquet');

CREATE OR REPLACE TEMP TABLE comparison_after AS
SELECT source, session_id, project_path, project_dir, file_name, is_agent,
       line_number, message_type, uuid, parent_uuid, timestamp, message_role,
       message_content, model, tool_name, tool_use_id, tool_input, input_tokens,
       output_tokens, cache_creation_tokens, cache_read_tokens, reasoning_tokens,
       slug, git_branch, cwd, version, stop_reason, reasoning_effort, repository,
       file_path, client, raw_event
FROM read_conversations(
  path := '/absolute/path/to/frozen-codex-home',
  source := 'codex',
  include_archived := true
);

SUMMARIZE comparison_before;
SUMMARIZE comparison_after;

SELECT 'before' AS artifact, NULL::VARCHAR AS client, message_type,
       count(source) AS rows
FROM comparison_before
GROUP BY ALL
UNION ALL
SELECT 'after' AS artifact, client, message_type, count(source) AS rows
FROM comparison_after
GROUP BY ALL
ORDER BY artifact, client, message_type;

-- Filename plus physical line was the shared identity in the old artifact.
-- The ordinal makes duplicate normalized records on one line stable.
CREATE OR REPLACE TEMP TABLE comparison_matched AS
WITH
before_ranked AS (
  SELECT *,
         row_number() OVER (
           PARTITION BY file_name, line_number
           ORDER BY uuid NULLS FIRST, message_content NULLS FIRST
         ) AS match_ordinal
  FROM comparison_before
),
after_ranked AS (
  SELECT *,
         row_number() OVER (
           PARTITION BY file_name, line_number
           ORDER BY uuid NULLS FIRST, message_content NULLS FIRST
         ) AS match_ordinal
  FROM comparison_after
)
SELECT CASE
         WHEN b.file_name IS NULL THEN 'newly_recovered'
         WHEN a.file_name IS NULL THEN 'missing_after'
         ELSE 'matched'
       END AS match_class,
       a.client,
       CASE WHEN a.file_name IS NULL THEN b.message_type ELSE a.message_type END
         AS message_type,
       a.file_path AS after_file_path,
       a.line_number AS after_line_number,
       a.raw_event,
       to_json(b) AS before_json,
       to_json(a.* EXCLUDE (file_path, client, raw_event, match_ordinal)) AS after_json
FROM before_ranked AS b
FULL JOIN after_ranked AS a
  ON b.file_name = a.file_name
 AND b.line_number = a.line_number
 AND b.match_ordinal = a.match_ordinal;

SELECT match_class, count(match_class) AS rows
FROM comparison_matched
GROUP BY ALL
ORDER BY match_class;

-- raw_event is source evidence only if it exactly matches the physical JSONL
-- source record after the record's CRLF/LF terminator is removed.
CREATE OR REPLACE TEMP TABLE comparison_source AS
WITH source_lines AS (
  SELECT file_path,
         line_number,
         CASE
           WHEN ends_with(content, chr(13) || chr(10)) THEN left(content, length(content) - 2)
           WHEN ends_with(content, chr(10)) THEN left(content, length(content) - 1)
           ELSE content
         END AS raw_line
  FROM read_lines('/absolute/path/to/frozen-codex-home/**/*.jsonl')
)
SELECT matched.*,
       source_lines.raw_line,
       matched.raw_event IS NOT NULL
         AND matched.raw_event = source_lines.raw_line AS source_record_exact
FROM comparison_matched AS matched
LEFT JOIN source_lines
  ON matched.after_file_path = source_lines.file_path
 AND matched.after_line_number = source_lines.line_number;

SELECT source_record_exact, count(after_file_path) AS after_rows
FROM comparison_source
WHERE after_file_path IS NOT NULL
GROUP BY ALL
ORDER BY source_record_exact;

-- Only these legacy fields have a direct scalar in the physical event. A source
-- denominator exists only for an exact record with an observed source scalar.
-- Session and propagated metadata intentionally have no source-backed rate.
CREATE OR REPLACE TEMP TABLE comparison_cells AS
WITH
source_values AS (
  SELECT *,
         CASE WHEN source_record_exact AND json_valid(raw_line)
           THEN json_extract_string(raw_line, '$.timestamp')
         END AS source_timestamp,
         CASE WHEN source_record_exact AND json_valid(raw_line) THEN CASE
           WHEN json_extract_string(raw_line, '$.type') = 'response_item'
             THEN json_extract_string(raw_line, '$.payload.id')
           WHEN json_extract_string(raw_line, '$.payload.type') = 'item_completed'
             THEN json_extract_string(raw_line, '$.payload.item.id')
         END END AS source_uuid,
         CASE WHEN source_record_exact AND json_valid(raw_line) THEN CASE
           WHEN json_extract_string(raw_line, '$.type') = 'response_item'
             THEN json_extract_string(raw_line, '$.payload.name')
           WHEN json_extract_string(raw_line, '$.payload.type') = 'item_completed'
             THEN json_extract_string(raw_line, '$.payload.item.name')
         END END AS source_tool_name,
         CASE WHEN source_record_exact AND json_valid(raw_line) THEN CASE
           WHEN json_extract_string(raw_line, '$.type') = 'response_item'
             THEN json_extract_string(raw_line, '$.payload.call_id')
           WHEN json_extract_string(raw_line, '$.payload.type') = 'item_completed'
             THEN json_extract_string(raw_line, '$.payload.item.call_id')
         END END AS source_tool_use_id,
         CASE WHEN source_record_exact AND json_valid(raw_line) THEN CASE
           WHEN json_type(raw_line, '$.payload.arguments') = 'VARCHAR'
             THEN json_extract_string(raw_line, '$.payload.arguments')
           WHEN json_type(raw_line, '$.payload.input') = 'VARCHAR'
             THEN json_extract_string(raw_line, '$.payload.input')
           WHEN json_type(raw_line, '$.payload.arguments') IN ('ARRAY', 'OBJECT')
             THEN json_extract(raw_line, '$.payload.arguments')::VARCHAR
           WHEN json_type(raw_line, '$.payload.input') IN ('ARRAY', 'OBJECT')
             THEN json_extract(raw_line, '$.payload.input')::VARCHAR
         END END AS source_tool_input,
         CASE WHEN source_record_exact AND json_valid(raw_line)
           AND json_extract_string(raw_line, '$.type') = 'response_item'
           THEN json_extract_string(raw_line, '$.payload.role')
         END AS source_message_role,
         CASE WHEN source_record_exact AND json_valid(raw_line) THEN CASE
           WHEN json_extract_string(raw_line, '$.type') = 'response_item'
             AND json_extract_string(raw_line, '$.payload.type') = 'function_call_output'
             THEN json_extract_string(raw_line, '$.payload.output')
           WHEN json_extract_string(raw_line, '$.type') = 'response_item'
             AND json_extract_string(raw_line, '$.payload.type') = 'reasoning'
             THEN json_extract_string(raw_line, '$.payload.summary[0].text')
           WHEN json_extract_string(raw_line, '$.type') = 'response_item'
             THEN json_extract_string(raw_line, '$.payload.content[0].text')
           WHEN json_extract_string(raw_line, '$.payload.type') IN ('agent_message', 'compaction_summary')
             THEN json_extract_string(raw_line, '$.payload.message')
         END END AS source_message_content,
         CASE WHEN source_record_exact AND json_valid(raw_line)
           THEN json_extract_string(raw_line, '$.payload.usage.input_tokens')
         END AS source_input_tokens,
         CASE WHEN source_record_exact AND json_valid(raw_line)
           THEN json_extract_string(raw_line, '$.payload.usage.output_tokens')
         END AS source_output_tokens,
         CASE WHEN source_record_exact AND json_valid(raw_line)
           THEN json_extract_string(raw_line, '$.payload.usage.cache_creation_tokens')
         END AS source_cache_creation_tokens,
         CASE WHEN source_record_exact AND json_valid(raw_line)
           THEN json_extract_string(raw_line, '$.payload.usage.cache_read_tokens')
         END AS source_cache_read_tokens,
         CASE WHEN source_record_exact AND json_valid(raw_line)
           THEN json_extract_string(raw_line, '$.payload.usage.reasoning_tokens')
         END AS source_reasoning_tokens
  FROM comparison_source
),
column_values AS (
  SELECT source_values.* EXCLUDE (before_json, after_json),
         field.key AS column_name,
         json_extract_string(before_json, '$.' || field.key) AS before_value,
         json_extract_string(after_json, '$.' || field.key) AS after_value
  FROM source_values,
       json_each(CASE WHEN after_json IS NULL THEN before_json ELSE after_json END) AS field
),
source_evidence AS (
  SELECT *,
         CASE column_name
           WHEN 'timestamp' THEN source_timestamp
           WHEN 'uuid' THEN source_uuid
           WHEN 'tool_name' THEN source_tool_name
           WHEN 'tool_use_id' THEN source_tool_use_id
           WHEN 'tool_input' THEN source_tool_input
           WHEN 'message_role' THEN source_message_role
           WHEN 'message_content' THEN source_message_content
           WHEN 'input_tokens' THEN source_input_tokens
           WHEN 'output_tokens' THEN source_output_tokens
           WHEN 'cache_creation_tokens' THEN source_cache_creation_tokens
           WHEN 'cache_read_tokens' THEN source_cache_read_tokens
           WHEN 'reasoning_tokens' THEN source_reasoning_tokens
         END AS source_value
  FROM column_values
),
classified AS (
  SELECT *,
         before_value IS NULL AS before_is_null,
         after_value IS NULL AS after_is_null,
         before_value = '' AS before_is_empty,
         after_value = '' AS after_is_empty,
         source_value IS NOT NULL AS source_observed,
         source_value IS NOT NULL
           AND after_value IS NOT NULL
           AND after_value = source_value AS source_equal
  FROM source_evidence
)
SELECT client, message_type, match_class, column_name,
       count(column_name) AS cells,
       count(column_name) FILTER (WHERE before_is_null) AS before_null_cells,
       count(column_name) FILTER (WHERE after_is_null) AS after_null_cells,
       count(column_name) FILTER (WHERE before_is_empty) AS before_empty_cells,
       count(column_name) FILTER (WHERE after_is_empty) AS after_empty_cells,
       count(column_name) FILTER (WHERE before_is_null AND after_value IS NOT NULL)
         AS recovered_from_null_cells,
       count(column_name) FILTER (
         WHERE match_class = 'matched'
           AND before_value IS NOT NULL
           AND after_value IS NOT NULL
           AND before_value IS DISTINCT FROM after_value
       ) AS changed_matched_cells,
       count(column_name) FILTER (
         WHERE match_class = 'matched'
           AND before_value IS NOT DISTINCT FROM after_value
       ) AS unchanged_matched_cells,
       count(column_name) FILTER (WHERE source_observed) AS source_observed_cells,
       count(column_name) FILTER (WHERE source_equal) AS source_equal_cells,
       count(column_name) FILTER (WHERE before_is_null AND source_observed)
         AS source_present_before_null_cells,
       count(column_name) FILTER (
         WHERE before_is_null AND after_value IS NOT NULL AND source_equal
       ) AS source_verified_recovered_from_null_cells
FROM classified
GROUP BY ALL;

SELECT *
FROM comparison_cells
ORDER BY client, message_type, match_class, column_name;

SELECT sum(before_null_cells) AS previous_null_cells,
       sum(after_null_cells) AS remaining_null_cells,
       sum(recovered_from_null_cells) AS recovered_cells,
       (sum(before_null_cells) - sum(after_null_cells))::DOUBLE
         / nullif(sum(before_null_cells), 0) AS unadjusted_null_reduction,
       sum(source_present_before_null_cells) AS source_present_before_null_cells,
       sum(source_verified_recovered_from_null_cells) AS source_verified_recovered_cells,
       sum(source_verified_recovered_from_null_cells)::DOUBLE
         / nullif(sum(source_present_before_null_cells), 0) AS source_verified_recovery_rate
FROM comparison_cells
WHERE match_class = 'matched';
