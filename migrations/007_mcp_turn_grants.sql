CREATE TABLE mcp_run_context (
 request_id TEXT PRIMARY KEY REFERENCES requests(id),
 principal_id TEXT NOT NULL, channel_id TEXT NOT NULL, run_id TEXT NOT NULL
);
CREATE TABLE mcp_detail_views (
 id TEXT PRIMARY KEY, interaction_local_id TEXT NOT NULL REFERENCES mcp_interactions(id),
 viewer_id TEXT NOT NULL, revision INTEGER NOT NULL, fingerprint TEXT NOT NULL,
 scope_digest TEXT NOT NULL, expires_at INTEGER NOT NULL
);
CREATE TABLE mcp_grant_records (
 id TEXT PRIMARY KEY, request_id TEXT NOT NULL REFERENCES requests(id), grant_id TEXT NOT NULL,
 scope_json TEXT NOT NULL, state TEXT NOT NULL, expires_at TEXT NOT NULL,
 application_count INTEGER NOT NULL, UNIQUE(request_id,grant_id)
);
CREATE TABLE mcp_grant_revokes (
 grant_local_id TEXT PRIMARY KEY REFERENCES mcp_grant_records(id),
 operation_key TEXT UNIQUE NOT NULL, state TEXT NOT NULL, in_flight_count INTEGER
);
ALTER TABLE mcp_interactions ADD COLUMN grant_scope TEXT;
