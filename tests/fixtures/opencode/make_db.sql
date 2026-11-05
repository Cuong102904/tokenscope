-- Synthetic OpenCode DB (minimal subset of the real schema). All values are made up.
CREATE TABLE session(id TEXT PRIMARY KEY, project_id TEXT, directory TEXT, title TEXT, time_updated INTEGER);
CREATE TABLE message(id TEXT PRIMARY KEY, session_id TEXT, time_created INTEGER, time_updated INTEGER, data TEXT);

INSERT INTO session VALUES('ses_1','p1','/home/u/proj-a','Fix the parser',1700000100000);
INSERT INTO session VALUES('ses_2','p1','/home/u/proj-b','Write docs',1700000200000);

-- user message: must be ignored
INSERT INTO message VALUES('msg_u1','ses_1',1700000000000,1700000000000,
 '{"role":"user","time":{"created":1700000000000}}');
-- assistant: input 676, output 60, reasoning 22, cache read 73216
INSERT INTO message VALUES('msg_a1','ses_1',1700000001000,1700000050000,
 '{"role":"assistant","modelID":"gpt-test","providerID":"prov","cost":0,"tokens":{"total":73974,"input":676,"output":60,"reasoning":22,"cache":{"write":0,"read":73216}},"time":{"created":1700000001000,"completed":1700000049000}}');
-- assistant in another session, cache write present
INSERT INTO message VALUES('msg_a2','ses_2',1700000002000,1700000060000,
 '{"role":"assistant","modelID":"claude-test","providerID":"prov","cost":0,"tokens":{"total":1150,"input":100,"output":40,"reasoning":0,"cache":{"write":10,"read":1000}},"time":{"created":1700000002000,"completed":1700000059000}}');
-- assistant without tokens (aborted): must be skipped
INSERT INTO message VALUES('msg_a3','ses_2',1700000003000,1700000070000,
 '{"role":"assistant","modelID":"claude-test","providerID":"prov","time":{"created":1700000003000}}');
