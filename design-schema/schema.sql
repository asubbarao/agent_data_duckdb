-- Claude Code + Codex transcripts: one small schema for both sources (DESIGN, for Alok's review).
-- Nothing here is created on dev. Proven only in a disposable local duckdb over the self-dispatch
-- reader's Parquet (design-selfdispatch/). Unit of ingest = the file; key = (file, line).
-- Whole-file replace: a modified file's rows are deleted and re-read; every view below derives from
-- lines_input by (file, line), so no view holds state that outlives a file replace.
LOAD webbed;

-- The input. Today: the reader's Parquet. Later: the stored closed files UNION ALL BY NAME a live
-- read of files modified since the last ingest (same columns).
CREATE OR REPLACE VIEW lines_input AS
FROM read_parquet('/private/tmp/claude-501/agent_data_selfdispatch/lines/*/*.parquet', hive_partitioning := true)
WHERE scope <> 'bounded';   -- 'bounded' was a test subset of the same files

-- lines: one row per transcript line; record keeps the whole line. Only transcript folders
-- (~/.claude/projects, ~/.codex/sessions, ~/.codex/archived_sessions); history.jsonl, plugins, jobs are out.
CREATE OR REPLACE VIEW lines AS
WITH h AS (
    SELECT *, json_transform(record, '{"sessionId":"VARCHAR","session_id":"VARCHAR","agentId":"VARCHAR",
             "promptId":"VARCHAR","subtype":"VARCHAR","attachment":{"type":"VARCHAR"},
             "payload":{"turn_id":"VARCHAR","item":{"type":"VARCHAR"},
                        "internal_chat_message_metadata_passthrough":{"turn_id":"VARCHAR"}}}') AS h
    FROM lines_input
    WHERE (string_split(file, '/'))[4:5] IN (['.claude', 'projects'], ['.codex', 'sessions'], ['.codex', 'archived_sessions']))
SELECT source, file, line, coalesce(h.sessionId, h.session_id, session_id) AS session_id,
       h.agentId AS agent_id, type, payload_type,
       concat_ws('/', type, payload_type, h.subtype, h.attachment.type, h.payload.item.type) AS record_type,
       TRY_CAST(timestamp AS TIMESTAMPTZ) AS ts, uuid, parent_uuid,
       coalesce(h.promptId, h.payload.turn_id, h.payload.internal_chat_message_metadata_passthrough.turn_id) AS turn_id,
       record
FROM h;

-- turn_marks: every line that names its turn. Claude: user lines' promptId; Codex: payload turn ids.
-- Other views take the turn of the nearest mark at or before their line (ASOF).
CREATE OR REPLACE VIEW turn_marks AS
SELECT file, line, turn_id FROM lines WHERE turn_id IS NOT NULL;

-- blocks (Claude): message.content as one row per content block; a string content is one text block.
CREATE OR REPLACE VIEW blocks AS
WITH m AS (
    SELECT file, line, session_id, agent_id, ts, uuid,
           json_transform(record, '{"requestId":"VARCHAR","toolUseResult":"JSON",
             "message":{"id":"VARCHAR","role":"VARCHAR","model":"VARCHAR","content":"JSON"}}') AS m
    FROM lines WHERE source = 'claude' AND type IN ('user', 'assistant'))
SELECT file, line, session_id, agent_id, ts, uuid, m.message.role AS role, m.message.id AS response_id,
       m.requestId AS request_id, m.message.model AS model, m.toolUseResult AS tool_use_result, b.idx, b.block
FROM m CROSS JOIN UNNEST(from_json(
         CASE json_type(m.message.content) WHEN 'ARRAY' THEN m.message.content
              ELSE json_array(json_object('type', 'text', 'text', m.message.content ->> '$')) END,
         '[{"type":"VARCHAR","id":"VARCHAR","name":"VARCHAR","input":"JSON","tool_use_id":"VARCHAR",
            "content":"JSON","is_error":"BOOLEAN","text":"VARCHAR","thinking":"VARCHAR","signature":"VARCHAR"}]'))
     WITH ORDINALITY AS b(block, idx);

-- codex_items (Codex): the payload fields every Codex view needs, parsed once.
CREATE OR REPLACE VIEW codex_items AS
SELECT file, line, session_id, ts, type, payload_type, turn_id,
       json_transform(record, '{"payload":{"call_id":"VARCHAR","id":"VARCHAR","name":"VARCHAR","namespace":"VARCHAR",
         "arguments":"VARCHAR","input":"VARCHAR","output":"JSON","response_id":"VARCHAR","model":"VARCHAR",
         "role":"VARCHAR","content":"JSON","summary":"JSON","encrypted_content":"VARCHAR",
         "started_at_ms":"BIGINT","completed_at_ms":"BIGINT",
         "usage":{"input_tokens":"BIGINT","cached_input_tokens":"BIGINT","cache_write_input_tokens":"BIGINT",
                  "output_tokens":"BIGINT","reasoning_output_tokens":"BIGINT"},
         "info":{"last_token_usage":{"input_tokens":"BIGINT","cached_input_tokens":"BIGINT","cache_write_input_tokens":"BIGINT",
                  "output_tokens":"BIGINT","reasoning_output_tokens":"BIGINT"}},
         "item":{"id":"VARCHAR","type":"VARCHAR","tool":"VARCHAR","server":"VARCHAR","status":"VARCHAR",
                 "exit_code":"BIGINT","command":"JSON","arguments":"JSON","changes":"JSON","prompt":"VARCHAR",
                 "result":"JSON","error":"JSON","aggregated_output":"VARCHAR","stdout":"VARCHAR"}}}').payload AS p
FROM lines
WHERE source = 'codex' AND type IN ('response_item', 'event_msg', 'token_usage_record', 'turn_context');

-- usage: one row per API response. Claude: last line of each message.id within the file (Claude repeats
-- usage on every content-block line). Codex: token_usage_record (names its response_id); files written
-- before that record existed fall back to event_msg/token_count last_token_usage (never the cumulative total).
-- total_input_tokens is comparable across sources: Claude input + cache read + cache write; Codex input
-- (OpenAI input_tokens already includes cached_input_tokens). Copies of the same response in
-- resumed/forked session files are marked by the `copies` view (sum WHERE first_seen).
CREATE OR REPLACE VIEW usage AS
WITH claude AS (
    SELECT file, line, session_id, agent_id, ts, u.message.id AS response_id, u.requestId AS request_id,
           u.message.model AS model, u.message.stop_reason AS stop_reason, u.message.usage AS t
    FROM (SELECT *, json_transform(record, '{"requestId":"VARCHAR","message":{"id":"VARCHAR","model":"VARCHAR","stop_reason":"VARCHAR",
            "usage":{"input_tokens":"BIGINT","cache_read_input_tokens":"BIGINT","cache_creation_input_tokens":"BIGINT",
                     "output_tokens":"BIGINT","output_tokens_details":{"thinking_tokens":"BIGINT"}}}}') AS u
          FROM lines WHERE source = 'claude' AND type = 'assistant')
    QUALIFY row_number() OVER (PARTITION BY file, response_id ORDER BY line DESC) = 1),
codex AS (
    SELECT file, line, session_id, ts, p.response_id, coalesce(p.usage, p.info.last_token_usage) AS t
    FROM codex_items WHERE type = 'token_usage_record'
    UNION ALL
    SELECT file, line, session_id, ts, NULL, p.info.last_token_usage
    FROM codex_items
    WHERE payload_type = 'token_count' AND p.info.last_token_usage IS NOT NULL
      AND file NOT IN (SELECT file FROM codex_items WHERE type = 'token_usage_record')),
codex_model AS (SELECT file, line, p.model AS model FROM codex_items WHERE type = 'turn_context'),
u AS (
    SELECT 'claude' AS source, file, line, session_id, agent_id, ts, response_id, request_id, model, stop_reason,
           t.input_tokens + t.cache_read_input_tokens + t.cache_creation_input_tokens AS total_input_tokens,
           t.cache_read_input_tokens AS cache_read_tokens, t.cache_creation_input_tokens AS cache_write_tokens,
           t.output_tokens, t.output_tokens_details.thinking_tokens AS reasoning_tokens
    FROM claude
    UNION ALL
    SELECT 'codex', c.file, c.line, session_id, NULL, ts, response_id, NULL, m.model, NULL,
           t.input_tokens, t.cached_input_tokens, t.cache_write_input_tokens, t.output_tokens, t.reasoning_output_tokens
    FROM codex c ASOF LEFT JOIN codex_model m ON c.file = m.file AND c.line >= m.line)
SELECT u.*, k.turn_id
FROM u ASOF LEFT JOIN turn_marks k ON u.file = k.file AND u.line >= k.line;

-- tool_calls: one row per tool call, paired with its result (same file, same call id).
-- level 'model' = a call the model issued (Claude tool_use; Codex function_call / custom_tool_call).
-- level 'nested' = Codex work done inside a model call (event_msg/item_completed CommandExecution,
-- McpToolCall, FileChange, ... whose id is not itself a call id); parent_call_id is the model call before it.
-- response_id = the API response that issued the call (Claude message.id; Codex the token_usage_record
-- that follows the call), so usage.* tokens attribute to calls.
CREATE OR REPLACE VIEW tool_calls AS
WITH c_call AS (
    SELECT 'claude' AS source, file, line, session_id, agent_id, ts, block.id AS call_id, block.name AS tool_name,
           NULL AS namespace, block.input AS input, response_id
    FROM blocks WHERE block.type = 'tool_use'),
c_result AS (
    SELECT file, line AS result_line, ts AS result_ts, block.tool_use_id AS call_id, block.content AS result,
           block.is_error AS is_error   -- toolUseResult (structured copy of the result) stays in lines.record at result_line
    FROM blocks WHERE block.type = 'tool_result'),
x_resp AS (SELECT file, line, p.response_id FROM codex_items WHERE type = 'token_usage_record'),
x_call AS (
    SELECT 'codex' AS source, c.file, c.line, session_id, NULL AS agent_id, ts, p.call_id, p.name AS tool_name,
           p.namespace, coalesce(TRY_CAST(p.arguments AS JSON), to_json(p.input)) AS input, r.response_id
    FROM codex_items c ASOF LEFT JOIN x_resp r ON c.file = r.file AND c.line < r.line
    WHERE payload_type IN ('function_call', 'custom_tool_call', 'tool_search_call')),
x_item AS (
    SELECT file, line, session_id, ts, p.item AS i, p.completed_at_ms - p.started_at_ms AS duration_ms
    FROM codex_items
    WHERE payload_type = 'item_completed'
      AND p.item.type IN ('CommandExecution', 'McpToolCall', 'FileChange', 'DynamicToolCall', 'CollabAgentToolCall', 'Extension', 'ImageView')),
x_result AS (
    SELECT o.file, o.line AS result_line, o.ts AS result_ts, o.p.call_id, o.p.output AS result,
           i.i.status NOT IN ('completed') AS is_error, to_json(i.i) AS result_detail
    FROM codex_items o ASOF LEFT JOIN x_item i ON o.file = i.file AND o.p.call_id = i.i.id AND o.line > i.line
    WHERE o.payload_type IN ('function_call_output', 'custom_tool_call_output', 'tool_search_output')),
x_nested AS (
    SELECT 'codex' AS source, n.file, n.line, n.session_id, NULL AS agent_id, n.ts, n.i.id AS call_id,
           coalesce(n.i.tool, n.i.type) AS tool_name, coalesce(n.i.server, n.i.type) AS namespace,
           coalesce(n.i.arguments, n.i.command, n.i.changes, to_json(n.i.prompt)) AS input, p.response_id,
           p.call_id AS parent_call_id, n.line AS result_line, n.ts AS result_ts,
           coalesce(n.i.result, n.i.error, to_json(n.i.aggregated_output), to_json(n.i.stdout)) AS result,
           CASE n.i.status WHEN 'completed' THEN coalesce(n.i.exit_code, 0) <> 0 ELSE true END AS is_error, to_json(n.i) AS result_detail, n.duration_ms
    FROM x_item n ASOF LEFT JOIN x_call p ON n.file = p.file AND n.line > p.line
    WHERE (n.file, n.i.id) NOT IN (SELECT (file, call_id) FROM x_call)),
calls AS (
    SELECT 'model' AS level, c.*, NULL AS parent_call_id, r.* EXCLUDE (file, call_id),
           date_diff('millisecond', c.ts, r.result_ts) AS duration_ms
    FROM c_call c ASOF LEFT JOIN c_result r ON c.file = r.file AND c.call_id = r.call_id AND c.line < r.result_line
    UNION ALL BY NAME   -- ASOF: a file can hold the same call twice (copied history); each pairs with the next result
    SELECT 'model' AS level, c.*, NULL AS parent_call_id, r.* EXCLUDE (file, call_id),
           date_diff('millisecond', c.ts, r.result_ts) AS duration_ms
    FROM x_call c ASOF LEFT JOIN x_result r ON c.file = r.file AND c.call_id = r.call_id AND c.line < r.result_line
    UNION ALL BY NAME
    SELECT 'nested' AS level, * FROM x_nested),
keyed AS (   -- the turn, found on narrow keys so input/result are not carried through the ASOF
    SELECT c.file, c.line, c.call_id, k.turn_id
    FROM (SELECT file, line, call_id FROM c_call UNION ALL SELECT file, line, call_id FROM x_call
          UNION ALL SELECT file, line, call_id FROM x_nested) c
    ASOF LEFT JOIN turn_marks k ON c.file = k.file AND c.line >= k.line)
SELECT c.source, c.level, c.session_id, c.agent_id, keyed.turn_id, c.file, c.line, c.ts, c.response_id,
       c.call_id, c.parent_call_id, c.tool_name, c.namespace, c.input, strlen(c.input::VARCHAR) AS input_bytes,
       c.result_line, c.result_ts, c.result, strlen(c.result::VARCHAR) AS result_bytes, c.is_error,
       c.duration_ms, c.result_detail
FROM calls c JOIN keyed USING (file, line, call_id);

-- copies: the one cross-file view. Resumed/forked sessions copy earlier lines into a new file, so the
-- same tool call or API response appears in several files. first_seen marks the earliest copy; sum
-- tokens and bytes WHERE first_seen. Every other view depends on one file only (whole-file replace).
CREATE OR REPLACE VIEW copies AS
SELECT 'tool_call' AS kind, source, call_id AS id, file, line,
       row_number() OVER (PARTITION BY source, call_id ORDER BY ts, file, line) = 1 AS first_seen
FROM tool_calls
UNION ALL
SELECT 'response', source, response_id, file, line,
       row_number() OVER (PARTITION BY source, response_id ORDER BY ts, file, line) = 1
FROM usage WHERE response_id IS NOT NULL;

-- messages (thin): visible text. Claude text blocks; Codex response_item message / agent_message content.
CREATE OR REPLACE VIEW messages AS
SELECT 'claude' AS source, file, line, idx, session_id, agent_id, ts, role, block.text AS text
FROM blocks WHERE block.type = 'text'
UNION ALL
SELECT 'codex', file, line, b.idx, session_id, NULL, ts, coalesce(p.role, 'agent'), b.part.text
FROM codex_items CROSS JOIN UNNEST(from_json(p.content, '[{"type":"VARCHAR","text":"VARCHAR"}]')) WITH ORDINALITY AS b(part, idx)
WHERE payload_type IN ('message', 'agent_message');

-- thinking (thin): Claude thinking blocks (text is empty when redacted); Codex reasoning summary parts
-- (one row per part; an item with no summary keeps one row with NULL text). Encrypted bodies stay in record.
CREATE OR REPLACE VIEW thinking AS
SELECT 'claude' AS source, file, line, idx, session_id, agent_id, ts, response_id, block.thinking AS text,
       strlen(block.signature) AS opaque_bytes
FROM blocks WHERE block.type IN ('thinking', 'redacted_thinking')
UNION ALL
SELECT 'codex', file, line, s.idx, session_id, NULL, ts, NULL, s.part.text, strlen(p.encrypted_content)
FROM codex_items CROSS JOIN UNNEST(coalesce(nullif(from_json(p.summary, '[{"text":"VARCHAR"}]'), []), [NULL])) WITH ORDINALITY AS s(part, idx)
WHERE payload_type = 'reasoning';

-- attachments (thin, Claude only: Codex has no attachment records): what the harness injected.
-- reminders = the <system-reminder> bodies of the rendered text.
CREATE OR REPLACE VIEW attachments AS
WITH a AS (
    SELECT file, line, session_id, agent_id, ts, record_type,
           json_transform(record, '{"attachment":{"type":"VARCHAR","hookEvent":"VARCHAR","toolUseID":"VARCHAR"},
             "rendered":[{"content":"VARCHAR"}]}') AS a, record -> 'attachment' AS attachment
    FROM lines WHERE source = 'claude' AND type = 'attachment')
SELECT file, line, session_id, agent_id, ts, a.attachment.type AS kind, a.attachment.hookEvent AS hook_event,
       a.attachment.toolUseID AS tool_use_id, attachment,
       list_transform(a.rendered, lambda r: r.content) AS rendered,
       flatten(list_transform(a.rendered, lambda r: html_extract_text(parse_html(r.content), '//system-reminder'))) AS reminders
FROM a;

-- system_events (thin): every line no other view reads, whole record kept.
-- Claude: system/*, queue-operation, and session-state records (mode, pr-link, ai-title, last-prompt, ...).
-- Codex: session_meta, turn_context, task_started/complete, turn_aborted, compacted, world_state, ...
CREATE OR REPLACE VIEW system_events AS
SELECT source, file, line, session_id, ts, turn_id, record_type, record   -- no agent_id: 0 % filled on these records
FROM lines
WHERE type NOT IN ('user', 'assistant', 'attachment', 'token_usage_record')
  AND coalesce(payload_type, '') NOT IN ('message', 'agent_message', 'reasoning', 'function_call', 'custom_tool_call',
        'tool_search_call', 'function_call_output', 'custom_tool_call_output', 'tool_search_output', 'token_count', 'item_completed');

-- sessions (thin): one row per file (a Claude subagent file or a Codex thread is its own row).
CREATE OR REPLACE VIEW sessions AS
WITH s AS (
    SELECT source, file, line, session_id, agent_id, ts, type,
           CASE WHEN type IN ('user', 'session_meta', 'ai-title', 'custom-title')
                THEN json_transform(record, '{"cwd":"VARCHAR","gitBranch":"VARCHAR","version":"VARCHAR",
                       "aiTitle":"VARCHAR","customTitle":"VARCHAR",
                       "payload":{"cwd":"VARCHAR","cli_version":"VARCHAR","parent_thread_id":"VARCHAR",
                                  "forked_from_id":"VARCHAR","git":{"branch":"VARCHAR"}}}') END AS j
    FROM lines)
SELECT source, file, first(session_id ORDER BY line) AS session_id, first(agent_id ORDER BY line) AS agent_id,
       list_contains(string_split(file, '/'), 'subagents') AS is_subagent,
       first(j.payload.parent_thread_id ORDER BY line) FILTER (WHERE type = 'session_meta') AS parent_session_id,
       first(j.payload.forked_from_id ORDER BY line) FILTER (WHERE type = 'session_meta') AS forked_from_id,
       first(coalesce(j.cwd, j.payload.cwd) ORDER BY line) FILTER (WHERE type IN ('user', 'session_meta')) AS cwd,
       first(coalesce(j.gitBranch, j.payload.git.branch) ORDER BY line) FILTER (WHERE type IN ('user', 'session_meta')) AS git_branch,
       first(coalesce(j.version, j.payload.cli_version) ORDER BY line) FILTER (WHERE type IN ('user', 'session_meta')) AS version,
       last(coalesce(j.customTitle, j.aiTitle) ORDER BY line) FILTER (WHERE type IN ('ai-title', 'custom-title')) AS title,
       min(ts) AS started_at, max(ts) AS ended_at, count(line) AS lines
FROM s GROUP BY source, file;
