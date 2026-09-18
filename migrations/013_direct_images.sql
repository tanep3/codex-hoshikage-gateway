-- A complete inventory is recorded separately from each image's immutable
-- content. Failed capture cannot turn a finished Codex Turn into a new run.
CREATE TABLE direct_image_inventories (
 request_id TEXT PRIMARY KEY REFERENCES direct_dispatches(request_id),
 state TEXT NOT NULL CHECK(state IN ('COMPLETE','UNKNOWN')),
 error_code TEXT,
 updated_at INTEGER NOT NULL
);
CREATE TABLE direct_generated_images (
 request_id TEXT NOT NULL REFERENCES direct_image_inventories(request_id),
 item_id TEXT NOT NULL,
 ordinal INTEGER NOT NULL CHECK(ordinal>=0),
 state TEXT NOT NULL CHECK(state IN ('READY','FAILED','UNKNOWN')),
 relative_path TEXT,
 sha256 TEXT,
 bytes INTEGER,
 updated_at INTEGER NOT NULL,
 PRIMARY KEY(request_id,item_id),
 UNIQUE(request_id,ordinal),
 CHECK((state='READY' AND relative_path IS NOT NULL AND sha256 IS NOT NULL AND bytes>=0)
    OR (state!='READY' AND relative_path IS NULL AND sha256 IS NULL AND bytes IS NULL))
);
