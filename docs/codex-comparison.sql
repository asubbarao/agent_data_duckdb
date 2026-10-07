-- Reproducible comparison for one frozen Codex corpus.
--
-- Run in a fresh DuckDB process with the rebuilt extension loaded. The old
-- extension is represented by its existing frozen Parquet result because both
-- extension artifacts cannot be loaded in one process. This file only issues
-- SELECT statements: it creates no table, view, COPY output, or sidecar.
--
-- Replace the three paths, then run with `duckdb -unsigned -f` when the local
-- build is unsigned. SQL NULL remains NULL; empty strings are counted separately.

SET memory_limit = '16GB';
INSTALL read_lines FROM community;
LOAD read_lines;
LOAD '/absolute/path/to/after/agent_data.duckdb_extension';

SUMMARIZE
SELECT source, session_id, project_path, project_dir, file_name, is_agent,
       line_number, message_type, uuid, parent_uuid, timestamp, message_role,
       message_content, model, tool_name, tool_use_id, tool_input, input_tokens,
       output_tokens, cache_creation_tokens, cache_read_tokens, reasoning_tokens,
       slug, git_branch, cwd, version, stop_reason, reasoning_effort, repository,
       file_path, client
FROM read_conversations(
  path := '/absolute/path/to/frozen-codex-home',
  source := 'codex',
  include_archived := true
);

-- The new reader is projected to exactly the scalar schema frozen by the old
-- artifact. No raw_event or metadata is carried through this comparison.
WITH
before_rows AS (
  SELECT *
  FROM read_parquet('/absolute/path/to/artifacts/codex-before.parquet')
),
after_rows AS (
  SELECT source, session_id, project_path, project_dir, file_name, is_agent,
         line_number, message_type, uuid, parent_uuid, timestamp, message_role,
         message_content, model, tool_name, tool_use_id, tool_input, input_tokens,
         output_tokens, cache_creation_tokens, cache_read_tokens, reasoning_tokens,
         slug, git_branch, cwd, version, stop_reason, reasoning_effort, repository,
         file_path, client
  FROM read_conversations(
    path := '/absolute/path/to/frozen-codex-home',
    source := 'codex',
    include_archived := true
  )
),
before_ranked AS (
  SELECT *,
         row_number() OVER (
           PARTITION BY file_name, line_number
           ORDER BY uuid NULLS FIRST, message_content NULLS FIRST
         ) AS match_ordinal
  FROM before_rows
),
after_ranked AS (
  SELECT *,
         row_number() OVER (
           PARTITION BY file_name, line_number
           ORDER BY uuid NULLS FIRST, message_content NULLS FIRST
         ) AS match_ordinal
  FROM after_rows
),
aligned AS (
  SELECT CASE
           WHEN b.file_name IS NULL THEN 'newly_recovered'
           WHEN a.file_name IS NULL THEN 'missing_after'
           ELSE 'matched'
         END AS match_class,
         a.client,
         CASE WHEN a.file_name IS NULL THEN b.message_type ELSE a.message_type END
           AS message_type,
         b,
         a
  FROM before_ranked AS b
  FULL JOIN after_ranked AS a
    ON b.file_name = a.file_name
   AND b.line_number = a.line_number
   AND b.match_ordinal = a.match_ordinal
),
cell_values AS (
  SELECT match_class, client, message_type, cell
  FROM aligned,
       UNNEST([
         struct_pack(column_name := 'source', before_value := b.source, after_value := a.source),
         struct_pack(column_name := 'session_id', before_value := b.session_id, after_value := a.session_id),
         struct_pack(column_name := 'project_path', before_value := b.project_path, after_value := a.project_path),
         struct_pack(column_name := 'project_dir', before_value := b.project_dir, after_value := a.project_dir),
         struct_pack(column_name := 'file_name', before_value := b.file_name, after_value := a.file_name),
         struct_pack(column_name := 'is_agent', before_value := b.is_agent::VARCHAR, after_value := a.is_agent::VARCHAR),
         struct_pack(column_name := 'line_number', before_value := b.line_number::VARCHAR, after_value := a.line_number::VARCHAR),
         struct_pack(column_name := 'message_type', before_value := b.message_type, after_value := a.message_type),
         struct_pack(column_name := 'uuid', before_value := b.uuid, after_value := a.uuid),
         struct_pack(column_name := 'parent_uuid', before_value := b.parent_uuid, after_value := a.parent_uuid),
         struct_pack(column_name := 'timestamp', before_value := b.timestamp, after_value := a.timestamp),
         struct_pack(column_name := 'message_role', before_value := b.message_role, after_value := a.message_role),
         struct_pack(column_name := 'message_content', before_value := b.message_content, after_value := a.message_content),
         struct_pack(column_name := 'model', before_value := b.model, after_value := a.model),
         struct_pack(column_name := 'tool_name', before_value := b.tool_name, after_value := a.tool_name),
         struct_pack(column_name := 'tool_use_id', before_value := b.tool_use_id, after_value := a.tool_use_id),
         struct_pack(column_name := 'tool_input', before_value := b.tool_input, after_value := a.tool_input),
         struct_pack(column_name := 'input_tokens', before_value := b.input_tokens::VARCHAR, after_value := a.input_tokens::VARCHAR),
         struct_pack(column_name := 'output_tokens', before_value := b.output_tokens::VARCHAR, after_value := a.output_tokens::VARCHAR),
         struct_pack(column_name := 'cache_creation_tokens', before_value := b.cache_creation_tokens::VARCHAR, after_value := a.cache_creation_tokens::VARCHAR),
         struct_pack(column_name := 'cache_read_tokens', before_value := b.cache_read_tokens::VARCHAR, after_value := a.cache_read_tokens::VARCHAR),
         struct_pack(column_name := 'reasoning_tokens', before_value := b.reasoning_tokens::VARCHAR, after_value := a.reasoning_tokens::VARCHAR),
         struct_pack(column_name := 'slug', before_value := b.slug, after_value := a.slug),
         struct_pack(column_name := 'git_branch', before_value := b.git_branch, after_value := a.git_branch),
         struct_pack(column_name := 'cwd', before_value := b.cwd, after_value := a.cwd),
         struct_pack(column_name := 'version', before_value := b.version, after_value := a.version),
         struct_pack(column_name := 'stop_reason', before_value := b.stop_reason, after_value := a.stop_reason),
         struct_pack(column_name := 'reasoning_effort', before_value := b.reasoning_effort, after_value := a.reasoning_effort),
         struct_pack(column_name := 'repository', before_value := b.repository, after_value := a.repository)
       ]) AS cells(cell)
)
SELECT CASE
         WHEN grouping(client) = 0 THEN 'by_client_message'
         WHEN grouping(cell.column_name) = 0 THEN 'by_column'
         WHEN grouping(match_class) = 0 THEN 'by_match_class'
         ELSE 'overall'
       END AS report_scope,
       client,
       message_type,
       match_class,
       cell.column_name AS column_name,
       count(cell.column_name) AS cells,
       count(cell.column_name) FILTER (WHERE cell.before_value IS NULL) AS before_null_cells,
       count(cell.column_name) FILTER (WHERE cell.after_value IS NULL) AS after_null_cells,
       count(cell.column_name) FILTER (WHERE cell.before_value = '') AS before_empty_cells,
       count(cell.column_name) FILTER (WHERE cell.after_value = '') AS after_empty_cells,
       count(cell.column_name) FILTER (
         WHERE cell.before_value IS NULL AND cell.after_value IS NOT NULL
       ) AS recovered_from_null_cells,
       count(cell.column_name) FILTER (
         WHERE match_class = 'matched'
           AND cell.before_value IS NOT NULL
           AND cell.after_value IS NOT NULL
           AND cell.before_value IS DISTINCT FROM cell.after_value
       ) AS changed_matched_cells,
       count(cell.column_name) FILTER (
         WHERE match_class = 'matched'
           AND cell.before_value IS NOT DISTINCT FROM cell.after_value
       ) AS unchanged_matched_cells
FROM cell_values
GROUP BY GROUPING SETS (
  (client, message_type, match_class, cell.column_name),
  (match_class, cell.column_name),
  (match_class),
  ()
)
ORDER BY report_scope, client, message_type, match_class, column_name;

-- Separate, bounded provenance probe. It checks raw-event fidelity for a
-- representative fixed-size set and never feeds the scalar-comparison rates.
-- A full source-backed recovery rate requires a deliberately scoped source
-- extraction for the event shapes under review.
WITH raw_sample AS (
  SELECT file_path, line_number, raw_event
  FROM read_conversations(
    path := '/absolute/path/to/frozen-codex-home',
    source := 'codex',
    include_archived := true
  )
  WHERE raw_event IS NOT NULL
  ORDER BY file_path, line_number
  LIMIT 1000
),
source_lines AS (
  SELECT file_path,
         line_number,
         CASE
           WHEN ends_with(content, chr(13) || chr(10)) THEN left(content, length(content) - 2)
           WHEN ends_with(content, chr(10)) THEN left(content, length(content) - 1)
           ELSE content
         END AS raw_line
  FROM read_lines('/absolute/path/to/frozen-codex-home/**/*.jsonl')
)
SELECT count(raw_sample.file_path) AS sampled_rows,
       count(raw_sample.file_path) FILTER (
         WHERE raw_sample.raw_event = source_lines.raw_line
       ) AS exact_raw_rows,
       count(raw_sample.file_path) FILTER (
         WHERE raw_sample.raw_event IS DISTINCT FROM source_lines.raw_line
       ) AS nonexact_raw_rows
FROM raw_sample
LEFT JOIN source_lines
  ON raw_sample.file_path = source_lines.file_path
 AND raw_sample.line_number = source_lines.line_number;
