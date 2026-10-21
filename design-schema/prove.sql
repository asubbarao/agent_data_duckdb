-- Proof run (disposable local duckdb only): materialize every view for one batch of files.
-- Every view except `copies` depends on one file only, so a batch of whole files gives exact rows;
-- this is the same property whole-file-replace ingest relies on.
--   duckdb /private/tmp/claude-501/agent_data_schema/work.duckdb -f schema.sql          (once)
--   BATCH=k duckdb /private/tmp/claude-501/agent_data_schema/work.duckdb -f prove.sql   (k = 0..7)
-- Output: out/<view>/batch=k/data_0.parquet. 8 batches: 17-35 s each, peak RSS 2.6-4.4 GB.
SET threads = 2; SET memory_limit = '3GB'; SET preserve_insertion_order = false;
SET temp_directory = '/private/tmp/claude-501/agent_data_schema/tmp';
LOAD webbed;
CREATE OR REPLACE TEMP VIEW lines_input AS
FROM read_parquet('/private/tmp/claude-501/agent_data_selfdispatch/lines/*/*.parquet', hive_partitioning := true)
WHERE scope <> 'bounded' AND hash(file) % 8 = getenv('BATCH')::INT;
COPY (SELECT * EXCLUDE (record), strlen(record::VARCHAR) AS record_bytes, getenv('BATCH') AS batch FROM lines)
  TO '/private/tmp/claude-501/agent_data_schema/out/lines' (FORMAT parquet, PARTITION_BY (batch), OVERWRITE_OR_IGNORE);
COPY (SELECT *, getenv('BATCH') AS batch FROM tool_calls)
  TO '/private/tmp/claude-501/agent_data_schema/out/tool_calls' (FORMAT parquet, PARTITION_BY (batch), OVERWRITE_OR_IGNORE);
COPY (SELECT *, getenv('BATCH') AS batch FROM usage)
  TO '/private/tmp/claude-501/agent_data_schema/out/usage' (FORMAT parquet, PARTITION_BY (batch), OVERWRITE_OR_IGNORE);
COPY (SELECT *, getenv('BATCH') AS batch FROM messages)
  TO '/private/tmp/claude-501/agent_data_schema/out/messages' (FORMAT parquet, PARTITION_BY (batch), OVERWRITE_OR_IGNORE);
COPY (SELECT *, getenv('BATCH') AS batch FROM thinking)
  TO '/private/tmp/claude-501/agent_data_schema/out/thinking' (FORMAT parquet, PARTITION_BY (batch), OVERWRITE_OR_IGNORE);
COPY (SELECT *, getenv('BATCH') AS batch FROM attachments)
  TO '/private/tmp/claude-501/agent_data_schema/out/attachments' (FORMAT parquet, PARTITION_BY (batch), OVERWRITE_OR_IGNORE);
COPY (SELECT * EXCLUDE (record), strlen(record::VARCHAR) AS record_bytes, getenv('BATCH') AS batch FROM system_events)
  TO '/private/tmp/claude-501/agent_data_schema/out/system_events' (FORMAT parquet, PARTITION_BY (batch), OVERWRITE_OR_IGNORE);
COPY (SELECT *, getenv('BATCH') AS batch FROM sessions)
  TO '/private/tmp/claude-501/agent_data_schema/out/sessions' (FORMAT parquet, PARTITION_BY (batch), OVERWRITE_OR_IGNORE);
