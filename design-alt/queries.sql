-- queries.sql: the five ergonomics questions against the RawDuck tables (run from the store's folder).
LOAD rawduck;
LOAD webbed;
ATTACH 'rawduck:raw.db' AS raw (READ_ONLY);

-- 1. tokens per model per day (Claude: one row per API response, last line wins; Codex: token_count after each turn's model)
WITH claude AS (
  SELECT "json.message.id", arg_max("json.message.model", "json.timestamp") AS model, max("json.timestamp")::DATE AS day,
         arg_max("json.message.usage.input_tokens", "json.timestamp") AS input, arg_max("json.message.usage.output_tokens", "json.timestamp") AS output
  FROM raw.claude_lines WHERE "json.type" = 'assistant' GROUP BY "json.message.id"
), codex AS (
  SELECT t."json.payload.model" AS model, u."json.timestamp"::DATE AS day,
         u."json.payload.info.last_token_usage.input_tokens" AS input, u."json.payload.info.last_token_usage.output_tokens" AS output
  FROM raw.codex_lines u ASOF JOIN raw.codex_lines t
    ON t.filename = u.filename AND t."json.type" = 'turn_context' AND u."json.ordinal" >= t."json.ordinal"
  WHERE u."json.payload.type" = 'token_count'
)
SELECT model, day, sum(input) AS input_tokens, sum(output) AS output_tokens
FROM (FROM claude SELECT model, day, input, output UNION ALL FROM codex) GROUP BY model, day ORDER BY day, model;

-- 2. Claude tool calls by name with error rate (content blocks unnested at query time)
WITH blocks AS (
  SELECT unnest(json_extract("json.message.content", '$[*]')) AS b FROM raw.claude_lines
  WHERE json_type("json.message.content") = 'ARRAY'
)
SELECT (u.b->>'$.name') AS tool, count(u.b) AS calls, round(avg(coalesce((r.b->>'$.is_error') = 'true', false)::INT), 3) AS error_rate
FROM blocks u LEFT JOIN blocks r ON (r.b->>'$.tool_use_id') = (u.b->>'$.id') AND (r.b->>'$.type') = 'tool_result'
WHERE (u.b->>'$.type') = 'tool_use' GROUP BY tool ORDER BY calls DESC;

-- 3. subagent sessions with their parent (Claude: path + meta.json; Codex: session_meta)
SELECT 'claude' AS source, string_split(l.filename, '/')[-3] AS parent_session, parse_filename(l.filename, true) AS subagent,
       any_value(m."json.agentType") AS agent_type, count(l.filename) AS lines
FROM raw.claude_lines l LEFT JOIN raw.claude_meta m ON m.filename = replace(l.filename, '.jsonl', '.meta.json')
WHERE list_contains(string_split(l.filename, '/'), 'subagents') GROUP BY ALL
UNION ALL
SELECT 'codex', "json.payload.source.subagent.thread_spawn.parent_thread_id", "json.payload.id", "json.payload.agent_path", NULL
FROM raw.codex_lines WHERE "json.type" = 'session_meta' AND "json.payload.source.subagent.thread_spawn.parent_thread_id" IS NOT NULL;

-- 4. system reminders by kind (first words of each <system-reminder> in user text)
WITH t AS (
  SELECT unnest(json_extract_string("json.message.content", '$[*].text')) AS text FROM raw.claude_lines
  WHERE "json.type" = 'user' AND json_type("json.message.content") = 'ARRAY'
  UNION ALL SELECT "json.message.content"->>'$' FROM raw.claude_lines
  WHERE "json.type" = 'user' AND json_type("json.message.content") = 'VARCHAR'
), r AS (SELECT unnest(html_extract_text(parse_html(text), '//system-reminder')) AS reminder FROM t)
SELECT left(trim(reminder), 60) AS reminder_kind, count(reminder) AS n FROM r GROUP BY reminder_kind ORDER BY n DESC;

-- 5. turns per session (Claude promptId groups; Codex task_started)
SELECT 'claude' AS source, "json.sessionId" AS session, count(DISTINCT "json.promptId") AS turns FROM raw.claude_lines GROUP BY ALL
UNION ALL
SELECT 'codex', filename, count(filename) FILTER (WHERE "json.payload.type" = 'task_started') FROM raw.codex_lines GROUP BY ALL
ORDER BY turns DESC;
