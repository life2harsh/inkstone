-- Inkstone v0.1 System Architecture Migration
-- Adds atomic sequence transaction, unified update_id, snapshot base_seq, direct WS control messages
-- Also drops legacy client_update_id and snapshot_version columns, adds aad_version to snapshots.

-- ── 1. Rename client_update_id → update_id in doc_updates ────────────────

ALTER TABLE doc_updates ADD COLUMN update_id UUID;

UPDATE doc_updates SET update_id = client_update_id WHERE update_id IS NULL;

ALTER TABLE doc_updates ALTER COLUMN update_id SET NOT NULL;

-- Drop old unique constraint on (doc_id, client_update_id)
ALTER TABLE doc_updates DROP CONSTRAINT IF EXISTS doc_updates_doc_id_client_update_id_key;

-- Add new unique constraint on (doc_id, update_id)
ALTER TABLE doc_updates ADD UNIQUE (doc_id, update_id);

-- Index for idempotent lookup by update_id
CREATE INDEX IF NOT EXISTS idx_doc_updates_update_id ON doc_updates(doc_id, update_id);

-- Drop legacy column
ALTER TABLE doc_updates DROP COLUMN IF EXISTS client_update_id;


-- ── 2. Atomic sequence allocation (FOR UPDATE lock on counter) ────────────

-- The existing doc_update_counters table is already used for atomic seq.
-- The fix is in the application code: we now use BEGIN + FOR UPDATE + INSERT
-- in a single transaction. No schema change needed here.


-- ── 3. Replace snapshot_version with real base_seq ───────────────────────

-- Delete legacy snapshots that used snapshot_version (different semantics)
DELETE FROM doc_snapshots;

ALTER TABLE doc_snapshots ADD COLUMN base_seq BIGINT;

ALTER TABLE doc_snapshots ALTER COLUMN base_seq SET NOT NULL;

-- Drop old unique constraint
ALTER TABLE doc_snapshots DROP CONSTRAINT IF EXISTS doc_snapshots_doc_id_snapshot_version_key;

ALTER TABLE doc_snapshots ADD UNIQUE (doc_id, base_seq);

CREATE INDEX IF NOT EXISTS idx_doc_snapshots_base_seq ON doc_snapshots(doc_id, base_seq DESC);

-- Drop legacy column
ALTER TABLE doc_snapshots DROP COLUMN IF EXISTS snapshot_version;


-- ── 4. Add aad_version to doc_snapshots ───────────────────────────────────

ALTER TABLE doc_snapshots ADD COLUMN IF NOT EXISTS aad_version INTEGER NOT NULL DEFAULT 1;
