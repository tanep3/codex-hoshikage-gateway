ALTER TABLE resource_deliveries ADD COLUMN retry_of TEXT REFERENCES resource_deliveries(id);
ALTER TABLE resource_deliveries ADD COLUMN shared_workspace TEXT;
CREATE TABLE recovery_reviews(id TEXT PRIMARY KEY,snapshot_json TEXT NOT NULL,expires_at INTEGER NOT NULL);
