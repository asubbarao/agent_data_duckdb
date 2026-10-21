-- Claude + Codex transcripts as SQL views: the replacement for agent_data's read_conversations
-- for these two sources. Written as the statements that WOULD go into dev's setup.sql.
-- NOTHING here has been created on dev; every body was run on dev as a plain SELECT/WITH or COPY.
--
-- §1 listing (one live find) -> readers, dirs, files: all discovery derived from one view
-- §2 structure derivation (one statement; its output is the columns literal in dirs)
-- §3 reads (PR #1576 dispatch per directory / file), lines_program (rendered typed read)
-- §4 lines, blocks
-- §5 the nine concept views, same names and columns for both sources

LOAD shellfs;     -- find through read_lines
LOAD hostfs;      -- path scalars
LOAD quackapi;    -- quackapi_post (1576 reads)
LOAD webbed;      -- attachments.reminders
LOAD read_lines;  -- read_lines('cmd |')

-- §1 ─────────────────────────────────────────────────────────────────────────────────────────
-- readers: PR #1576 verbatim. A file's reader is the catalog's read_<ext> table function.
CREATE OR REPLACE VIEW readers AS
SELECT DISTINCT '.' || substr(function_name, 6) AS ext, function_name AS catalog_reader
FROM duckdb_functions()
WHERE function_type = 'table' AND starts_with(function_name, 'read_');

-- listing: PR #1576's `entries`, column names kept. One view, re-listed on every query (files
-- are read live anyway). Changed from 1576, and why:
--  * source: one shellfs `find <roots> -type f` instead of git_tree_each/git_status_each (these
--    folders are not git repos). No folder names: transcripts are recognised by content (§4).
--  * git-only columns dropped (tracked, status, presence, size_delta, type_mismatch, is_symlink...).
--  * reader: a CASE by extension first (.jsonl/.jsonl.zst have no catalog read_<ext>; read_json
--    decompresses .zst natively, read_text does not), then the catalog reader, then read_blob.
--  * session/agent ids from the path parts (subagents/<agent>.jsonl under <session>/).
--  * sql_statement built with printf and dollar quotes instead of 1576's backtick translate.
CREATE OR REPLACE VIEW listing AS
WITH found AS (
    SELECT rtrim(content, e'\n') AS path
    FROM read_lines($c$find /Users/aloksubbarao/.claude /Users/aloksubbarao/.codex -type f |$c$)
),
here AS (SELECT found.*, file_extension(path) AS ext FROM found)
SELECT here.*,
       CASE WHEN starts_with(path, '/Users/aloksubbarao/.codex/') THEN 'codex' ELSE 'claude' END AS root,
       parse_dirpath(path)                    AS parent,
       file_name(parent)                      AS parent_name,
       file_name(path)                        AS name,
       parse_filename(path, true)             AS stem,
       path_split(path)                       AS parts,
       len(parts)                             AS depth,
       file_size(path)                        AS bytes,
       file_last_modified(path)               AS modified,
       ends_with(replace(name, '.jsonl.zst', '.jsonl'), '.jsonl') AS is_jsonl,
       parent_name = 'subagents'              AS is_subagent,
       -- ids from the path: <session>.jsonl, <session>/subagents/agent-<id>.jsonl,
       -- <session>/tool-results/<file>, <session>/subagents/agent-<id>.meta.json
       CASE WHEN is_subagent THEN replace(replace(replace(name, '.meta.json', ''), '.jsonl', ''), 'agent-', '') END AS agent_id,
       CASE WHEN parent_name IN ('tool-results', 'subagents') THEN file_name(parse_dirpath(parent))
            ELSE replace(stem, '.jsonl', '') END AS session_id,
       CASE WHEN is_jsonl               THEN 'read_ndjson'
            WHEN here.ext = '.json'     THEN 'read_json'
            WHEN here.ext IN ('.txt', '.md', '.log') THEN 'read_text'
            WHEN here.ext = '.pdf'      THEN 'read_pdf'
            ELSE coalesce(catalog_reader, 'read_blob') END AS reader,
       -- transcripts are read per directory (dirs); every other file by its own reader
       CASE WHEN NOT is_jsonl THEN printf($s$SELECT * FROM %s('%s')$s$, reader, path) END AS sql_statement
FROM here LEFT JOIN readers ON readers.ext = here.ext;

-- dirs: the unit of reading is a directory. One statement per directory reads all of that
-- folder's .jsonl and .jsonl.zst files with the declared columns. The list is the folder's files
-- from listing rather than '<dir>/*.jsonl' + '<dir>/*.jsonl.zst' globs, because a glob that
-- matches nothing is an error and most folders have only one of the two.
-- columns: the top-level keys and types §2 derived on dev (185 sampled files, 264 MB, 51 s).
-- Nested objects stay JSON (semi-raw, parsed where used) except `message` (Claude) and `payload`
-- (Codex), which keep §2's STRUCT so the views use dot access; inside payload the large varied
-- objects (state, thread_settings, resume_metadata, retained_context, token-usage totals) are JSON.
-- A key absent from every sampled file is not read: re-run §2 when new record kinds appear
-- (the first sample, capped at 1.5 MB, missed prRepository, prUrl and continuedInSessionId).
CREATE OR REPLACE VIEW dirs AS
SELECT parent, root,
       list(path ORDER BY path)                AS files,
       sum(bytes)::BIGINT                      AS bytes,
       max(modified)                           AS modified,     -- incremental: re-read a dir when this moves
       printf($s$SELECT * FROM read_ndjson([%s], filename := true, columns := {
    'type': 'VARCHAR', 'subtype': 'VARCHAR', 'timestamp': 'VARCHAR', 'ts': 'VARCHAR', 'uuid': 'VARCHAR', 'parentUuid': 'VARCHAR',
    'logicalParentUuid': 'VARCHAR', 'leafUuid': 'VARCHAR', 'sessionId': 'VARCHAR', 'promptId': 'VARCHAR', 'requestId': 'VARCHAR',
    'messageId': 'VARCHAR', 'snapshotMessageId': 'VARCHAR', 'interruptedMessageId': 'VARCHAR', 'refusedUserMessageUuid': 'VARCHAR',
    'retractedMessageUuids': 'JSON', 'commandUuid': 'VARCHAR', 'deliveryId': 'VARCHAR', 'toolUseID': 'VARCHAR', 'sourceToolUseID': 'VARCHAR',
    'cwd': 'VARCHAR', 'relocatedCwd': 'VARCHAR', 'gitBranch': 'VARCHAR', 'version': 'VARCHAR', 'entrypoint': 'VARCHAR', 'slug': 'VARCHAR',
    'isMeta': 'BOOLEAN', 'isCompactSummary': 'BOOLEAN', 'isVisibleInTranscriptOnly': 'BOOLEAN', 'isSnapshotUpdate': 'BOOLEAN',
    'isApiErrorMessage': 'BOOLEAN', 'isAbortedMidStream': 'BOOLEAN', 'apiErrorStatus': 'UBIGINT', 'apiError': 'VARCHAR', 'error': 'JSON',
    'errorDetails': 'VARCHAR', 'apiRefusalCategory': 'VARCHAR', 'apiRefusalExplanation': 'VARCHAR', 'retryAttempt': 'UBIGINT',
    'retryInMs': 'UBIGINT', 'maxRetries': 'UBIGINT', 'cause': 'VARCHAR', 'originalModel': 'VARCHAR', 'fallbackModel': 'VARCHAR',
    'promptSource': 'VARCHAR', 'turnOrigin': 'VARCHAR', 'origin': 'JSON', 'effort': 'VARCHAR', 'perTurnEffort': 'VARCHAR',
    'message': 'STRUCT("content" JSON, "id" VARCHAR, "model" VARCHAR, "stop_reason" VARCHAR, "usage" STRUCT("cache_creation_input_tokens" UBIGINT, "cache_read_input_tokens" UBIGINT, "input_tokens" UBIGINT, "output_tokens" UBIGINT, "output_tokens_details" STRUCT("thinking_tokens" UBIGINT)))',
    'toolUseResult': 'JSON', 'attachment': 'JSON', 'rendered': 'JSON', 'imagePasteIds': 'JSON', 'userFeedback': 'VARCHAR',
    'content': 'VARCHAR', 'level': 'VARCHAR', 'stopReason': 'VARCHAR', 'preventedContinuation': 'BOOLEAN', 'hasOutput': 'BOOLEAN',
    'hookCount': 'UBIGINT', 'hookInfos': 'JSON', 'hookErrors': 'JSON', 'hookAdditionalContext': 'JSON', 'compactMetadata': 'JSON',
    'durationMs': 'UBIGINT', 'messageCount': 'UBIGINT', 'trigger': 'VARCHAR', 'reason': 'VARCHAR', 'scope': 'VARCHAR', 'direction': 'VARCHAR',
    'operation': 'VARCHAR', 'commandRun': 'JSON', 'classifierBoundary': 'BOOLEAN', 'classifierMetaLines': 'VARCHAR', 'explicit': 'BOOLEAN',
    'agentName': 'VARCHAR', 'agentSetting': 'VARCHAR', 'aiTitle': 'VARCHAR', 'title': 'VARCHAR', 'customTitle': 'VARCHAR', 'mode': 'VARCHAR',
    'permissionMode': 'VARCHAR', 'prNumber': 'UBIGINT', 'prRepository': 'VARCHAR', 'prUrl': 'VARCHAR', 'bridgeSessionId': 'VARCHAR',
    'lastPrompt': 'VARCHAR', 'continuedInSessionId': 'VARCHAR', 'sessionKind': 'VARCHAR', 'worktreeSession': 'JSON',
    'accountUuid': 'VARCHAR', 'vetoedAgainstAccountUuid': 'VARCHAR', 'frameUrl': 'VARCHAR', 'trackingPath': 'VARCHAR', 'path': 'VARCHAR',
    'artifacts': 'JSON', 'artifactCount': 'UBIGINT', 'attributionPlugin': 'VARCHAR', 'pendingBackgroundAgentCount': 'UBIGINT', 'v': 'UBIGINT',
    'startTime': 'UBIGINT', 'totalCostUSD': 'DOUBLE', 'totalDuration': 'UBIGINT', 'totalAPIDuration': 'UBIGINT',
    'totalAPIDurationWithoutRetries': 'UBIGINT', 'totalToolDuration': 'UBIGINT', 'totalLinesAdded': 'UBIGINT', 'totalLinesRemoved': 'UBIGINT',
    'hasUnknownModelCost': 'BOOLEAN', 'modelUsage': 'JSON', 'source': 'VARCHAR', 'ordinal': 'UBIGINT',
    'payload': 'STRUCT("type" VARCHAR, "id" VARCHAR, "session_id" VARCHAR, "thread_id" VARCHAR, "turn_id" VARCHAR, "root_turn_id" VARCHAR,
        "response_id" VARCHAR, "call_id" VARCHAR, "name" VARCHAR, "namespace" VARCHAR, "arguments" JSON, "input" VARCHAR, "output" JSON,
        "status" VARCHAR, "role" VARCHAR, "phase" VARCHAR, "author" VARCHAR, "recipient" VARCHAR, "content" JSON, "summary" JSON,
        "encrypted_content" VARCHAR, "text" VARCHAR, "message" VARCHAR, "last_agent_message" VARCHAR, "reason" VARCHAR, "outcome" VARCHAR,
        "error" JSON, "timestamp" VARCHAR, "started_at" UBIGINT, "completed_at" UBIGINT, "duration_ms" UBIGINT, "time_to_first_token_ms" UBIGINT,
        "cwd" VARCHAR, "model" VARCHAR, "effort" VARCHAR, "model_provider" VARCHAR, "model_context_window" UBIGINT, "originator" VARCHAR,
        "cli_version" VARCHAR, "source" JSON, "thread_source" VARCHAR, "parent_thread_id" VARCHAR, "forked_from_id" VARCHAR,
        "agent_nickname" VARCHAR, "agent_path" VARCHAR, "git" STRUCT("branch" VARCHAR, "commit_hash" VARCHAR, "repository_url" VARCHAR),
        "collaboration_mode" STRUCT("mode" VARCHAR, "settings" STRUCT("developer_instructions" VARCHAR, "model" VARCHAR, "reasoning_effort" VARCHAR)),
        "collaboration_mode_kind" VARCHAR, "realtime_session_id" VARCHAR, "compaction_response_id" VARCHAR, "window_id" VARCHAR,
        "window_number" UBIGINT, "first_window_id" VARCHAR, "previous_window_id" VARCHAR, "replacement_history" JSON,
        "replacement_history_metadata" JSON, "resume_metadata" JSON, "retained_context" JSON, "state" JSON, "thread_settings" JSON,
        "file_system_sandbox_policy" JSON, "turn_attribution" JSON, "internal_chat_message_metadata_passthrough" STRUCT("turn_id" VARCHAR),
        "info" STRUCT("last_token_usage" STRUCT("cache_write_input_tokens" UBIGINT, "cached_input_tokens" UBIGINT, "input_tokens" UBIGINT, "output_tokens" UBIGINT, "reasoning_output_tokens" UBIGINT, "total_tokens" UBIGINT)),
        "usage" STRUCT("cache_write_input_tokens" UBIGINT, "cached_input_tokens" UBIGINT, "input_tokens" UBIGINT, "output_tokens" UBIGINT, "reasoning_output_tokens" UBIGINT, "total_tokens" UBIGINT),
        "turn_token_usage" JSON, "thread_token_usage" JSON, "latest_token_usage_record" JSON,
        "item" STRUCT("type" VARCHAR, "id" VARCHAR, "server" VARCHAR, "tool" VARCHAR, "arguments" JSON, "result" JSON, "results" JSON,
            "error" JSON, "status" VARCHAR, "command" JSON, "cwd" VARCHAR, "process_id" VARCHAR, "source" VARCHAR, "aggregated_output" VARCHAR,
            "formatted_output" VARCHAR, "stdout" VARCHAR, "stderr" VARCHAR, "exit_code" HUGEINT, "duration" JSON, "durationMs" UBIGINT,
            "changes" JSON, "content" JSON, "raw_content" JSON, "summary_text" JSON, "phase" VARCHAR, "kind" VARCHAR, "query" VARCHAR,
            "action" JSON, "actionName" VARCHAR, "appName" VARCHAR, "connectorId" VARCHAR, "pluginId" VARCHAR, "linkId" VARCHAR,
            "client_id" VARCHAR, "agent_path" VARCHAR, "agent_thread_id" VARCHAR, "model" JSON, "reasoning_effort" JSON, "readOnlyHint" BOOLEAN))'
    }) WITH ORDINALITY$s$,
              string_agg(printf($q$'%s'$q$, path), ', ' ORDER BY path)) AS sql_statement
FROM listing WHERE is_jsonl
GROUP BY parent, root;

-- files: everything that is not a .jsonl transcript (tool-results .txt, plans .md, todos and
-- subagent meta .json, pasted images and PDFs, sqlite...), keyed by path, with the session and
-- agent ids from the path so a row joins back to its session or tool call. One view in place of
-- read_plans / read_todos / read_history / read_stats. Contents come through `reads` (§3).
CREATE OR REPLACE VIEW files AS
SELECT root, path, parent, name, ext, bytes, modified, session_id, agent_id, is_subagent, reader, sql_statement
FROM listing WHERE NOT is_jsonl;

-- §2 ─────────────────────────────────────────────────────────────────────────────────────────
-- Structure derivation: one statement on dev, re-run when new record kinds appear. The json_schema
-- community extension was checked first: it offers json_schema_validate / _patch / _update
-- (update merges documents; verified on dev) and no inference, so the inference is DuckDB's own
-- json_group_structure. Steps: the sample comes from the find listing; a self-dispatched read
-- (the reader needs a literal file list) returns the merged structure per source; ignore.csv
-- paths are dropped unless mapping.csv uses them; jsonata turns the structure into types
-- (objects -> STRUCT; arrays, NULL-only leaves, mapping.csv JSON paths and map-like objects
-- such as file-path or model-name keys -> JSON). json_tree was tried for the walk and ran out of
-- memory on the 435 KB Claude structure.
/*
LOAD jsonata;
WITH found AS (
    SELECT rtrim(content, e'\n') AS path
    FROM read_lines($c$find /Users/aloksubbarao/.claude/projects /Users/aloksubbarao/.codex/sessions -type f -name '*.jsonl' |$c$)
),
sample AS (
    SELECT path FROM found
    WHERE file_size(path) BETWEEN 20000 AND 4000000
      AND file_last_modified(path) BETWEEN TIMESTAMP '2026-09-20' AND TIMESTAMP '2026-10-14'
      AND (file_name(parse_dirpath(path)) <> 'subagents' OR ends_with(path, '0.jsonl'))
),
program AS (
    SELECT printf($s$
        WITH obj AS (SELECT CASE WHEN contains(filename, '/.codex/') THEN 'codex' ELSE 'claude' END AS source, json
                     FROM read_ndjson_objects([%s], filename := true)),
        arr AS (SELECT source, coalesce(json->'$.message.content', json->'$.payload.content') AS c FROM obj),
        st  AS (SELECT source, json_group_structure(json) AS st FROM obj GROUP BY source),
        bst AS (SELECT source, json_group_structure(e) AS bst
                FROM arr CROSS JOIN UNNEST(from_json(c, '["JSON"]')) AS t(e) WHERE json_type(c) = 'ARRAY' GROUP BY source)
        SELECT source, st, bst FROM st JOIN bst USING (source)$s$,
        string_agg(printf($q$'%s'$q$, path), ', ')) AS sql,
        sum(file_size(path)) AS sample_bytes, count(path) AS sample_files
    FROM sample
),
receipt AS (
    SELECT sample_bytes, sample_files, quackapi_post('http://localhost:9495/sql', json_object('sql', sql)::VARCHAR) AS r FROM program
),
structures AS (
    SELECT sample_bytes, sample_files, s->>'source' AS source, (s->>'st')::JSON AS st, (s->>'bst')::JSON AS bst
    FROM receipt CROSS JOIN UNNEST(json_extract(r.body, '$[*]')) AS t(s)
),
mapping AS (
    SELECT source, type, split_part(p, '[', 1) AS p
    FROM read_csv('/Users/aloksubbarao/worktrees/agent_data-claude-fields/design/mapping.csv')
    CROSS JOIN UNNEST(string_split(json_paths, '|')) AS t(p)
),
ignored AS (
    SELECT source, json_path AS p FROM read_csv('/Users/aloksubbarao/worktrees/agent_data-claude-fields/design/ignore.csv')
    WHERE json_path NOT IN ('payload', '*')
    EXCEPT SELECT source, p FROM mapping
),
rules AS (
    SELECT s.source,
           json_object('drop', coalesce(list(i.p) FILTER (WHERE NOT starts_with(i.p, '@')), []),
                       'json', coalesce(any_value(j.line_json), []))                                AS line_rules,
           json_object('drop', coalesce(list(replace(i.p, '@block.', '')) FILTER (WHERE starts_with(i.p, '@block.')), []),
                       'json', coalesce(any_value(j.block_json), []))                               AS block_rules
    FROM structures s
    LEFT JOIN ignored i USING (source)
    LEFT JOIN (SELECT source,
                      list(DISTINCT p) FILTER (WHERE NOT starts_with(p, '@'))                       AS line_json,
                      list(DISTINCT replace(p, '@block.', '')) FILTER (WHERE starts_with(p, '@block.')) AS block_json
               FROM mapping WHERE type = 'JSON' AND p NOT IN ('payload', '@line') GROUP BY source) j USING (source)
    GROUP BY s.source
),
declared AS (
    SELECT s.source, s.sample_bytes, s.sample_files,
      jsonata($j$(
        $ident := function($k){ $count($filter($split($k, ""), function($c){ $not($contains("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_", $c)) })) = 0 };
        $ismap := function($s){ $count($filter($keys($s), function($k){ $not($ident($k)) })) > 0 };
        $path  := function($p, $k){ $p = "" ? $k : $p & "." & $k };
        $kept  := function($s, $p){ $filter($keys($s), function($k){ $not($path($p, $k) in $drop) }) };
        $f := function($s, $p){ $p in $json ? "JSON" : $type($s) = "object" ? ($ismap($s) ? "JSON" :
                 "STRUCT(" & $join($map($kept($s, $p), function($k){ "\"" & $k & "\" " & $f($lookup($s, $k), $path($p, $k)) }), ", ") & ")")
               : $type($s) = "array" ? "JSON" : $s = "NULL" ? "JSON" : $s };
        $merge($map($kept($, ""), function($k){ {$k: $f($lookup($, $k), $k)} }))
      )$j$, s.st, r.line_rules) AS columns,
      jsonata($j$(
        $ident := function($k){ $count($filter($split($k, ""), function($c){ $not($contains("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_", $c)) })) = 0 };
        $ismap := function($s){ $count($filter($keys($s), function($k){ $not($ident($k)) })) > 0 };
        $path  := function($p, $k){ $p = "" ? $k : $p & "." & $k };
        $kept  := function($s, $p){ $filter($keys($s), function($k){ $not($path($p, $k) in $drop) }) };
        $g := function($s, $p){ $p in $json ? "JSON" : $type($s) = "object" ? ($ismap($s) ? "JSON" :
                 $merge($map($kept($s, $p), function($k){ {$k: $g($lookup($s, $k), $path($p, $k))} })))
               : $type($s) = "array" ? "JSON" : $s = "NULL" ? "JSON" : $s };
        $g($, "")
      )$j$, s.bst, r.block_rules) AS block
    FROM structures s JOIN rules r USING (source)
),
literal AS (
    SELECT '{' || string_agg(DISTINCT printf($f$'%s': '%s'$f$, k, columns->>k), ', ') || '}' AS declared_literal
    FROM declared CROSS JOIN UNNEST(json_keys(columns)) AS t(k)
),
blocks AS (
    SELECT any_value(sample_files) AS sample_files, any_value(sample_bytes) AS sample_bytes,
           json_merge_patch(any_value(block) FILTER (WHERE source = 'claude'),
                            any_value(block) FILTER (WHERE source = 'codex')) AS block_structure
    FROM declared
)
SELECT blocks.sample_files, blocks.sample_bytes, literal.declared_literal, blocks.block_structure
FROM blocks POSITIONAL JOIN literal;
*/

-- §3 ─────────────────────────────────────────────────────────────────────────────────────────
-- reads: PR #1576's dispatch, pointed at dev's /sql instead of :8080, one post per directory
-- (dirs) or per file (files). 1576 posts CREATE TABLE; here the statement is a SELECT and the
-- receipt keeps the rows as JSON, so nothing is created. A failed read falls back to read_blob.
CREATE OR REPLACE VIEW reads AS
WITH work AS (
    SELECT 'dir' AS unit, parent AS target, 'read_ndjson' AS reader, files, sql_statement FROM dirs
    UNION ALL
    SELECT 'file', path, reader, [path], sql_statement FROM files
),
cte AS (
    SELECT work.*, quackapi_post('http://localhost:9495/sql', json_object('sql', sql_statement)::VARCHAR) AS response
    FROM work
)
SELECT unit, target, reader, files, response.status,
       COALESCE(CASE WHEN response.status = 200 THEN reader END,
                CASE WHEN quackapi_post('http://localhost:9495/sql',
                              json_object('sql', printf($s$SELECT * FROM read_blob([%s])$s$,
                                  list_aggr(list_transform(files, f -> printf($q$'%s'$q$, f)), 'string_agg', ', ')))::VARCHAR).status = 200
                     THEN 'read_blob' END) AS read_by,
       response.body AS rows_json
FROM cte;

-- lines_program: the same per-directory statements rendered into one typed program
-- (self-dispatch "constructed program"): UNION ALL BY NAME of every directory's read. setup.sql
-- runs the text it returns, which creates lines_raw. Typed rows, no JSON round trip.
CREATE OR REPLACE VIEW lines_program AS
SELECT 'CREATE OR REPLACE VIEW lines_raw AS ' || string_agg(sql_statement, ' UNION ALL BY NAME ' ORDER BY parent) AS ddl
FROM dirs;

-- §4 ─────────────────────────────────────────────────────────────────────────────────────────
-- lines: one row per transcript record, key (path, line_number); record identity (uuid,
-- message.id, call_id, payload.id) is the join key wherever a record has one.
--  * line_number = WITH ORDINALITY minus the file's first ordinal + 1. Verified equal to
--    read_lines' line_number on 9,925/9,925 lines of 36 files (row_number() without ORDER BY
--    matched only 9,745), and equal to read_conversations' line_number on all 9,464 shared keys.
--  * source by content: Codex when the file has a session_meta record, Claude when records carry
--    sessionId and type; other .jsonl (plugins, jobs, history) are dropped here.
--  * Codex session_id = the FIRST session_meta's payload.id (forked children replay the parent's).
CREATE OR REPLACE VIEW lines AS
WITH numbered AS (
    -- records carry their own top-level `source` and `path` keys: renamed so `source` can be
    -- claude/codex and `path` the file
    SELECT lines_raw.* EXCLUDE (filename, ordinality) RENAME (source AS record_source, path AS record_path),
           filename                                                         AS path,
           ordinality - min(ordinality) OVER (PARTITION BY filename) + 1    AS line_number
    FROM lines_raw
),
typed AS (
    SELECT numbered.*,
           CASE WHEN bool_or(type = 'session_meta') OVER f THEN 'codex'
                WHEN bool_or(sessionId IS NOT NULL AND type IS NOT NULL) OVER f THEN 'claude' END AS source,
           first_value(CASE WHEN type = 'session_meta' THEN payload.id END IGNORE NULLS) OVER f AS codex_session_id,
           first_value(CASE WHEN type = 'session_meta'
                            THEN coalesce(payload.parent_thread_id, payload.source->>'$.subagent.thread_spawn.parent_thread_id') END IGNORE NULLS) OVER f AS codex_parent_id,
           file_name(parse_dirpath(path)) = 'subagents'                     AS is_subagent,
           CASE WHEN is_subagent THEN file_name(parse_dirpath(parse_dirpath(path))) END AS folder_session,
           CASE WHEN is_subagent THEN replace(parse_filename(path, true), 'agent-', '') END AS agent_id
    FROM numbered
    WINDOW f AS (PARTITION BY path ORDER BY line_number ROWS BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING)
)
SELECT typed.* EXCLUDE (codex_session_id, codex_parent_id, folder_session),
       CASE WHEN source = 'codex' THEN codex_session_id
            WHEN is_subagent      THEN folder_session
            ELSE replace(parse_filename(path, true), '.jsonl', '') END     AS session_id,
       CASE WHEN source = 'codex' THEN codex_parent_id ELSE folder_session END AS parent_session_id,
       CASE WHEN source = 'codex' THEN concat_ws('/', type, payload.type, payload.item.type) ELSE type END AS record_type,
       -- carried turn: Claude promptId; Codex turn_id on the payload
       last_value(CASE WHEN source = 'claude' THEN promptId
                       ELSE coalesce(payload.turn_id, payload.internal_chat_message_metadata_passthrough.turn_id) END IGNORE NULLS)
           OVER (PARTITION BY path ORDER BY line_number ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS turn_id
FROM typed
WHERE source IS NOT NULL;

-- blocks: content blocks, key (path, line_number, block_index). Claude message.content and Codex
-- payload.content are a string or a block list; a string is one text block.
CREATE OR REPLACE VIEW blocks AS
WITH c AS (
    SELECT path, line_number, source, session_id, turn_id, record_type, type, uuid, timestamp,
           coalesce(message.content, payload.content) AS content
    FROM lines
    WHERE coalesce(message.content, payload.content) IS NOT NULL
)
SELECT c.* EXCLUDE (content), b.block_index, b.block.type AS block_type, b.block
FROM c CROSS JOIN UNNEST(from_json(CASE json_type(content) WHEN 'ARRAY' THEN content
                                        ELSE json_array(json_object('type', 'text', 'text', content->>'$')) END,
                                   -- §2's block_structure (Claude and Codex block keys merged)
                                   '[{"type": "VARCHAR", "text": "VARCHAR", "thinking": "VARCHAR", "id": "VARCHAR", "name": "VARCHAR",
                                      "input": "JSON", "tool_use_id": "VARCHAR", "content": "JSON", "is_error": "BOOLEAN",
                                      "source": "JSON", "image_url": "VARCHAR", "detail": "VARCHAR", "encrypted_content": "VARCHAR"}]'))
     WITH ORDINALITY AS b(block, block_index);

-- §5 ─────────────────────────────────────────────────────────────────────────────────────────
-- sessions: one row per transcript file (Claude subagent files share the parent's session_id
-- and differ by agent_id). Facts are first/last non-null values in line order.
CREATE OR REPLACE VIEW sessions AS
SELECT source, session_id, path, is_subagent, agent_id,
       any_value(parent_session_id)                                                         AS parent_session_id,
       arg_min(payload.forked_from_id, line_number) FILTER (WHERE type = 'session_meta')    AS forked_from_session_id,
       coalesce(arg_max(agentName, line_number) FILTER (WHERE agentName IS NOT NULL),
                arg_min(payload.agent_nickname, line_number) FILTER (WHERE type = 'session_meta')) AS agent_name,
       arg_max(agentSetting, line_number) FILTER (WHERE agentSetting IS NOT NULL)           AS agent_role,
       arg_min(payload.agent_path, line_number) FILTER (WHERE type = 'session_meta')        AS agent_path,
       arg_max(coalesce(aiTitle, title), line_number) FILTER (WHERE coalesce(aiTitle, title) IS NOT NULL) AS title,
       arg_max(customTitle, line_number) FILTER (WHERE customTitle IS NOT NULL)             AS custom_title,
       arg_max(mode, line_number) FILTER (WHERE mode IS NOT NULL)                           AS mode,
       arg_max(permissionMode, line_number) FILTER (WHERE permissionMode IS NOT NULL)       AS permission_mode,
       arg_max(prRepository, line_number) FILTER (WHERE prRepository IS NOT NULL)           AS pr_repository,
       arg_max(prUrl, line_number) FILTER (WHERE prUrl IS NOT NULL)                         AS pr_url,
       arg_max(bridgeSessionId, line_number) FILTER (WHERE bridgeSessionId IS NOT NULL)     AS channel,
       arg_max(lastPrompt, line_number) FILTER (WHERE lastPrompt IS NOT NULL)               AS last_prompt,
       arg_max(continuedInSessionId, line_number) FILTER (WHERE continuedInSessionId IS NOT NULL) AS continued_in_session_id,
       arg_max(slug, line_number) FILTER (WHERE slug IS NOT NULL)                           AS slug,
       coalesce(arg_min(cwd, line_number) FILTER (WHERE cwd IS NOT NULL),
                arg_min(payload.cwd, line_number) FILTER (WHERE type = 'session_meta'))     AS cwd,
       coalesce(arg_max(gitBranch, line_number) FILTER (WHERE gitBranch IS NOT NULL),
                arg_min(payload.git.branch, line_number) FILTER (WHERE type = 'session_meta')) AS git_branch,
       arg_min(payload.git.commit_hash, line_number) FILTER (WHERE type = 'session_meta')   AS git_commit,
       arg_min(payload.git.repository_url, line_number) FILTER (WHERE type = 'session_meta') AS repository,
       coalesce(arg_max(version, line_number) FILTER (WHERE version IS NOT NULL),
                arg_min(payload.cli_version, line_number) FILTER (WHERE type = 'session_meta')) AS version,
       coalesce(arg_max(entrypoint, line_number) FILTER (WHERE entrypoint IS NOT NULL),
                arg_min(payload.originator, line_number) FILTER (WHERE type = 'session_meta')) AS client,
       arg_min(coalesce(payload.thread_source, payload.source->>'$'), line_number) FILTER (WHERE type = 'session_meta') AS thread_source,
       arg_min(payload.model_provider, line_number) FILTER (WHERE type = 'session_meta')    AS model_provider,
       arg_min(timestamp, line_number)                                                      AS started_at,
       arg_max(timestamp, line_number)                                                      AS ended_at
FROM lines
GROUP BY source, session_id, path, is_subagent, agent_id;

-- turns: Claude = promptId groups; Codex = turn_context + task_started/complete/turn_aborted.
CREATE OR REPLACE VIEW turns AS
SELECT source, session_id, path, turn_id,
       arg_max(payload.root_turn_id, line_number) FILTER (WHERE payload.root_turn_id IS NOT NULL) AS root_turn_id,
       coalesce(arg_max(payload.model, line_number) FILTER (WHERE type = 'turn_context'),
                arg_max(message.model, line_number) FILTER (WHERE type = 'assistant'))      AS model,
       coalesce(arg_max(coalesce(payload.effort, payload.collaboration_mode.settings.reasoning_effort), line_number) FILTER (WHERE type = 'turn_context'),
                arg_max(coalesce(perTurnEffort, effort), line_number) FILTER (WHERE coalesce(perTurnEffort, effort) IS NOT NULL)) AS effort,
       coalesce(arg_max(payload.cwd, line_number) FILTER (WHERE type = 'turn_context'),
                arg_max(cwd, line_number) FILTER (WHERE cwd IS NOT NULL))                   AS cwd,
       arg_min(timestamp, line_number)                                                      AS started_at,
       arg_max(timestamp, line_number)                                                      AS ended_at,
       arg_max(CASE payload.type WHEN 'task_complete' THEN 'completed' WHEN 'turn_aborted' THEN coalesce(payload.reason, 'aborted') END, line_number)
           FILTER (WHERE payload.type IN ('task_complete', 'turn_aborted'))                 AS status,
       arg_max(payload.last_agent_message, line_number) FILTER (WHERE payload.type = 'task_complete') AS last_message
FROM lines
WHERE turn_id IS NOT NULL
GROUP BY source, session_id, path, turn_id;

-- messages: text blocks of Claude user/assistant lines and Codex response_item message and
-- agent_message. Codex event_msg item_completed UserMessage/AgentMessage are client copies of
-- the response_item lines and are left out.
CREATE OR REPLACE VIEW messages AS
SELECT b.source, b.session_id, b.path, b.line_number, b.block_index, b.turn_id, b.timestamp,
       l.uuid, coalesce(l.parentUuid, l.logicalParentUuid)                    AS parent_uuid,
       coalesce(l.payload.role, l.type)                                       AS role,
       coalesce(l.payload.author, CASE WHEN l.isMeta THEN 'harness' END)      AS author,
       coalesce(l.promptSource, l.turnOrigin, l.origin->>'kind')              AS prompt_source,
       b.block.text                                                           AS text,
       l.payload.phase                                                        AS phase,
       l.payload.recipient                                                    AS recipient
FROM blocks b JOIN lines l USING (path, line_number)
WHERE b.block_type IN ('text', 'input_text', 'output_text')
  AND b.record_type IN ('user', 'assistant', 'response_item/message', 'response_item/agent_message');

-- tool_calls: key tool_use_id.
CREATE OR REPLACE VIEW tool_calls AS
SELECT b.source, b.session_id, b.path, b.line_number, b.turn_id, b.timestamp,
       b.block.id AS tool_use_id, b.block.name AS name, b.block.input AS input,
       NULL::VARCHAR AS server, NULL::VARCHAR AS namespace, b.record_type
FROM blocks b WHERE b.source = 'claude' AND b.block_type = 'tool_use'
UNION ALL BY NAME
SELECT source, session_id, path, line_number, turn_id, timestamp,
       payload.call_id AS tool_use_id, payload.name AS name,
       -- function_call arguments are a JSON document inside a JSON string; custom tools send raw text
       coalesce(try_cast(payload.arguments->>'$' AS JSON), to_json(payload.input)) AS input,
       NULL AS server, payload.namespace AS namespace, record_type
FROM lines WHERE record_type IN ('response_item/function_call', 'response_item/custom_tool_call')
UNION ALL BY NAME
SELECT source, session_id, path, line_number, turn_id, timestamp,
       payload.item.id AS tool_use_id,
       coalesce(payload.item.tool, 'command') AS name,                 -- read_conversations' label
       coalesce(payload.item.arguments, payload.item.command) AS input,
       payload.item.server AS server, NULL AS namespace, record_type
FROM lines WHERE record_type IN ('event_msg/item_completed/McpToolCall', 'event_msg/item_completed/CommandExecution');

-- tool_results: key tool_use_id. Codex MCP and command items carry call and result on one line.
CREATE OR REPLACE VIEW tool_results AS
SELECT b.source, b.session_id, b.path, b.line_number, b.turn_id, b.timestamp,
       b.block.tool_use_id AS tool_use_id, b.block.content AS content, b.block.is_error AS is_error,
       NULL::VARCHAR AS status, NULL::BIGINT AS exit_code, l.toolUseResult AS result, b.record_type
FROM blocks b JOIN lines l USING (path, line_number)
WHERE b.source = 'claude' AND b.block_type = 'tool_result'
UNION ALL BY NAME
SELECT source, session_id, path, line_number, turn_id, timestamp,
       payload.call_id AS tool_use_id, payload.output AS content, NULL AS is_error,
       payload.status AS status, NULL AS exit_code, NULL AS result, record_type
FROM lines WHERE record_type IN ('response_item/function_call_output', 'response_item/custom_tool_call_output')
UNION ALL BY NAME
SELECT source, session_id, path, line_number, turn_id, timestamp,
       payload.item.id AS tool_use_id,
       coalesce(to_json(payload.item.aggregated_output), payload.item.result, payload.item.error) AS content,
       payload.item.status = 'failed' AS is_error,
       payload.item.status AS status, payload.item.exit_code AS exit_code, NULL AS result, record_type
FROM lines WHERE record_type IN ('event_msg/item_completed/McpToolCall', 'event_msg/item_completed/CommandExecution');

-- thinking: Claude thinking/redacted_thinking blocks (empty text = redacted on 2026-10 lines);
-- Codex reasoning items, one row per summary part (encrypted_content = the body is withheld;
-- an item with no summary is one row with NULL text).
CREATE OR REPLACE VIEW thinking AS
SELECT b.source, b.session_id, b.path, b.line_number, b.block_index, b.turn_id, b.timestamp,
       l.message.id AS response_id,
       nullif(b.block.thinking, '') AS text,
       b.block_type = 'redacted_thinking' OR coalesce(b.block.thinking, '') = '' AS redacted
FROM blocks b JOIN lines l USING (path, line_number)
WHERE b.source = 'claude' AND b.block_type IN ('thinking', 'redacted_thinking')
UNION ALL BY NAME
SELECT source, session_id, path, line_number, s.block_index, turn_id, timestamp,
       payload.id AS response_id, s.part.text AS text,
       payload.encrypted_content IS NOT NULL AS redacted
FROM lines CROSS JOIN UNNEST(CASE WHEN json_array_length(payload.summary) > 0
                                  THEN from_json(payload.summary, '[{"text":"VARCHAR"}]') ELSE [NULL] END)
          WITH ORDINALITY AS s(part, block_index)
WHERE record_type = 'response_item/reasoning';

-- usage: Claude one row per API response (message.id). Every line of a response repeats its
-- usage, so the last line is taken: this is the fix for read_conversations' overcount. API-error
-- lines have no message.id and are keyed by their uuid. Codex: token_usage_record names its
-- response; older rollouts have only event_msg token_count, whose last_token_usage is that
-- response's usage ('legacy', keyed by its line).
CREATE OR REPLACE VIEW usage AS
WITH has_record AS (SELECT DISTINCT path FROM lines WHERE record_type = 'token_usage_record')
SELECT source, session_id, path,
       coalesce(message.id, uuid)                                           AS response_id,
       arg_max(line_number, line_number)                                    AS line_number,
       arg_max(turn_id, line_number)                                        AS turn_id,
       arg_max(timestamp, line_number)                                      AS timestamp,
       arg_max(message.model, line_number)                                  AS model,
       arg_max(message.usage.input_tokens, line_number)                     AS input_tokens,
       arg_max(message.usage.output_tokens, line_number)                    AS output_tokens,
       arg_max(message.usage.cache_creation_input_tokens, line_number)      AS cache_creation_tokens,
       arg_max(message.usage.cache_read_input_tokens, line_number)          AS cache_read_tokens,
       arg_max(message.usage.output_tokens_details.thinking_tokens, line_number) AS thinking_tokens,
       arg_max(message.stop_reason, line_number)                            AS stop_reason,
       arg_max(coalesce(perTurnEffort, effort), line_number)                AS effort,
       arg_max(coalesce(apiErrorStatus::VARCHAR, apiError, error->>'$'), line_number) AS error,
       'response'                                                           AS usage_scope
FROM lines WHERE source = 'claude' AND type = 'assistant'
GROUP BY source, session_id, path, coalesce(message.id, uuid)
UNION ALL BY NAME
SELECT source, session_id, path, payload.response_id AS response_id, line_number, turn_id, timestamp,
       payload.usage.input_tokens AS input_tokens, payload.usage.output_tokens AS output_tokens,
       payload.usage.cache_write_input_tokens AS cache_creation_tokens, payload.usage.cached_input_tokens AS cache_read_tokens,
       payload.usage.reasoning_output_tokens AS thinking_tokens, 'response' AS usage_scope
FROM lines WHERE record_type = 'token_usage_record'
UNION ALL BY NAME
SELECT l.source, l.session_id, l.path, l.line_number, l.turn_id, l.timestamp,
       l.payload.info.last_token_usage.input_tokens AS input_tokens,
       l.payload.info.last_token_usage.output_tokens AS output_tokens,
       l.payload.info.last_token_usage.cache_write_input_tokens AS cache_creation_tokens,
       l.payload.info.last_token_usage.cached_input_tokens AS cache_read_tokens,
       l.payload.info.last_token_usage.reasoning_output_tokens AS thinking_tokens,
       'legacy' AS usage_scope
FROM lines l ANTI JOIN has_record h USING (path)
WHERE l.record_type = 'event_msg/token_count';

-- attachments: Claude attachment lines. The text the model saw is `rendered[*].content` (one or
-- more parts, kept as a list); reminders = the <system-reminder> bodies in those parts, parsed
-- with webbed (html_extract_text over parse_html; strict XML and html_to_duck_blocks fail on
-- them). Bounded set: 1,308 of 1,499 attachment lines are rendered, all 1,308 carry the tag,
-- 1,310 reminders parse; the other 191 (hook bookkeeping) were never shown to the model.
CREATE OR REPLACE VIEW attachments AS
WITH a AS (
    SELECT source, session_id, path, line_number, turn_id, timestamp, uuid,
           attachment->>'type' AS kind, attachment->>'hookEvent' AS hook_event,
           coalesce(attachment->>'toolUseID', toolUseID) AS tool_use_id,
           json_extract_string(rendered, '$[*].content') AS texts,
           attachment                                                    -- the rest, by kind
    FROM lines WHERE type = 'attachment'
)
SELECT a.*, flatten(list_transform(texts, t -> html_extract_text(parse_html(t), '//system-reminder'))) AS reminders
FROM a;

-- system_events: Claude system and queue-operation lines; Codex records that are not messages,
-- tool items, reasoning or usage (task/thread events, world_state, realtime_item, inter-agent,
-- and the item_completed kinds FileChange, Extension, SubAgentActivity).
CREATE OR REPLACE VIEW system_events AS
SELECT source, session_id, path, line_number, turn_id, timestamp,
       coalesce(subtype, operation) AS subtype, content,
       -- json_object, not to_json({...}): the struct form failed under SUMMARIZE (cast of the
       -- printed struct to JSON)
       json_object('stopReason', stopReason, 'level', level, 'durationMs', durationMs, 'hookCount', hookCount,
                   'hookInfos', hookInfos, 'hookErrors', hookErrors, 'preventedContinuation', preventedContinuation,
                   'compactMetadata', compactMetadata, 'isCompactSummary', isCompactSummary) AS payload,
       record_type
FROM lines WHERE source = 'claude' AND type IN ('system', 'queue-operation')
UNION ALL BY NAME
SELECT source, session_id, path, line_number, turn_id, timestamp,
       coalesce(payload.type, type) AS subtype, coalesce(payload.message, payload.text) AS content,
       payload::JSON AS payload, record_type
FROM lines
WHERE source = 'codex'
  AND type NOT IN ('response_item', 'session_meta', 'turn_context', 'token_usage_record')
  AND record_type NOT IN ('event_msg/token_count', 'event_msg/item_completed/UserMessage', 'event_msg/item_completed/AgentMessage',
                          'event_msg/item_completed/Reasoning', 'event_msg/item_completed/McpToolCall',
                          'event_msg/item_completed/CommandExecution');
