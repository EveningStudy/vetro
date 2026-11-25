CREATE TABLE sessions (
    id TEXT PRIMARY KEY, title TEXT, working_directory TEXT, created_at INTEGER,
    last_activity_at INTEGER, model TEXT, hidden INTEGER, main_chain_id INTEGER
);
CREATE TABLE message_nodes (
    session_id TEXT, node_id INTEGER, parent_node_id INTEGER, created_at INTEGER,
    chat_message TEXT, PRIMARY KEY (session_id, node_id)
);
CREATE TABLE tool_call_state (
    session_id TEXT, tool_call_id TEXT, tool_call_json TEXT, tool_call_update_json TEXT,
    PRIMARY KEY (session_id, tool_call_id)
);
INSERT INTO sessions VALUES ('fixture-parent','Background sample','/fixture',1800000000,1800000004,'fixture-model',0,6);
INSERT INTO message_nodes VALUES
    ('fixture-parent',1,NULL,1800000000,'{"message_id":"fixture-1","role":"user","content":"Run the sample.","metadata":{"is_user_input":true}}'),
    ('fixture-parent',2,1,1800000001,'{"message_id":"fixture-2","role":"assistant","tool_calls":[{"id":"fixture-launch","name":"exec","arguments":{"command":"fixture-wrapper","workdir":"/fixture","timeout":1,"shell_flavor":"powershell"}}]}'),
    ('fixture-parent',3,2,1800000001,'{"message_id":"fixture-3","role":"tool","tool_call_id":"fixture-launch","content":"Command running in background with ID: fixture-shell","metadata":{}}'),
    ('fixture-parent',4,3,1800000002,'{"message_id":"fixture-4","role":"assistant","tool_calls":[{"id":"fixture-receipt","name":"get_output","arguments":{"shell_id":"fixture-shell","timeout":1}}]}'),
    ('fixture-parent',5,4,1800000003,'{"message_id":"fixture-5","role":"tool","tool_call_id":"fixture-receipt","content":"session_id=sess_fixture_child\n","metadata":{}}'),
    ('fixture-parent',6,5,1800000004,'{"message_id":"fixture-6","role":"assistant","content":"Sample finished.","metadata":{"finish_reason":"stop"}}');
INSERT INTO tool_call_state VALUES
    ('fixture-parent','fixture-launch',
     '{"toolCallId":"fixture-launch","title":"Run sample","kind":"execute","content":[],"rawInput":{"command":"fixture-wrapper","workdir":"/fixture","timeout":1,"shell_flavor":"powershell"},"_meta":{"cognition.ai/inferenceToolName":"exec"}}',
     '{"toolCallId":"fixture-launch","status":"completed","_meta":{"cognition.ai/inferenceToolName":"exec"}}'),
    ('fixture-parent','fixture-receipt',
     '{"toolCallId":"fixture-receipt","title":"Get output","rawInput":{"shell_id":"fixture-shell","timeout":1},"_meta":{"cognition.ai/inferenceToolName":"get_output"}}',
     '{"toolCallId":"fixture-receipt","status":"completed","_meta":{"cognition.ai/inferenceToolName":"get_output"}}');
