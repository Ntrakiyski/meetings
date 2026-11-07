-- Expand ownership without guessing or deleting legacy recordings.
ALTER TABLE meetings ADD COLUMN connections_issuer TEXT;
ALTER TABLE meetings ADD COLUMN organization_id TEXT;
ALTER TABLE meetings ADD COLUMN workspace_id TEXT;
ALTER TABLE meetings ADD COLUMN legacy_created_by TEXT;
UPDATE meetings SET legacy_created_by=created_by;
ALTER TABLE meetings ADD COLUMN ownership_state TEXT NOT NULL DEFAULT 'legacy_unverified'
  CHECK (ownership_state IN ('legacy_unverified','verified') AND
    (ownership_state != 'verified' OR
      (coalesce(length(connections_issuer),0)>0 AND coalesce(length(organization_id),0)>0 AND coalesce(length(workspace_id),0)>0)));
ALTER TABLE meetings ADD COLUMN ownership_key TEXT GENERATED ALWAYS AS
  (CASE WHEN ownership_state='verified' THEN json_array(connections_issuer,organization_id,workspace_id) END) VIRTUAL;
CREATE INDEX meetings_connections_scope_created_idx ON meetings(ownership_key,created_at DESC);
CREATE TRIGGER meetings_verified_ownership_immutable
BEFORE UPDATE OF connections_issuer,organization_id,workspace_id,created_by,clerk_org_id,legacy_created_by,ownership_state ON meetings
WHEN OLD.ownership_state='verified' AND
  (NEW.connections_issuer IS NOT OLD.connections_issuer OR NEW.organization_id IS NOT OLD.organization_id OR
   NEW.workspace_id IS NOT OLD.workspace_id OR NEW.created_by IS NOT OLD.created_by OR
   NEW.clerk_org_id IS NOT OLD.clerk_org_id OR NEW.legacy_created_by IS NOT OLD.legacy_created_by OR NEW.ownership_state IS NOT OLD.ownership_state)
BEGIN SELECT RAISE(ABORT,'verified meeting ownership is immutable'); END;

-- Original organization/user keys and payloads remain available for verified recovery.
ALTER TABLE meeting_sync_queue RENAME TO meeting_sync_legacy_queue;
CREATE TABLE meeting_sync_queue (
  connections_issuer TEXT NOT NULL CHECK(length(connections_issuer)>0),
  organization_id TEXT NOT NULL CHECK(length(organization_id)>0),
  workspace_id TEXT NOT NULL CHECK(length(workspace_id)>0),
  created_by TEXT NOT NULL CHECK(length(created_by)>0),
  external_id TEXT NOT NULL CHECK(length(external_id)>0),
  revision INTEGER NOT NULL CHECK(revision>0),
  payload TEXT NOT NULL,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  ownership_key TEXT GENERATED ALWAYS AS(json_array(connections_issuer,organization_id,workspace_id)) VIRTUAL,
  PRIMARY KEY(connections_issuer,workspace_id,created_by,external_id)
);
CREATE INDEX idx_meeting_sync_queue_scope ON meeting_sync_queue(ownership_key,created_by,revision);
CREATE TRIGGER meeting_sync_queue_ownership_immutable
BEFORE UPDATE OF connections_issuer,organization_id,workspace_id,created_by,external_id ON meeting_sync_queue
WHEN NEW.connections_issuer IS NOT OLD.connections_issuer OR NEW.organization_id IS NOT OLD.organization_id OR
  NEW.workspace_id IS NOT OLD.workspace_id OR NEW.created_by IS NOT OLD.created_by OR NEW.external_id IS NOT OLD.external_id
BEGIN SELECT RAISE(ABORT,'queued meeting ownership is immutable'); END;
