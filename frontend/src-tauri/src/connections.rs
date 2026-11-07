use crate::api::TranscriptSegment;
use crate::connections_queue::{
    drain_queued_payloads, meeting_payload, publish_payload, stored_meeting_identity,
    MeetingPayload,
};
pub use crate::connections_queue::{queue_meeting, recover_pending_finals};
use crate::database::repositories::meeting::MeetingsRepository;
use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use sqlx::SqlitePool;
use std::time::Duration;

static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("valid Connections HTTP client")
});

pub async fn publish_meeting(
    pool: &SqlitePool,
    external_id: &str,
    title: &str,
    segments: &[TranscriptSegment],
    raw_segments: Option<&[TranscriptSegment]>,
    summary: Option<&str>,
    state: &str,
    started_at: Option<&str>,
    ended_at: Option<&str>,
) -> Result<()> {
    let identity = crate::auth::require_operation_identity()?;
    publish_meeting_for_identity(
        pool,
        &identity,
        external_id,
        title,
        segments,
        raw_segments,
        summary,
        state,
        started_at,
        ended_at,
    )
    .await
}

async fn publish_meeting_for_identity(
    pool: &SqlitePool,
    identity: &crate::auth::OperationIdentity,
    external_id: &str,
    title: &str,
    segments: &[TranscriptSegment],
    raw_segments: Option<&[TranscriptSegment]>,
    summary: Option<&str>,
    state: &str,
    started_at: Option<&str>,
    ended_at: Option<&str>,
) -> Result<()> {
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
    publish_payload(
        pool,
        identity,
        external_id,
        &payload,
        |external_id, payload| {
            let identity = identity.clone();
            async move { send_payload(&identity, &external_id, &payload).await }
        },
    )
    .await
}

async fn send_payload(
    identity: &crate::auth::OperationIdentity,
    external_id: &str,
    payload: &MeetingPayload,
) -> Result<()> {
    let current_identity = crate::auth::require_current_operation_identity()?;
    if current_identity != *identity {
        return Err(anyhow!(
            "The queued meeting belongs to a different Connections user, issuer or workspace."
        ));
    }
    let access_token = crate::auth::access_token().await?;
    if crate::auth::require_current_operation_identity()? != *identity {
        return Err(anyhow!(
            "The queued meeting belongs to a different Connections user, issuer or workspace."
        ));
    }
    let encoded_id =
        url::form_urlencoded::byte_serialize(external_id.as_bytes()).collect::<String>();
    let url = format!(
        "{}/api/meetings/{encoded_id}",
        identity
            .issuer
            .strip_suffix("/api/identity")
            .ok_or_else(|| anyhow!("Invalid recording issuer."))?
    );
    let response = HTTP_CLIENT
        .put(url)
        .bearer_auth(access_token)
        .json(payload)
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow!("Connections ingest returned {}", response.status()));
    }
    Ok(())
}

pub fn retry_queued_meetings(pool: SqlitePool, identity: crate::auth::OperationIdentity) {
    tauri::async_runtime::spawn(async move {
        if let Err(error) = drain_queued_payloads(&pool, &identity, |external_id, payload| {
            let identity = identity.clone();
            async move { send_payload(&identity, &external_id, &payload).await }
        })
        .await
        {
            log::warn!("Queued meeting sync stopped after a delivery failure: {error:#}");
        }
    });
}

#[tauri::command]
pub async fn api_publish_live_transcript(
    state: tauri::State<'_, crate::state::AppState>,
    external_id: String,
    title: String,
    transcripts: Vec<serde_json::Value>,
) -> std::result::Result<(), String> {
    let segments = transcripts
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<Vec<TranscriptSegment>, _>>()
        .map_err(|error| format!("Invalid live transcript segment: {error}"))?;
    publish_meeting(
        state.db_manager.pool(),
        &external_id,
        &title,
        &segments,
        None,
        None,
        "live",
        None,
        None,
    )
    .await
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn api_recover_legacy_recordings(
    state: tauri::State<'_, crate::state::AppState>,
) -> std::result::Result<u64, String> {
    let token = crate::auth::access_token()
        .await
        .map_err(|error| error.to_string())?;
    let identity =
        crate::auth::require_current_operation_identity().map_err(|error| error.to_string())?;
    let origin = identity
        .issuer
        .strip_suffix("/api/identity")
        .ok_or("Invalid recovery issuer.")?;
    let response = HTTP_CLIENT
        .get(format!("{origin}/api/meetings-identity/legacy"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!(
            "Connections recovery returned {}",
            response.status()
        ));
    }
    let response: crate::connections_identity::LegacyIdentityResponse =
        response.json().await.map_err(|error| error.to_string())?;
    if crate::auth::require_current_operation_identity().map_err(|error| error.to_string())?
        != identity
    {
        return Err("Sign-in changed during recording recovery. Try again.".into());
    }
    let proof = response.verify(&identity).map_err(|error| error.to_string())?
        .ok_or("Connections has not verified the original ownership for this workspace. Your recordings remain local.")?;
    let recovered =
        crate::database::legacy_recovery::recover_verified_legacy(state.db_manager.pool(), &proof)
            .await
            .map_err(|error| error.to_string())?;
    retry_queued_meetings(state.db_manager.pool().clone(), identity);
    Ok(recovered)
}

#[tauri::command]
pub async fn api_legacy_recording_count(
    state: tauri::State<'_, crate::state::AppState>,
) -> std::result::Result<i64, String> {
    crate::database::legacy_preservation::legacy_recording_count(state.db_manager.pool())
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn api_retry_meeting_sync(
    state: tauri::State<'_, crate::state::AppState>,
) -> std::result::Result<usize, String> {
    crate::auth::access_token()
        .await
        .map_err(|error| error.to_string())?;
    let identity =
        crate::auth::require_current_operation_identity().map_err(|error| error.to_string())?;
    let recovered = recover_pending_finals(state.db_manager.pool(), &identity)
        .await
        .map_err(|error| error.to_string())?;
    if recovered > 0 {
        crate::auth::finish_recording_identity_if_matches(&identity);
    }
    drain_queued_payloads(
        state.db_manager.pool(),
        &identity,
        |external_id, payload| {
            let identity = identity.clone();
            async move { send_payload(&identity, &external_id, &payload).await }
        },
    )
    .await
    .map_err(|error| error.to_string())
}

pub async fn publish_saved_meeting(
    pool: &SqlitePool,
    meeting_id: &str,
    summary: Option<&str>,
) -> Result<()> {
    let identity = crate::auth::require_current_operation_identity()?;
    let stored_identity = stored_meeting_identity(pool, meeting_id, &identity.scope_key()).await?;
    if stored_identity != identity {
        return Err(anyhow!(
            "The saved meeting belongs to a different Connections user, issuer or workspace."
        ));
    }
    let meeting = MeetingsRepository::get_meeting(pool, meeting_id)
        .await?
        .ok_or_else(|| anyhow!("Meeting not found"))?;
    let segments = meeting
        .transcripts
        .iter()
        .map(|segment| TranscriptSegment {
            id: segment.id.clone(),
            text: segment.text.clone(),
            timestamp: segment.timestamp.clone(),
            speaker: segment.speaker.clone(),
            audio_start_time: segment.audio_start_time,
            audio_end_time: segment.audio_end_time,
            duration: segment.duration,
        })
        .collect::<Vec<_>>();
    let raw_segments = meeting
        .transcripts
        .into_iter()
        .map(|segment| TranscriptSegment {
            id: segment.id,
            text: segment.raw_text,
            timestamp: segment.timestamp,
            speaker: segment.speaker,
            audio_start_time: segment.audio_start_time,
            audio_end_time: segment.audio_end_time,
            duration: segment.duration,
        })
        .collect::<Vec<_>>();
    publish_meeting_for_identity(
        pool,
        &identity,
        &meeting.id,
        &meeting.title,
        &segments,
        Some(&raw_segments),
        summary,
        "final",
        Some(&meeting.created_at),
        Some(&meeting.updated_at),
    )
    .await
}
