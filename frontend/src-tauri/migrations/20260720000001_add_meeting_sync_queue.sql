CREATE TABLE meeting_sync_queue (
    clerk_org_id TEXT NOT NULL,
    created_by TEXT NOT NULL,
    external_id TEXT NOT NULL,
    revision INTEGER NOT NULL,
    payload TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (clerk_org_id, created_by, external_id)
);

CREATE INDEX idx_meeting_sync_queue_identity
    ON meeting_sync_queue (clerk_org_id, created_by, revision);
