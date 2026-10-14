-- Mapped value = raw value, for every unified column read_conversations fills
-- from a Claude transcript.
--
-- Left: the columns as read_conversations returns them.
-- Right: the same rows, each column derived again from raw_event (and from the
-- subagent meta.json beside the transcript) in SQL.
-- duck_diff compares the two by key; any difference fails the run.
-- finetype inventories the raw field paths, so a top-level raw field that is
-- neither mapped to a column nor kept in metadata.record fails the run too.
--
-- Run with the extension under test already loaded:
--   duckdb -unsigned -bail -cmd "LOAD 'build/release/agent_data.duckdb_extension'" \
--     -f test/mapping/claude_raw_mapping.sql
-- AGENT_DATA_CLAUDE_HOME points it at another Claude home (default: the fixture).
-- duck_diff and finetype are community extensions, so this runs as its own CI
-- job with the DuckDB CLI rather than inside the sqllogictest suite, which runs
-- on every distribution platform and cannot rely on community builds there.

INSTALL duck_diff FROM community;
LOAD duck_diff;
INSTALL finetype FROM community;
LOAD finetype;

CREATE TEMP VIEW claude_rows AS
SELECT *, TRY_CAST(raw_event AS JSON) AS j
FROM read_conversations(
    source := 'claude',
    path := coalesce(nullif(getenv('AGENT_DATA_CLAUDE_HOME'), ''), 'test/data_claude_unified'));

CREATE TEMP VIEW claude_subagent_meta AS
SELECT replace(filename, '.meta.json', '.jsonl') AS file_path,
       nullif(json->>'agentType', '') AS meta_agent_type,
       nullif(json->>'name', '') AS meta_agent_name,
       nullif(json->>'toolUseId', '') AS meta_tool_use_id
FROM read_json_objects(
    coalesce(nullif(getenv('AGENT_DATA_CLAUDE_HOME'), ''), 'test/data_claude_unified')
        || '/projects/*/*/subagents/*.meta.json',
    filename := true);

CREATE TEMP VIEW claude_mapped AS
SELECT file_path, line_number, record_id,
       event_type, response_id, reasoning_effort, reasoning_tokens, parent_uuid,
       cwd, slug, git_branch, version, client, originator, model_provider,
       turn_id, root_turn_id, thread_source,
       agent_nickname, agent_role, repository, channel, forked_from_session_id,
       session_created_at, session_updated_at, usage_scope, usage_source_line
FROM claude_rows;

CREATE TEMP VIEW claude_derived AS
WITH raw AS (
    SELECT c.*, m.meta_agent_type, m.meta_agent_name, m.meta_tool_use_id,
           j->>'type' AS raw_type,
           nullif(j->>'promptId', '') AS raw_prompt,
           coalesce(nullif(j->>'turnOrigin', ''), nullif(j->'origin'->>'kind', '')) AS raw_origin,
           nullif(timestamp, '') AS raw_ts
    FROM claude_rows c
    LEFT JOIN claude_subagent_meta m USING (file_path)
),
turns AS (
    SELECT *,
           last_value(raw_prompt IGNORE NULLS) OVER file_so_far AS d_turn_id
    FROM raw
    WINDOW file_so_far AS (PARTITION BY file_path ORDER BY line_number ROWS UNBOUNDED PRECEDING)
),
facts AS (
    SELECT *,
           last_value(raw_origin IGNORE NULLS) OVER (
               PARTITION BY file_path, d_turn_id ORDER BY line_number ROWS UNBOUNDED PRECEDING) AS d_thread_source,
           coalesce(last_value(nullif(j->>'cwd', '') IGNORE NULLS) OVER file_so_far,
                    first_value(nullif(j->>'cwd', '') IGNORE NULLS) OVER whole_file) AS d_cwd,
           coalesce(last_value(nullif(j->>'slug', '') IGNORE NULLS) OVER file_so_far,
                    first_value(nullif(j->>'slug', '') IGNORE NULLS) OVER whole_file) AS d_slug,
           coalesce(last_value(nullif(j->>'gitBranch', '') IGNORE NULLS) OVER file_so_far,
                    first_value(nullif(j->>'gitBranch', '') IGNORE NULLS) OVER whole_file) AS d_git_branch,
           coalesce(last_value(nullif(j->>'version', '') IGNORE NULLS) OVER file_so_far,
                    first_value(nullif(j->>'version', '') IGNORE NULLS) OVER whole_file) AS d_version,
           coalesce(last_value(nullif(j->>'entrypoint', '') IGNORE NULLS) OVER file_so_far,
                    first_value(nullif(j->>'entrypoint', '') IGNORE NULLS) OVER whole_file) AS d_entrypoint,
           last_value(CASE WHEN NOT is_agent AND raw_type = 'agent-name' THEN nullif(j->>'agentName', '') END IGNORE NULLS) OVER whole_session AS s_agent_name,
           last_value(CASE WHEN NOT is_agent AND raw_type = 'agent-setting' THEN nullif(j->>'agentSetting', '') END IGNORE NULLS) OVER whole_session AS s_agent_setting,
           last_value(CASE WHEN NOT is_agent AND raw_type = 'pr-link' THEN nullif(j->>'prRepository', '') END IGNORE NULLS) OVER whole_session AS s_repository,
           last_value(CASE WHEN NOT is_agent AND raw_type = 'bridge-session' THEN nullif(j->>'bridgeSessionId', '') END IGNORE NULLS) OVER whole_session AS s_channel,
           first_value(raw_ts IGNORE NULLS) OVER (PARTITION BY session_id ORDER BY raw_ts ASC NULLS LAST ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) AS s_created,
           first_value(raw_ts IGNORE NULLS) OVER (PARTITION BY session_id ORDER BY raw_ts DESC NULLS LAST ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING) AS s_updated
    FROM turns
    WINDOW file_so_far AS (PARTITION BY file_path ORDER BY line_number ROWS UNBOUNDED PRECEDING),
           whole_file AS (PARTITION BY file_path ORDER BY line_number ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING),
           whole_session AS (PARTITION BY session_id ORDER BY file_path, line_number ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING)
),
-- A replayed tool call (rewind, compaction) keeps its first, issuing turn.
tool_turns AS (
    SELECT session_id, tool_use_id, d_turn_id AS parent_turn_id
    FROM turns
    WHERE NOT is_agent AND raw_type = 'assistant' AND tool_use_id IS NOT NULL
    QUALIFY row_number() OVER (PARTITION BY session_id, tool_use_id ORDER BY file_path, line_number) = 1
),
continued AS (
    SELECT j->>'continuedInSessionId' AS session_id, session_id AS continued_from
    FROM raw
    WHERE NOT is_agent AND raw_type = 'continued-in'
    QUALIFY row_number() OVER (PARTITION BY j->>'continuedInSessionId' ORDER BY session_id) = 1
)
SELECT f.file_path, f.line_number, f.record_id,
       CASE WHEN f.raw_type = 'system' THEN coalesce(nullif(f.j->>'subtype', ''), f.raw_type)
            WHEN f.raw_type = 'attachment' THEN coalesce(nullif(f.j->'attachment'->>'type', ''), f.raw_type)
            ELSE f.raw_type END AS event_type,
       CASE WHEN f.message_type IN ('assistant', 'thinking')
            THEN coalesce(nullif(f.j->'message'->>'id', ''), nullif(f.j->>'requestId', '')) END AS response_id,
       coalesce(nullif(f.j->>'perTurnEffort', ''), nullif(f.j->>'effort', '')) AS reasoning_effort,
       CASE WHEN f.output_tokens IS NOT NULL
            THEN TRY_CAST(f.j->'message'->'usage'->'output_tokens_details'->>'thinking_tokens' AS BIGINT) END AS reasoning_tokens,
       coalesce(nullif(f.j->>'parentUuid', ''), nullif(f.j->>'logicalParentUuid', ''), nullif(f.j->>'leafUuid', '')) AS parent_uuid,
       f.d_cwd AS cwd,
       f.d_slug AS slug,
       f.d_git_branch AS git_branch,
       f.d_version AS version,
       f.d_entrypoint AS client,
       f.d_entrypoint AS originator,
       CASE WHEN f.j IS NOT NULL THEN 'anthropic' END AS model_provider,
       f.d_turn_id AS turn_id,
       CASE WHEN f.is_agent THEN t.parent_turn_id ELSE f.d_turn_id END AS root_turn_id,
       f.d_thread_source AS thread_source,
       CASE WHEN f.is_agent THEN f.meta_agent_name ELSE f.s_agent_name END AS agent_nickname,
       CASE WHEN f.is_agent THEN f.meta_agent_type ELSE f.s_agent_setting END AS agent_role,
       f.s_repository AS repository,
       f.s_channel AS channel,
       CASE WHEN NOT f.is_agent THEN k.continued_from END AS forked_from_session_id,
       f.s_created AS session_created_at,
       f.s_updated AS session_updated_at,
       CASE WHEN f.input_tokens IS NOT NULL OR f.output_tokens IS NOT NULL THEN 'response' END AS usage_scope,
       CASE WHEN f.input_tokens IS NOT NULL OR f.output_tokens IS NOT NULL THEN f.line_number END AS usage_source_line
FROM facts f
LEFT JOIN tool_turns t ON t.session_id = f.session_id AND t.tool_use_id = f.meta_tool_use_id
LEFT JOIN continued k ON k.session_id = f.session_id;

-- Column types: the mapped relation has the derived relation's schema.
SELECT CASE WHEN status = 'identical' THEN 'ok'
            ELSE error(printf('schema: %s is %s, expected %s', column_name, left_type, right_type)) END AS schema_check,
       column_name
FROM schema_diff('FROM claude_mapped', 'FROM claude_derived');

-- The rows that differ, for the log, before the assertion below fails.
SELECT * EXCLUDE (diff_data)
FROM table_diff('FROM claude_mapped', 'FROM claude_derived',
                pk := ['file_path', 'line_number', 'record_id'])
WHERE diff_status <> 'identical'
LIMIT 20;

SELECT CASE WHEN n_total = n_identical THEN 'ok'
            ELSE error(printf('mapped != raw on %d of %d rows', n_total - n_identical, n_total)) END AS mapping_check,
       n_identical, n_total
FROM table_diff_summary('FROM claude_mapped', 'FROM claude_derived',
                        pk := ['file_path', 'line_number', 'record_id']);

-- Gaps by record type: a column that is blank on some but not all rows of a
-- message_type is either a field Claude only sometimes writes or a mapping gap.
-- Listed for review; the assertions above and below are what fail the run.
SELECT * FROM (
    UNPIVOT (
        SELECT message_type,
               round(100 * avg((nullif(trim(COLUMNS(* EXCLUDE (message_type, j))::VARCHAR), '') IS NULL)::INT), 1)
        FROM claude_rows
        GROUP BY message_type
    ) ON COLUMNS(* EXCLUDE (message_type)) INTO NAME column_name VALUE pct_blank
)
WHERE pct_blank > 0 AND pct_blank < 100
ORDER BY message_type, column_name;

-- No assistant row is an empty reply: a line whose only block is thinking
-- (redacted or not) is a thinking row.
SELECT error(printf('empty assistant row %s line %d', file_path, line_number)) AS empty_assistant_check
FROM claude_rows
WHERE message_type = 'assistant'
  AND nullif(trim(message_content), '') IS NULL
  AND tool_name IS NULL;

-- Attachments are harness context: none names a tool unless it cites a real
-- tool_use call in the same session.
SELECT error(printf('attachment %s (%s) names %s, not a tool call in its session',
                    a.record_id, a.event_type, a.tool_name)) AS attachment_tool_check
FROM claude_rows a
ANTI JOIN claude_rows t ON t.message_type = 'assistant' AND t.session_id = a.session_id
    AND t.tool_use_id = a.tool_use_id AND t.tool_name = a.tool_name
WHERE a.message_type = 'attachment' AND a.tool_name IS NOT NULL;

-- No unread raw field: every top-level field finetype finds in raw_event is
-- either read into a column or kept in metadata.record. The bulky bodies
-- (message, toolUseResult, attachment, rendered, snapshot) feed
-- message_content/tool_* or stay in raw_event.
CREATE TEMP VIEW claude_raw_fields AS
SELECT c.file_path, c.line_number, c.record_id, f.field,
       ft_unpack(c.raw_event)::JSON -> f.field ->> 'type' AS finetype
FROM claude_rows c
CROSS JOIN UNNEST(json_keys(ft_unpack(c.raw_event)::JSON)) AS f(field)
WHERE c.j IS NOT NULL;

CREATE TEMP VIEW claude_unread_fields AS
SELECT r.field, r.finetype, r.record_id
FROM claude_raw_fields r
JOIN claude_rows c USING (record_id)
WHERE r.field NOT IN ('message', 'content', 'toolUseResult', 'attachment', 'rendered', 'snapshot',
                      'uuid', 'parentUuid', 'sessionId', 'timestamp', 'cwd', 'version',
                      'gitBranch', 'slug', 'type')
  AND NOT coalesce(json_exists(c.metadata, '$.record.' || to_json(r.field)::VARCHAR), false);

SELECT * FROM claude_unread_fields LIMIT 20;

SELECT error(printf('raw field %s (%s) on %s is neither mapped nor kept in metadata.record',
                    field, finetype, record_id)) AS unread_check
FROM claude_unread_fields;

-- The session span columns are the same ISO-8601 timestamps as `timestamp`.
SELECT CASE WHEN (ft_profile(session_created_at)).type = (ft_profile(timestamp)).type
             AND (ft_profile(session_updated_at)).type = (ft_profile(timestamp)).type
            THEN 'ok' ELSE error('session_created_at/updated_at are not typed like timestamp') END AS type_check,
       (ft_profile(timestamp)).type AS timestamp_type
FROM claude_rows;
