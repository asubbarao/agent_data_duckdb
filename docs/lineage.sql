-- Cross-system lineage for shell-launched children.
--
-- In-process children carry their parent natively (Claude subagents/, Codex
-- thread_spawn) and read_conversations() exposes it as parent_session_id.
-- A `codex exec` or `claude -p` run started from another agent's shell tool is
-- a top-level session in its own home with no parent pointer, in either
-- direction across the two systems. The caller's transcript still holds the
-- launch: a tool call whose input carries the brief (heredoc or a brief.md
-- written just before) and the child's working directory. This query joins
-- the two. session_id is unique across both homes, so the parent may live in
-- the other system.
--
-- Evidence, strongest first:
--   brief_text   the child's first caller-written message appears verbatim in
--                the caller's tool input (JSON-escaped, as the transcript holds it)
--   cwd_and_time the caller's launch command names the child's cwd and ran
--                within the window before the child started
--
-- LOAD agent_data;

WITH rows AS (
    SELECT * FROM read_conversations(path = '~/.claude', source = 'claude')
    UNION ALL BY NAME
    SELECT * FROM read_conversations(path = '~/.codex', source = 'codex')
),
launch_patterns AS (
    -- Text that marks a tool call as a possible launch: the command itself, or
    -- the brief file the command then feeds on stdin. Add your own launcher here.
    SELECT 'codex exec' AS pattern
    UNION ALL SELECT 'claude -p'
    UNION ALL SELECT 'brief.md'
),
children AS (
    -- The first caller-written message of every session with no native parent.
    SELECT source AS child_source, session_id AS child_session, cwd AS child_cwd,
           try_cast(timestamp AS TIMESTAMPTZ) AS child_start, message_content AS brief,
           substr(to_json(message_content)::VARCHAR, 2, length(to_json(message_content)::VARCHAR) - 2) AS brief_escaped
    FROM rows
    WHERE author = 'caller' AND NOT is_agent AND message_role = 'user'
    QUALIFY row_number() OVER (PARTITION BY source, session_id ORDER BY line_number) = 1
),
launches AS (
    SELECT DISTINCT source AS caller_source, session_id AS caller_session, cwd AS caller_cwd,
           try_cast(timestamp AS TIMESTAMPTZ) AS call_ts, tool_name, tool_input, uuid AS call_uuid,
           pattern = 'brief.md' AS is_brief_file
    FROM rows
    JOIN launch_patterns ON contains(rows.tool_input, launch_patterns.pattern)
    WHERE author = 'agent'
),
by_text AS (
    SELECT c.child_source, c.child_session, l.caller_source, l.caller_session, l.call_uuid, l.call_ts,
           'brief_text' AS evidence
    FROM children c
    JOIN launches l
      ON l.call_ts BETWEEN c.child_start - INTERVAL 6 HOUR AND c.child_start + INTERVAL 1 MINUTE
     AND length(c.brief) >= 40
     AND contains(l.tool_input, c.brief_escaped)
),
by_cwd AS (
    SELECT c.child_source, c.child_session, l.caller_source, l.caller_session, l.call_uuid, l.call_ts,
           'cwd_and_time' AS evidence
    FROM children c
    JOIN launches l
      ON l.call_ts BETWEEN c.child_start - INTERVAL 30 MINUTE AND c.child_start + INTERVAL 1 MINUTE
     AND NOT l.is_brief_file
     AND contains(l.tool_input, c.child_cwd)
    WHERE c.child_session NOT IN (SELECT child_session FROM by_text)
),
linked AS (
    SELECT * FROM by_text
    UNION ALL
    SELECT * FROM by_cwd
)
-- One caller per child: the latest qualifying call before the child started.
SELECT child_source, child_session, caller_source, caller_session, call_uuid, call_ts, evidence
FROM linked
QUALIFY row_number() OVER (PARTITION BY child_source, child_session ORDER BY call_ts DESC) = 1
ORDER BY call_ts;
