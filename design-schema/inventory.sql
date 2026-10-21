-- Inventory of every key path in the transcripts (disposable local duckdb only):
--   cd /private/tmp/claude-501/agent_data_schema && duckdb -f inventory.sql
-- Path = json_tree over json_structure(record): the structure is small (no values) and merges array
-- elements into one [0], so '[0]' -> '[*]' is the whole normalization (no regex). Fill n = lines.
-- Writes design-schema/inventory.csv: one row per (source, record_type, path, json types), with
-- map_path (keys are data: ids, file paths -> collapsed to {*} and kept as JSON) and home (the view
-- column that exposes it, or lines.record).
SET threads = 2; SET memory_limit = '4GB'; SET preserve_insertion_order = false;
SET temp_directory = '/private/tmp/claude-501/agent_data_schema/tmp';

COPY (SELECT file, line, source,
             (string_split(file, '/'))[4:5] IN (['.claude', 'projects'], ['.codex', 'sessions'], ['.codex', 'archived_sessions']) AS transcript,
             CASE source WHEN 'claude' THEN concat_ws('/', type, record ->> 'subtype', record -> 'attachment' ->> 'type')
                  ELSE concat_ws('/', type, payload_type, record -> 'payload' -> 'item' ->> 'type') END AS rtype,
             json_structure(record) AS st
      FROM read_parquet('/private/tmp/claude-501/agent_data_selfdispatch/lines/*/*.parquet', hive_partitioning := true)
      WHERE scope <> 'bounded')
TO 'structures.parquet';

COPY (SELECT source, transcript, rtype, replace(t.fullkey, '[0]', '[*]') AS path, t.key, replace(t.path, '[0]', '[*]') AS parent,
             CASE WHEN t.type IN ('OBJECT', 'ARRAY') THEN t.type ELSE t.value ->> '$' END AS jtype, count(t.id) AS n
      FROM 'structures.parquet' s, json_tree(s.st) t GROUP BY ALL)
TO 'inv_raw.parquet';

COPY (
WITH inv AS (FROM 'inv_raw.parquet' WHERE transcript),
maps AS (   -- map-shaped: many distinct keys, each seen on few lines (the keys are data)
    SELECT source, parent AS map_path FROM inv GROUP BY ALL
    HAVING count(DISTINCT key) > 40 AND sum(n) / count(DISTINCT key) < 300),
outer_maps AS (SELECT m.* FROM maps m ANTI JOIN maps o ON o.source = m.source AND starts_with(m.map_path, o.map_path || '.')),
homes(source, prefix, home) AS (VALUES
    ('claude', '$.type', 'lines.type, lines.record_type'), ('claude', '$.subtype', 'lines.record_type'),
    ('claude', '$.attachment.type', 'lines.record_type, attachments.kind'),
    ('claude', '$.sessionId', 'lines.session_id'), ('claude', '$.session_id', 'lines.session_id'),
    ('claude', '$.agentId', 'lines.agent_id'), ('claude', '$.timestamp', 'lines.ts'), ('claude', '$.uuid', 'lines.uuid'),
    ('claude', '$.parentUuid', 'lines.parent_uuid'), ('claude', '$.promptId', 'lines.turn_id'),
    ('claude', '$.requestId', 'usage.request_id'), ('claude', '$.message.id', 'usage.response_id, tool_calls.response_id'),
    ('claude', '$.message.model', 'usage.model'), ('claude', '$.message.stop_reason', 'usage.stop_reason'),
    ('claude', '$.message.role', 'messages.role'), ('claude', '$.message.content', 'blocks'),
    ('claude', '$.message.content[*].type', 'blocks.block.type'),
    ('claude', '$.message.usage.input_tokens', 'usage.total_input_tokens'),
    ('claude', '$.message.usage.cache_read_input_tokens', 'usage.cache_read_tokens'),
    ('claude', '$.message.usage.cache_creation_input_tokens', 'usage.cache_write_tokens'),
    ('claude', '$.message.usage.output_tokens', 'usage.output_tokens'),
    ('claude', '$.message.usage.output_tokens_details.thinking_tokens', 'usage.reasoning_tokens'),
    ('claude', '$.message.content[*].id', 'tool_calls.call_id'), ('claude', '$.message.content[*].name', 'tool_calls.tool_name'),
    ('claude', '$.message.content[*].input', 'tool_calls.input'), ('claude', '$.message.content[*].tool_use_id', 'tool_calls.call_id'),
    ('claude', '$.message.content[*].content', 'tool_calls.result'), ('claude', '$.message.content[*].is_error', 'tool_calls.is_error'),
    ('claude', '$.message.content[*].text', 'messages.text'), ('claude', '$.message.content[*].thinking', 'thinking.text'),
    ('claude', '$.message.content[*].signature', 'thinking.opaque_bytes'),
    ('claude', '$.attachment', 'attachments.attachment'), ('claude', '$.attachment.hookEvent', 'attachments.hook_event'),
    ('claude', '$.attachment.toolUseID', 'attachments.tool_use_id'), ('claude', '$.rendered', 'attachments.rendered, attachments.reminders'),
    ('claude', '$.cwd', 'sessions.cwd'), ('claude', '$.gitBranch', 'sessions.git_branch'), ('claude', '$.version', 'sessions.version'),
    ('claude', '$.aiTitle', 'sessions.title'), ('claude', '$.customTitle', 'sessions.title'),
    ('codex', '$.type', 'lines.type, lines.record_type'), ('codex', '$.payload.type', 'lines.payload_type, lines.record_type'),
    ('codex', '$.payload.item.type', 'lines.record_type'), ('codex', '$.timestamp', 'lines.ts'),
    ('codex', '$.payload.turn_id', 'lines.turn_id'), ('codex', '$.payload.internal_chat_message_metadata_passthrough.turn_id', 'lines.turn_id'),
    ('codex', '$.payload.call_id', 'tool_calls.call_id'), ('codex', '$.payload.name', 'tool_calls.tool_name'),
    ('codex', '$.payload.namespace', 'tool_calls.namespace'), ('codex', '$.payload.arguments', 'tool_calls.input'),
    ('codex', '$.payload.input', 'tool_calls.input'), ('codex', '$.payload.output', 'tool_calls.result'),
    ('codex', '$.payload.item', 'tool_calls.input, result, result_detail (tool items)'),
    ('codex', '$.payload.started_at_ms', 'tool_calls.duration_ms'), ('codex', '$.payload.completed_at_ms', 'tool_calls.duration_ms'),
    ('codex', '$.payload.response_id', 'usage.response_id'), ('codex', '$.payload.usage', 'usage.*_tokens'),
    ('codex', '$.payload.info.last_token_usage', 'usage.*_tokens (fallback)'), ('codex', '$.payload.model', 'usage.model'),
    ('codex', '$.payload.role', 'messages.role'), ('codex', '$.payload.content', 'messages.text'),
    ('codex', '$.payload.summary', 'thinking.text'), ('codex', '$.payload.encrypted_content', 'thinking.opaque_bytes'),
    ('codex', '$.payload.cwd', 'sessions.cwd'), ('codex', '$.payload.cli_version', 'sessions.version'),
    ('codex', '$.payload.parent_thread_id', 'sessions.parent_session_id'), ('codex', '$.payload.forked_from_id', 'sessions.forked_from_id'),
    ('codex', '$.payload.git.branch', 'sessions.git_branch')),
collapsed AS (   -- a path under a map is reported as <map>.{*}
    SELECT i.source, i.rtype, coalesce(m.map_path || '.{*}', i.path) AS path, m.map_path, i.jtype, i.n
    FROM inv i LEFT JOIN outer_maps m ON m.source = i.source AND starts_with(i.path, m.map_path || '.')),
grouped AS (
    SELECT source, rtype AS record_type, path, any_value(map_path) AS map_path, list(DISTINCT jtype ORDER BY jtype) AS json_types, sum(n) AS n
    FROM collapsed GROUP BY ALL),
homed AS (   -- longest home prefix that covers the path ([*] treated as a path step)
    SELECT g.*, h.home, h.prefix,
           row_number() OVER (PARTITION BY g.source, g.record_type, g.path ORDER BY strlen(h.prefix) DESC) AS r
    FROM grouped g LEFT JOIN homes h ON h.source = g.source
      AND starts_with(replace(g.path, '[*]', '.[*]') || '.', replace(h.prefix, '[*]', '.[*]') || '.'))
SELECT source, record_type, path, json_types, n, map_path IS NOT NULL AS map_shaped,
       coalesce(home, 'lines.record') AS home
FROM homed WHERE r = 1 ORDER BY source, record_type, path)
TO '/Users/aloksubbarao/worktrees/agent_data-claude-fields/design-schema/inventory.csv';
