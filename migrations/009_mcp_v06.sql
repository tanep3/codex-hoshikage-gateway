CREATE TABLE mcp_v06_runs (
 request_id TEXT PRIMARY KEY REFERENCES requests(id),
 profile TEXT NOT NULL CHECK(profile='source-conversation-v3'),
 selection_json TEXT NOT NULL CHECK(json_valid(selection_json)),
 execution_policy_json TEXT CHECK(execution_policy_json IS NULL OR json_valid(execution_policy_json)),
 binding_id TEXT,
 version INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE mcp_v06_views (
 id TEXT PRIMARY KEY,
 interaction_local_id TEXT NOT NULL REFERENCES mcp_interactions(id),
 audience TEXT NOT NULL CHECK(audience IN ('source_conversation','requester')),
 viewer_id TEXT NOT NULL,
 revision INTEGER NOT NULL CHECK(revision>0),
 scope_json TEXT CHECK(scope_json IS NULL OR json_valid(scope_json)),
 scope_fingerprint TEXT,
 policy_binding_id TEXT,
 presentation_id TEXT,
 presentation_fingerprint TEXT,
 content_fingerprint TEXT,
 page_count INTEGER CHECK(page_count BETWEEN 1 AND 64),
 expires_at INTEGER,
 state TEXT NOT NULL CHECK(state IN ('FETCHING','RENDERING','DELIVERING','READY','WAITING','STALE','UNAVAILABLE','CLOSED')),
 active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
 version INTEGER NOT NULL DEFAULT 0,
 boot_id TEXT NOT NULL,
 next_poll_ms INTEGER NOT NULL,
 poll_deadline_ms INTEGER NOT NULL,
 error_code TEXT,
 CHECK(state!='READY' OR (scope_json IS NOT NULL AND scope_fingerprint IS NOT NULL AND policy_binding_id IS NOT NULL AND presentation_id IS NOT NULL AND presentation_fingerprint IS NOT NULL AND content_fingerprint IS NOT NULL AND page_count IS NOT NULL AND expires_at IS NOT NULL))
);
CREATE UNIQUE INDEX mcp_v06_view_active ON mcp_v06_views(interaction_local_id,audience,viewer_id) WHERE active=1;
CREATE INDEX mcp_v06_view_poll ON mcp_v06_views(active,next_poll_ms);
CREATE INDEX mcp_v06_view_expiry ON mcp_v06_views(active,expires_at);
CREATE TABLE mcp_v06_pages (
 view_id TEXT NOT NULL REFERENCES mcp_v06_views(id),
 page_index INTEGER NOT NULL CHECK(page_index BETWEEN 0 AND 63),
 page_token TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('PREPARED','DELIVERING','CONFIRMED','UNKNOWN','FAILED','STALE')),
 part_count INTEGER NOT NULL CHECK(part_count BETWEEN 1 AND 32),
 display_bytes INTEGER NOT NULL CHECK(display_bytes BETWEEN 0 AND 262144),
 PRIMARY KEY(view_id,page_index), UNIQUE(view_id,page_token)
);
CREATE TABLE mcp_v06_parts (
 view_id TEXT NOT NULL,
 page_index INTEGER NOT NULL,
 part_index INTEGER NOT NULL CHECK(part_index BETWEEN 0 AND 31),
 message_id TEXT,
 channel_id TEXT NOT NULL,
 state TEXT NOT NULL CHECK(state IN ('PREPARED','SENDING','CONFIRMED','UNKNOWN','FAILED','DELETED')),
 attempt_id TEXT NOT NULL UNIQUE,
 render_mac TEXT,
 boot_id TEXT NOT NULL,
 PRIMARY KEY(view_id,page_index,part_index),
 FOREIGN KEY(view_id,page_index) REFERENCES mcp_v06_pages(view_id,page_index),
 CHECK(state!='CONFIRMED' OR (message_id IS NOT NULL AND render_mac IS NOT NULL))
);
CREATE TABLE mcp_v06_decisions (
 id TEXT PRIMARY KEY,
 interaction_local_id TEXT NOT NULL REFERENCES mcp_interactions(id),
 active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
 view_id TEXT REFERENCES mcp_v06_views(id),
 discord_interaction_id TEXT NOT NULL UNIQUE,
 action TEXT NOT NULL CHECK(action IN ('once','turn','decline')),
 operation_key TEXT NOT NULL UNIQUE,
 expected_revision INTEGER NOT NULL CHECK(expected_revision>0),
 reply_json TEXT NOT NULL CHECK(json_valid(reply_json)),
 state TEXT NOT NULL CHECK(state IN ('PREPARED','SENDING','UNKNOWN','ACCEPTED','RESOLVED','REJECTED')),
 operation_id TEXT,
 error_code TEXT,
 CHECK(action='decline' OR view_id IS NOT NULL)
);
CREATE UNIQUE INDEX mcp_v06_decision_active ON mcp_v06_decisions(interaction_local_id) WHERE active=1;
