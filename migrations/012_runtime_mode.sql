-- An explicit execution-mode fence prevents an old Proxy daemon from opening
-- a database after the offline direct-Codex cutover.
CREATE TABLE runtime_mode (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 mode TEXT NOT NULL CHECK(mode IN ('proxy','direct')),
 changed_at INTEGER NOT NULL,
 backup_id TEXT
);
INSERT INTO runtime_mode(singleton,mode,changed_at) VALUES(1,'proxy',0);
