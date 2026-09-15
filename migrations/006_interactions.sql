CREATE TABLE mcp_interactions (
 id TEXT PRIMARY KEY,
 interaction_id TEXT NOT NULL,
 request_id TEXT NOT NULL REFERENCES requests(id),
 response_id TEXT NOT NULL,
 conversation_id TEXT NOT NULL,
 workspace_id TEXT NOT NULL,
 instance_id TEXT NOT NULL,
 generation TEXT NOT NULL,
 base_url TEXT NOT NULL,
 revision INTEGER NOT NULL,
 request_digest TEXT NOT NULL,
 expires_at INTEGER NOT NULL,
 state TEXT NOT NULL,
 closed INTEGER NOT NULL DEFAULT 0,
 operation_key TEXT UNIQUE,
 operation_state TEXT,
 reply_digest TEXT,
 action TEXT,
 UNIQUE(instance_id,generation,interaction_id)
);
CREATE INDEX mcp_interactions_watch ON mcp_interactions(closed,request_id);
ALTER TABLE requests ADD COLUMN interaction_scan_done INTEGER NOT NULL DEFAULT 0;
