CREATE TABLE mcp_inline_runs (
 request_id TEXT PRIMARY KEY REFERENCES requests(id)
);
CREATE TABLE mcp_inline_views (
 id TEXT PRIMARY KEY,
 interaction_local_id TEXT NOT NULL REFERENCES mcp_interactions(id),
 presentation_id TEXT NOT NULL,
 fingerprint TEXT NOT NULL,
 revision INTEGER NOT NULL,
 snapshot_digest TEXT NOT NULL,
 expires_at INTEGER NOT NULL,
 active INTEGER NOT NULL DEFAULT 1,
 UNIQUE(interaction_local_id,presentation_id,fingerprint)
);
CREATE UNIQUE INDEX mcp_inline_current ON mcp_inline_views(interaction_local_id) WHERE active=1;
