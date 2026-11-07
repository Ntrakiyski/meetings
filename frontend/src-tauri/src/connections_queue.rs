use crate::connections_identity::OperationIdentity;
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::future::Future;
use std::sync::atomic::{AtomicI64, Ordering};

static LAST_REVISION: AtomicI64 = AtomicI64::new(0);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub id: String,
    pub text: String,
    pub timestamp: String,
    #[serde(default, alias = "source", skip_serializing_if = "Option::is_none")]
    pub speaker: Option<String>,
    // NEW: Recording-relative timestamps for playback synchronization
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_start_time: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_end_time: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MeetingPayload {
    state: String,
    revision: i64,
    title: String,
    transcript: String,
    transcript_segments: Vec<TranscriptSegment>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_transcript: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_transcript_segments: Option<Vec<TranscriptSegment>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ended_at: Option<String>,
}

pub async fn queue_meeting(
    pool: &SqlitePool,
    identity: &OperationIdentity,
    external_id: &str,
    title: &str,
    segments: &[TranscriptSegment],
    raw_segments: Option<&[TranscriptSegment]>,
    summary: Option<&str>,
    state: &str,
    started_at: Option<&str>,
    ended_at: Option<&str>,
) -> Result<()> {
    if state == "final" {
        validate_saved_meeting_identity(pool, external_id, identity).await?;
    }
    let Some(payload) = meeting_payload(
        title,
        segments,
        raw_segments,
        summary,
        state,
        started_at,
        ended_at,
    ) else {
        return Ok(());
    };
    queue_payload(pool, identity, external_id, &payload).await
}

pub(crate) fn meeting_payload(
    title: &str,
    segments: &[TranscriptSegment],
    raw_segments: Option<&[TranscriptSegment]>,
    summary: Option<&str>,
    state: &str,
    started_at: Option<&str>,
    ended_at: Option<&str>,
) -> Option<MeetingPayload> {
    let transcript = segments
        .iter()
        .map(|segment| segment.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if transcript.is_empty() && state != "final" {
        return None;
    }
    let raw_transcript = raw_segments.map(|segments| {
        segments
            .iter()
            .map(|segment| segment.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    });

    Some(MeetingPayload {
        state: state.to_string(),
        revision: next_revision(),
        title: title.to_string(),
        transcript,
        transcript_segments: segments.to_vec(),
        raw_transcript,
        raw_transcript_segments: raw_segments.map(<[TranscriptSegment]>::to_vec),
        summary: summary.map(str::to_string),
        started_at: started_at.map(str::to_string),
        ended_at: ended_at.map(str::to_string),
    })
}

fn next_revision() -> i64 {
    let now = chrono::Utc::now().timestamp_millis().max(1);
    let mut previous = LAST_REVISION.load(Ordering::Relaxed);
    loop {
        let next = now.max(previous.saturating_add(1));
        match LAST_REVISION.compare_exchange_weak(
            previous,
            next,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return next,
            Err(current) => previous = current,
        }
    }
}

async fn queue_payload(
    pool: &SqlitePool,
    identity: &OperationIdentity,
    external_id: &str,
    payload: &MeetingPayload,
) -> Result<()> {
    let serialized = serde_json::to_string(payload)?;
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "INSERT INTO meeting_sync_queue (connections_issuer, organization_id, workspace_id, created_by, external_id, revision, payload, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP)
         ON CONFLICT (connections_issuer, workspace_id, created_by, external_id) DO UPDATE SET
           revision = excluded.revision,
           payload = excluded.payload,
           updated_at = CURRENT_TIMESTAMP
         WHERE meeting_sync_queue.organization_id = excluded.organization_id
           AND excluded.revision > meeting_sync_queue.revision",
    )
    .bind(&identity.issuer)
    .bind(&identity.organization_id)
    .bind(&identity.workspace_id)
    .bind(&identity.user_id)
    .bind(external_id)
    .bind(payload.revision)
    .bind(serialized)
    .execute(&mut *transaction)
    .await?;
    if payload.state == "final" {
        sqlx::query(
            "UPDATE meetings SET sync_state = 'final', sync_revision = ?
             WHERE id = ? AND ownership_key = ? AND created_by = ?",
        )
        .bind(payload.revision)
        .bind(external_id)
        .bind(&identity.scope_key())
        .bind(&identity.user_id)
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(())
}

async fn validate_saved_meeting_identity(
    pool: &SqlitePool,
    meeting_id: &str,
    identity: &OperationIdentity,
) -> Result<()> {
    let stored = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT connections_issuer, organization_id, workspace_id, created_by FROM meetings WHERE id = ? AND ownership_state='verified'",
    )
    .bind(meeting_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| anyhow!("The final meeting must be saved locally before queueing."))?;
    if stored
        != (
            identity.issuer.clone(),
            identity.organization_id.clone(),
            identity.workspace_id.clone(),
            identity.user_id.clone(),
        )
    {
        return Err(anyhow!(
            "The saved meeting belongs to a different Connections user, issuer or workspace."
        ));
    }
    Ok(())
}

pub async fn recover_pending_finals(
    pool: &SqlitePool,
    identity: &OperationIdentity,
) -> Result<usize> {
    let meetings = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT id, title, created_at, updated_at FROM meetings
         WHERE ownership_key = ? AND created_by = ? AND sync_state = 'pending_final'
         ORDER BY created_at ASC",
    )
    .bind(&identity.scope_key())
    .bind(&identity.user_id)
    .fetch_all(pool)
    .await?;

    for (meeting_id, title, created_at, updated_at) in &meetings {
        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                Option<String>,
                String,
                Option<String>,
                Option<f64>,
                Option<f64>,
                Option<f64>,
            ),
        >(
            "SELECT id, transcript, enhanced_transcript, timestamp, speaker,
                    audio_start_time, audio_end_time, duration
             FROM transcripts WHERE meeting_id = ? ORDER BY timestamp ASC",
        )
        .bind(meeting_id)
        .fetch_all(pool)
        .await?;
        let segments = rows
            .iter()
            .map(|row| TranscriptSegment {
                id: row.0.clone(),
                text: row.2.clone().unwrap_or_else(|| row.1.clone()),
                timestamp: row.3.clone(),
                speaker: row.4.clone(),
                audio_start_time: row.5,
                audio_end_time: row.6,
                duration: row.7,
            })
            .collect::<Vec<_>>();
        let raw_segments = rows
            .into_iter()
            .map(|row| TranscriptSegment {
                id: row.0,
                text: row.1,
                timestamp: row.3,
                speaker: row.4,
                audio_start_time: row.5,
                audio_end_time: row.6,
                duration: row.7,
            })
            .collect::<Vec<_>>();
        queue_meeting(
            pool,
            identity,
            meeting_id,
            title,
            &segments,
            Some(&raw_segments),
            None,
            "final",
            Some(created_at),
            Some(updated_at),
        )
        .await?;
    }
    Ok(meetings.len())
}

pub(crate) async fn publish_payload<F, Fut>(
    pool: &SqlitePool,
    identity: &OperationIdentity,
    external_id: &str,
    payload: &MeetingPayload,
    deliver: F,
) -> Result<()>
where
    F: FnOnce(String, MeetingPayload) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    queue_payload(pool, identity, external_id, payload).await?;
    deliver(external_id.to_string(), payload.clone()).await?;
    delete_payload(pool, identity, external_id, payload).await
}

async fn delete_payload(
    pool: &SqlitePool,
    identity: &OperationIdentity,
    external_id: &str,
    payload: &MeetingPayload,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM meeting_sync_queue
         WHERE ownership_key = ? AND created_by = ? AND external_id = ? AND revision = ?",
    )
    .bind(&identity.scope_key())
    .bind(&identity.user_id)
    .bind(external_id)
    .bind(payload.revision)
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn drain_queued_payloads<F, Fut>(
    pool: &SqlitePool,
    identity: &OperationIdentity,
    mut deliver: F,
) -> Result<usize>
where
    F: FnMut(String, MeetingPayload) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let mut delivered = 0;
    loop {
        let rows = sqlx::query_as::<_, (String, String)>(
            "SELECT external_id, payload FROM meeting_sync_queue
             WHERE ownership_key = ? AND created_by = ? ORDER BY revision ASC LIMIT 20",
        )
        .bind(&identity.scope_key())
        .bind(&identity.user_id)
        .fetch_all(pool)
        .await?;
        if rows.is_empty() {
            return Ok(delivered);
        }
        for (external_id, serialized) in rows {
            let payload: MeetingPayload = serde_json::from_str(&serialized)?;
            deliver(external_id.clone(), payload.clone()).await?;
            delete_payload(pool, identity, &external_id, &payload).await?;
            delivered += 1;
        }
    }
}

pub(crate) async fn stored_meeting_identity(
    pool: &SqlitePool,
    meeting_id: &str,
    current_org: &str,
) -> Result<OperationIdentity> {
    let (issuer, organization_id, workspace_id, created_by) = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT connections_issuer, organization_id, workspace_id, created_by FROM meetings WHERE id = ? AND ownership_key = ?",
    )
    .bind(meeting_id)
    .bind(current_org)
    .fetch_one(pool)
    .await?;
    Ok(OperationIdentity::new(
        issuer,
        organization_id,
        workspace_id,
        created_by,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn queue_pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE meeting_sync_queue (clerk_org_id TEXT NOT NULL, created_by TEXT NOT NULL, external_id TEXT NOT NULL, revision INTEGER NOT NULL, payload TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY (clerk_org_id, created_by, external_id))").execute(&pool).await.unwrap();
        sqlx::query("CREATE TABLE meetings (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL, clerk_org_id TEXT NOT NULL, created_by TEXT NOT NULL, sync_state TEXT NOT NULL, sync_revision INTEGER NOT NULL DEFAULT 0)").execute(&pool).await.unwrap();
        sqlx::query("CREATE TABLE transcripts (id TEXT PRIMARY KEY, meeting_id TEXT NOT NULL, transcript TEXT NOT NULL, enhanced_transcript TEXT, timestamp TEXT NOT NULL, speaker TEXT, audio_start_time REAL, audio_end_time REAL, duration REAL)").execute(&pool).await.unwrap();
        expand_test_ownership(&pool).await;
        pool
    }

    fn identity(user: &str) -> OperationIdentity {
        OperationIdentity::new(
            "https://connections.fractals-solutions.com/api/identity",
            "company-a",
            "A1",
            user,
        )
    }

    fn payload(revision: i64) -> MeetingPayload {
        MeetingPayload {
            state: "final".into(),
            revision,
            title: "Test".into(),
            transcript: "hello".into(),
            transcript_segments: vec![],
            raw_transcript: None,
            raw_transcript_segments: None,
            summary: None,
            started_at: None,
            ended_at: None,
        }
    }

    async fn pending(pool: &SqlitePool, owner: &OperationIdentity, id: &str, with_text: bool) {
        sqlx::query("INSERT INTO meetings (id,title,created_at,updated_at,clerk_org_id,created_by,connections_issuer,organization_id,workspace_id,ownership_state,sync_state) VALUES (?, 'Pending', '2026-07-20', '2026-07-20', 'legacy-snapshot-unused', ?, ?, ?, ?, 'verified', 'pending_final')").bind(id).bind(&owner.user_id).bind(&owner.issuer).bind(&owner.organization_id).bind(&owner.workspace_id).execute(pool).await.unwrap();
        if with_text {
            sqlx::query("INSERT INTO transcripts (id,meeting_id,transcript,timestamp) VALUES (?, ?, 'Recovered', '00:01')").bind(format!("segment-{id}")).bind(id).execute(pool).await.unwrap();
        }
    }

    #[tokio::test]
    async fn restart_keeps_original_workspace_and_issuer_until_owner_retries() {
        let path = std::env::temp_dir().join(format!(
            "meetings-queue-{}-{}.sqlite",
            std::process::id(),
            next_revision()
        ));
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TABLE meetings (id TEXT PRIMARY KEY,created_at TEXT,clerk_org_id TEXT,created_by TEXT,sync_state TEXT DEFAULT 'local',sync_revision INTEGER DEFAULT 0); CREATE TABLE meeting_sync_queue (clerk_org_id TEXT,created_by TEXT,external_id TEXT,revision INTEGER,payload TEXT,updated_at TEXT,PRIMARY KEY(clerk_org_id,created_by,external_id)); INSERT INTO meetings(id,created_at,clerk_org_id,created_by) VALUES('legacy','2026','clerk-a','user-a'); INSERT INTO meeting_sync_queue VALUES('clerk-a','user-a','shared',1,'original-a','2026'),('clerk-b','user-a','shared',2,'original-b','2026');").execute(&pool).await.unwrap();
        let migrator = sqlx::migrate::Migrator {
            migrations: std::borrow::Cow::Owned(vec![sqlx::migrate::Migration::new(
                20261107230000,
                "workspace ownership".into(),
                sqlx::migrate::MigrationType::Simple,
                include_str!("../migrations/20261107230000_connections_workspace_identity.sql")
                    .into(),
                false,
            )]),
            ignore_missing: false,
            locking: true,
            no_tx: false,
        };
        migrator.run(&pool).await.unwrap();
        let owner = identity("user-a");
        let mut recording = payload(123);
        recording.transcript = "Offline recording preserved across restart".into();
        queue_payload(&pool, &owner, "offline", &recording)
            .await
            .unwrap();
        pool.close().await;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        migrator.run(&pool).await.unwrap();
        let legacy: Vec<(String, String)> = sqlx::query_as(
            "SELECT clerk_org_id,payload FROM meeting_sync_legacy_queue ORDER BY clerk_org_id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            legacy,
            vec![
                ("clerk-a".into(), "original-a".into()),
                ("clerk-b".into(), "original-b".into())
            ]
        );
        assert_eq!(
            sqlx::query_scalar::<_, Option<String>>(
                "SELECT ownership_key FROM meetings WHERE id='legacy'"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            None
        );
        for other in [
            OperationIdentity::new(&owner.issuer, "company-a", "A2", "user-a"),
            OperationIdentity::new(&owner.issuer, "company-b", "B1", "user-a"),
            OperationIdentity::new(
                "https://other.example/api/identity",
                "company-a",
                "A1",
                "user-a",
            ),
            identity("user-b"),
        ] {
            assert_eq!(
                drain_queued_payloads(&pool, &other, |_, _| async {
                    panic!("wrong owner must never deliver")
                })
                .await
                .unwrap(),
                0
            );
        }
        assert!(drain_queued_payloads(&pool, &owner, |_, _| async {
            Err(anyhow!("still offline"))
        })
        .await
        .is_err());
        pool.close().await;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(sqlx::sqlite::SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        assert_eq!(
            drain_queued_payloads(&pool, &owner, |external_id, recording| async move {
                assert_eq!(external_id, "offline");
                assert_eq!(recording.revision, 123);
                assert_eq!(
                    recording.transcript,
                    "Offline recording preserved across restart"
                );
                Ok(())
            })
            .await
            .unwrap(),
            1
        );
        pool.close().await;
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn divergent_company_cannot_replace_existing_workspace_payload() {
        let pool = queue_pool().await;
        let owner = identity("user-a");
        queue_payload(&pool, &owner, "shared", &payload(1))
            .await
            .unwrap();
        let divergent = OperationIdentity::new(&owner.issuer, "company-b", "A1", "user-a");
        queue_payload(&pool, &divergent, "shared", &payload(2))
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_as::<_, (String, i64)>(
                "SELECT organization_id,revision FROM meeting_sync_queue"
            )
            .fetch_one(&pool)
            .await
            .unwrap(),
            ("company-a".into(), 1)
        );
    }

    #[tokio::test]
    async fn final_queue_keeps_captured_identity_and_failed_delivery_retains_it() {
        let pool = queue_pool().await;
        queue_payload(&pool, &identity("user-a"), "meeting-1", &payload(1))
            .await
            .unwrap();
        let error = drain_queued_payloads(&pool, &identity("user-a"), |_, _| async {
            Err(anyhow!("offline"))
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("offline"));
        let row = sqlx::query_as::<_, (String, String)>(
            "SELECT workspace_id, created_by FROM meeting_sync_queue",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row, ("A1".into(), "user-a".into()));
    }

    #[tokio::test]
    async fn empty_final_is_durably_queued() {
        let pool = queue_pool().await;
        let owner = identity("user-a");
        pending(&pool, &owner, "empty", false).await;
        queue_meeting(
            &pool,
            &owner,
            "empty",
            "Silent",
            &[],
            None,
            None,
            "final",
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM meeting_sync_queue")
                .fetch_one(&pool)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn creators_cannot_replace_each_others_queue_rows() {
        let pool = queue_pool().await;
        queue_payload(&pool, &identity("user-a"), "shared", &payload(1))
            .await
            .unwrap();
        queue_payload(&pool, &identity("user-b"), "shared", &payload(2))
            .await
            .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM meeting_sync_queue")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn direct_publish_deletes_only_delivered_revision() {
        let pool = queue_pool().await;
        let owner = identity("user-a");
        publish_payload(&pool, &owner, "meeting-1", &payload(1), |_, _| {
            let pool = pool.clone();
            let owner = owner.clone();
            async move { queue_payload(&pool, &owner, "meeting-1", &payload(2)).await }
        })
        .await
        .unwrap();
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT revision FROM meeting_sync_queue")
                .fetch_one(&pool)
                .await
                .unwrap(),
            2
        );
    }

    #[tokio::test]
    async fn pending_final_is_requeued_idempotently_for_owner() {
        let pool = queue_pool().await;
        let owner = identity("user-a");
        pending(&pool, &owner, "meeting-1", true).await;
        assert_eq!(recover_pending_finals(&pool, &owner).await.unwrap(), 1);
        assert_eq!(recover_pending_finals(&pool, &owner).await.unwrap(), 0);
        let row = sqlx::query_as::<_, (String, i64, i64)>("SELECT m.sync_state,m.sync_revision,q.revision FROM meetings m JOIN meeting_sync_queue q ON q.external_id=m.id").fetch_one(&pool).await.unwrap();
        assert_eq!(row.0, "final");
        assert_eq!(row.1, row.2);
    }

    #[tokio::test]
    async fn final_queue_rejects_non_owner_before_write() {
        let pool = queue_pool().await;
        pending(&pool, &identity("user-a"), "meeting-1", false).await;
        assert!(queue_meeting(
            &pool,
            &identity("user-b"),
            "meeting-1",
            "Test",
            &[],
            None,
            None,
            "final",
            None,
            None
        )
        .await
        .is_err());
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM meeting_sync_queue")
                .fetch_one(&pool)
                .await
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn retry_drains_all_batches_for_only_one_identity() {
        let pool = queue_pool().await;
        for revision in 1..=25 {
            queue_payload(
                &pool,
                &identity("user-a"),
                &format!("m-{revision}"),
                &payload(revision),
            )
            .await
            .unwrap();
        }
        queue_payload(&pool, &identity("user-b"), "other", &payload(1))
            .await
            .unwrap();
        assert_eq!(
            drain_queued_payloads(&pool, &identity("user-a"), |_, _| async { Ok(()) })
                .await
                .unwrap(),
            25
        );
        assert_eq!(
            sqlx::query_scalar::<_, String>("SELECT created_by FROM meeting_sync_queue")
                .fetch_one(&pool)
                .await
                .unwrap(),
            "user-b"
        );
    }

    #[tokio::test]
    async fn stored_identity_includes_original_creator() {
        let pool = queue_pool().await;
        pending(&pool, &identity("original"), "meeting-1", false).await;
        assert_eq!(
            stored_meeting_identity(&pool, "meeting-1", &identity("original").scope_key())
                .await
                .unwrap(),
            identity("original")
        );
    }

    #[test]
    fn payload_preserves_speaker_and_summary() {
        let segments = [TranscriptSegment {
            id: "segment-1".to_string(),
            text: "Здравейте".to_string(),
            timestamp: "00:01".to_string(),
            speaker: Some("Microphone".to_string()),
            audio_start_time: Some(1.0),
            audio_end_time: Some(2.0),
            duration: Some(1.0),
        }];
        let payload = serde_json::to_value(MeetingPayload {
            state: "final".to_string(),
            revision: 2,
            title: "Test".to_string(),
            transcript: "Здравейте".to_string(),
            transcript_segments: segments.to_vec(),
            raw_transcript: Some("здравейте".to_string()),
            raw_transcript_segments: Some(segments.to_vec()),
            summary: Some("Резюме".to_string()),
            started_at: None,
            ended_at: None,
        })
        .unwrap();

        assert_eq!(payload["transcriptSegments"][0]["speaker"], "Microphone");
        assert_eq!(payload["summary"], "Резюме");
        assert_eq!(payload["rawTranscript"], "здравейте");
    }

    #[test]
    fn revisions_are_strictly_monotonic() {
        let first = next_revision();
        let second = next_revision();
        assert!(second > first);
    }
}

#[cfg(test)]
pub(crate) async fn expand_test_ownership(pool: &SqlitePool) {
    sqlx::raw_sql("CREATE TABLE IF NOT EXISTS meeting_sync_queue (clerk_org_id TEXT NOT NULL,created_by TEXT NOT NULL,external_id TEXT NOT NULL,revision INTEGER NOT NULL,payload TEXT NOT NULL,updated_at TEXT NOT NULL,PRIMARY KEY(clerk_org_id,created_by,external_id))").execute(pool).await.unwrap();
    sqlx::raw_sql(include_str!(
        "../migrations/20261107230000_connections_workspace_identity.sql"
    ))
    .execute(pool)
    .await
    .unwrap();
}
