-- App Server requests are scoped to a single Gateway dispatch and can never
-- be replayed to a new child after process loss.
CREATE TABLE direct_interactions (
 id TEXT PRIMARY KEY,
 request_id TEXT NOT NULL REFERENCES direct_dispatches(request_id),
 rpc_id_json TEXT NOT NULL,
 method TEXT NOT NULL,
 thread_id TEXT NOT NULL,
 turn_id TEXT NOT NULL,
 item_id TEXT,
 fingerprint TEXT NOT NULL,
 offered_decisions_json TEXT,
 state TEXT NOT NULL CHECK(state IN ('PENDING','SENDING','SENT','RESOLVED','UNKNOWN','UNAVAILABLE')),
 decision TEXT CHECK(decision IN ('accept','decline','cancel')),
 created_at INTEGER NOT NULL,
 updated_at INTEGER NOT NULL,
 UNIQUE(request_id,rpc_id_json),
 CHECK(state NOT IN ('SENDING','SENT','RESOLVED') OR decision IS NOT NULL)
);
CREATE INDEX direct_interactions_pending ON direct_interactions(request_id,state);
CREATE TRIGGER direct_interaction_no_rewind BEFORE UPDATE ON direct_interactions
WHEN (OLD.state IN ('SENDING','SENT','RESOLVED','UNKNOWN','UNAVAILABLE') AND NEW.state='PENDING')
  OR (OLD.state IN ('RESOLVED','UNKNOWN','UNAVAILABLE') AND NEW.state IN ('SENDING','SENT'))
BEGIN SELECT RAISE(ABORT,'direct interaction boundary immutable'); END;
