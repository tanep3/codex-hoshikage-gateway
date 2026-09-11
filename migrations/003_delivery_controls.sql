CREATE TABLE selection_menus(id TEXT PRIMARY KEY,thread_id TEXT NOT NULL REFERENCES conversations(thread_id),kind TEXT NOT NULL,scope TEXT NOT NULL,binding_json TEXT NOT NULL,items_json TEXT NOT NULL,next_cursor TEXT,expires_at INTEGER NOT NULL);
ALTER TABLE resource_deliveries ADD COLUMN next_attempt_at INTEGER NOT NULL DEFAULT 0;
ALTER TABLE resource_deliveries ADD COLUMN attempts INTEGER NOT NULL DEFAULT 0;
