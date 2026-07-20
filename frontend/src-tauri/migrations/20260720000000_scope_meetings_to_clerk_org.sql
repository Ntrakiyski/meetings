-- Existing meetings predate organization ownership and cannot be assigned safely.
-- Delete their dependent content explicitly because SQLite foreign-key enforcement
-- is not guaranteed on legacy connections. Application settings remain untouched.
DELETE FROM meeting_notes;
DELETE FROM summary_processes;
DELETE FROM transcript_chunks;
DELETE FROM transcripts;
DELETE FROM meetings;

ALTER TABLE meetings ADD COLUMN clerk_org_id TEXT NOT NULL DEFAULT '' CHECK (length(clerk_org_id) > 0);
ALTER TABLE meetings ADD COLUMN created_by TEXT NOT NULL DEFAULT '' CHECK (length(created_by) > 0);
ALTER TABLE meetings ADD COLUMN sync_state TEXT NOT NULL DEFAULT 'local' CHECK (sync_state IN ('local', 'live', 'pending_final', 'final'));
ALTER TABLE meetings ADD COLUMN sync_revision INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS meetings_clerk_org_created_idx
  ON meetings (clerk_org_id, created_at DESC);
