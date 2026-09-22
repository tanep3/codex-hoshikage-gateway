ALTER TABLE projects ADD COLUMN default_reasoning_effort TEXT NOT NULL DEFAULT '';

ALTER TABLE conversations ADD COLUMN latest_effort_sequence INTEGER NOT NULL DEFAULT 0;
ALTER TABLE conversations ADD COLUMN selected_reasoning_effort TEXT NOT NULL DEFAULT '';
ALTER TABLE conversations ADD COLUMN effective_reasoning_effort TEXT;
ALTER TABLE conversations ADD COLUMN effort_revision INTEGER NOT NULL DEFAULT 0;

ALTER TABLE requests ADD COLUMN reasoning_effort TEXT;
ALTER TABLE requests ADD COLUMN reasoning_effort_revision INTEGER;

ALTER TABLE operations ADD COLUMN desired_reasoning_effort TEXT;
