# Claude Code ↔ Codex: concept mapping

Source paths are JSON paths in one transcript line (`$` = the line). Codex wraps every line as
`{timestamp, type, payload:{type, …}}` (newer rollouts add `ordinal` and `metadata`); the substance is in
`payload`. Claude puts most facts at the top level and the conversation in `message.content[*]` blocks.
Full path list with fill counts: `inventory.csv` (every path, its JSON types, lines filled, and its home).

| concept | Claude source | Codex source | semantic differences |
|---|---|---|---|
| line key | file + line number | file + line number (`ordinal` = line − 1 in 826 of 831 files) | same: `read_lines` line_number |
| record type | `type` (+ `subtype` for system, `attachment.type`) | `type` / `payload.type` (+ `payload.item.type` for item_completed) | `lines.record_type` joins them with `/` |
| session | `sessionId` (`session_id` on some attachments) | file name uuid; `session_meta.payload.id` | a Claude subagent file carries the parent's `sessionId` + its own `agentId`; a Codex subagent or fork is its own file/thread (`payload.parent_thread_id`, `forked_from_id`) |
| subagent | `agentId`, file under `<session>/subagents/` | `session_meta.payload.agent_nickname/agent_path/agent_role` | Claude: one session, many agent files; Codex: one thread per agent |
| turn | `promptId` on user lines; carried to later lines (ASOF) | `payload.turn_id`, `payload.internal_chat_message_metadata_passthrough.turn_id` | Codex marks most lines; Claude only user prompts |
| timestamp | `timestamp` | `timestamp` | both ISO UTC |
| line identity / threading | `uuid`, `parentUuid` | `payload.id` (item id) | Codex has no parent chain; order is the line number |
| API response | `message.id` (+ `requestId`) on every assistant line of that response | `token_usage_record.payload.response_id` | Claude writes one content block per line and repeats `usage` on each; Codex writes one usage record after the response's items |
| model | `message.model` | `turn_context.payload.model` (per turn) | Codex model is per turn, joined ASOF to each usage record |
| tokens | `message.usage.{input_tokens, cache_read_input_tokens, cache_creation_input_tokens, output_tokens, output_tokens_details.thinking_tokens}` | `payload.usage.{input_tokens, cached_input_tokens, cache_write_input_tokens, output_tokens, reasoning_output_tokens}`; older: `event_msg/token_count payload.info.last_token_usage` | Claude `input_tokens` excludes cache; Codex `input_tokens` includes cached. `usage.total_input_tokens` = Claude input + cache read + cache write, Codex input. Never use Codex `total_token_usage`/`thread_token_usage`/`turn_token_usage` (cumulative) |
| tool call | `message.content[*]` where `type = tool_use`: `id`, `name`, `input` | `response_item` `function_call` (`call_id`, `name`, `namespace`, `arguments` = JSON text), `custom_tool_call` (`input` = free text, e.g. `exec` JS), `tool_search_call` | Codex `exec` is a code-mode call: the work happens inside it |
| nested tool work | — (Claude has none in these files) | `event_msg/item_completed` with `item.type` CommandExecution, McpToolCall, FileChange, DynamicToolCall, CollabAgentToolCall, Extension, ImageView; `item.id` not a call id | `tool_calls.level = 'nested'`, `parent_call_id` = the model call before it. When `item.id` = a `call_id` (1,485 MCP, 981 collab, 376 extension) the item is that call's `result_detail`, not a new row |
| tool result | user line, `message.content[*]` where `type = tool_result`: `tool_use_id`, `content` (text or blocks), `is_error`; structured copy in top-level `toolUseResult` | `function_call_output` / `custom_tool_call_output` (`call_id`, `output` = text or `[{type,text}]`) ; nested: `item.aggregated_output`, `item.result`, `item.error`, `item.exit_code`, `item.status` | Claude `is_error` is set on 63,044 of 101,801 results; Codex model-level results carry no error flag (only when a matched item says so) |
| call duration | result line `timestamp` − call line `timestamp` | same for model calls; nested: `payload.completed_at_ms − started_at_ms` | |
| visible text | `message.content[*].text` (string content = one text block) | `response_item/message payload.content[*].text` (`input_text`/`output_text`), `agent_message` | Claude user text includes `<system-reminder>` blocks; Codex `developer` role carries instructions |
| thinking | `message.content[*].thinking` + `signature` (text empty when redacted) | `response_item/reasoning payload.summary[*].text` + `encrypted_content` | 92.6 % of thinking rows have no readable text (encrypted/redacted) |
| injected context | `attachment` records (43 kinds), `rendered[*].content` | none as records (`developer` messages, `world_state`) | Claude only |
| session state | `ai-title`, `custom-title`, `mode`, `permission-mode`, `pr-link`, `agent-name`, `last-prompt`, `bridge-session`, `worktree-state`, `cost-state`, `continued-in`, … | `session_meta`, `turn_context`, `thread_settings_applied`, `world_state` | kept whole in `system_events.record`; `sessions` picks cwd/branch/version/title/parent |
| lifecycle | `system/turn_duration`, `system/stop_hook_summary`, `system/api_error`, `system/compact_boundary`, `queue-operation` | `event_msg/task_started`, `task_complete`, `turn_aborted`, `compacted`, `item_completed/ContextCompaction` | `system_events` |

## Source-specific (no counterpart; kept in `lines.record` or a JSON column)

- **Claude only:** `attachment` (all kinds), `rendered`, `toolUseResult` (structured result copy),
  `wireToolInputs` / `wireIngestContext` (map: tool id → input), `file-history-*` `snapshot.trackedFileBackups`
  (map: file path → backup), `serverClassifier*`, `attribution*`, `permissionDecision`, `mcpMeta`,
  `perTurnEffort`/`effort`/`advisorModel`, `apiBlockIndex`, `thinkingDurationMs`, `message.usage.iterations`,
  `cache_creation.ephemeral_{5m,1h}`, `server_tool_use`, `message.diagnostics.cache_miss_reason`,
  `isSidechain`, `userType`, `entrypoint`, `slug`, `sessionKind`, `cost-state` totals, `atis-latch`, `frame-link`, `artifact-*`.
- **Codex only:** `ordinal`, `metadata.*` (`client_authored`, `user_input_order`, `fallback_token_limit_override`,
  `mcp_attribution`, `retained_source`), `compacted.payload.replacement_history` (5–21 MB lines),
  `turn_context` sandbox/approval/permission profiles, `session_meta.base_instructions`/`context_window`/`dynamic_tools`,
  `token_count.payload.rate_limits`, `thread_token_usage`/`turn_token_usage` (cumulative),
  `inter_agent_communication_metadata`, `realtime_item/*`, `retained_context/*`, `world_state.payload.state`,
  `item_completed` Reasoning/AgentMessage/UserMessage (duplicates of `response_item` lines).

## Map-shaped objects (keys are data; collapsed to `{*}` in the inventory, kept as JSON)

Detected as parents with > 40 distinct keys and < 300 lines per key:
`$.wireToolInputs`, `$.wireIngestContext`, `$.snapshot.trackedFileBackups`, `$.toolUseResult.answers`,
`$.message.content[*].input.files`, `$.attachment.tools[*].schema.input_schema.properties`,
`$.attachment.entries[*].input_schema.properties`, `$.attachment.surfacedDefinitions[*].definition.input_schema.properties`
(Claude); `$.payload.item.changes`, `$.payload.item.arguments`, `$.payload.item.result.structuredContent` (Codex).
Tool inputs and results are the same kind of thing (each tool defines its own keys), so `tool_calls.input`,
`result` and `result_detail` stay JSON.
