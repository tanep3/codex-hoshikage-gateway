CREATE TABLE direct_artifacts (
    id TEXT PRIMARY KEY,
    request_id TEXT REFERENCES requests(id),
    discord_thread_id TEXT NOT NULL REFERENCES conversations(thread_id),
    call_id TEXT NOT NULL,
    source_path TEXT NOT NULL,
    display_name TEXT NOT NULL,
    relative_path TEXT NOT NULL UNIQUE,
    sha256 TEXT NOT NULL,
    bytes INTEGER NOT NULL CHECK (bytes >= 0),
    state TEXT NOT NULL CHECK (state IN ('READY')),
    created_at INTEGER NOT NULL,
    UNIQUE (discord_thread_id, call_id)
);
CREATE INDEX direct_artifacts_thread ON direct_artifacts(discord_thread_id, created_at, id);
