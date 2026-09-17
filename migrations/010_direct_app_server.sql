-- Direct Codex records are separate from legacy Proxy identities. No old
-- response or operation is reinterpreted as a local App Server execution.
CREATE TABLE direct_conversations (
 discord_thread_id TEXT PRIMARY KEY REFERENCES conversations(thread_id),
 workspace_path TEXT NOT NULL UNIQUE,
 workspace_dev INTEGER NOT NULL,
 workspace_ino INTEGER NOT NULL,
 codex_thread_id TEXT UNIQUE,
 model_provider TEXT NOT NULL,
 created_at INTEGER NOT NULL
);
CREATE TABLE direct_dispatches (
 request_id TEXT PRIMARY KEY REFERENCES requests(id),
 intent_id TEXT NOT NULL UNIQUE,
 discord_thread_id TEXT NOT NULL REFERENCES direct_conversations(discord_thread_id),
 send_state TEXT NOT NULL CHECK(send_state IN ('PREPARED','SENDING','ACKED','UNKNOWN','TERMINAL')),
 send_started_at INTEGER,
 codex_thread_id TEXT,
 codex_turn_id TEXT UNIQUE,
 terminal_status TEXT CHECK(terminal_status IN ('completed','failed','interrupted')),
 updated_at INTEGER NOT NULL,
 version INTEGER NOT NULL DEFAULT 0,
 CHECK(send_state='PREPARED' OR send_started_at IS NOT NULL),
 CHECK(codex_turn_id IS NULL OR codex_thread_id IS NOT NULL),
 CHECK(send_state!='TERMINAL' OR terminal_status IS NOT NULL)
);
CREATE INDEX direct_dispatches_active ON direct_dispatches(send_state,updated_at);
CREATE TABLE direct_answers (
 request_id TEXT PRIMARY KEY REFERENCES direct_dispatches(request_id),
 relative_path TEXT NOT NULL UNIQUE,
 sha256 TEXT NOT NULL,
 bytes INTEGER NOT NULL CHECK(bytes>=0),
 stored_at INTEGER NOT NULL
);
CREATE TRIGGER direct_dispatch_no_rewind BEFORE UPDATE ON direct_dispatches
WHEN (OLD.send_started_at IS NOT NULL AND (NEW.send_started_at IS NULL OR NEW.send_state='PREPARED'))
  OR (OLD.send_state IN ('UNKNOWN','TERMINAL') AND NEW.send_state IN ('PREPARED','SENDING','ACKED'))
BEGIN SELECT RAISE(ABORT,'direct dispatch boundary immutable'); END;
