use crate::api::TranscriptSegment;
use crate::database::repositories::meeting::MeetingsRepository;
use anyhow::{anyhow, Result};
use serde::Serialize;
use sqlx::SqlitePool;
use std::time::Duration;

const DEFAULT_INGEST_URL: &str =
    "https://4ksznmsh.eu-central.insforge.app/functions/meetily";
const KEYCHAIN_ACCOUNT: &str = "meetily";
const KEYCHAIN_SERVICE: &str = "connections-api-key";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MeetingPayload<'a> {
    external_id: &'a str,
    title: &'a str,
    transcript: String,
    transcript_segments: &'a [TranscriptSegment],
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<&'a str>,
}

pub async fn publish_meeting(
    external_id: &str,
    title: &str,
    segments: &[TranscriptSegment],
    summary: Option<&str>,
) -> Result<()> {
    let Some(api_key) = api_key() else {
        return Ok(());
    };
    let url = std::env::var("MEETILY_CONNECTIONS_URL")
        .unwrap_or_else(|_| DEFAULT_INGEST_URL.to_string());
    let transcript = segments
        .iter()
        .map(|segment| segment.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if transcript.is_empty() {
        return Ok(());
    }

    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()?
        .post(url)
        .bearer_auth(api_key)
        .json(&MeetingPayload {
            external_id,
            title,
            transcript,
            transcript_segments: segments,
            summary,
        })
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow!("Connections ingest returned {}", response.status()));
    }
    Ok(())
}

pub async fn publish_saved_meeting(
    pool: &SqlitePool,
    meeting_id: &str,
    summary: Option<&str>,
) -> Result<()> {
    let meeting = MeetingsRepository::get_meeting(pool, meeting_id)
        .await?
        .ok_or_else(|| anyhow!("Meeting not found"))?;
    let segments = meeting
        .transcripts
        .into_iter()
        .map(|segment| TranscriptSegment {
            id: segment.id,
            text: segment.text,
            timestamp: segment.timestamp,
            speaker: segment.speaker,
            audio_start_time: segment.audio_start_time,
            audio_end_time: segment.audio_end_time,
            duration: segment.duration,
        })
        .collect::<Vec<_>>();
    publish_meeting(&meeting.id, &meeting.title, &segments, summary).await
}

fn api_key() -> Option<String> {
    std::env::var("MEETILY_CONNECTIONS_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(keychain_api_key)
}

#[cfg(target_os = "macos")]
fn keychain_api_key() -> Option<String> {
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-a",
            KEYCHAIN_ACCOUNT,
            "-s",
            KEYCHAIN_SERVICE,
            "-w",
        ])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(not(target_os = "macos"))]
fn keychain_api_key() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_is_optional_when_unconfigured() {
        std::env::remove_var("MEETILY_CONNECTIONS_API_KEY");
        assert!(api_key().is_none() || cfg!(target_os = "macos"));
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
            external_id: "meeting-1",
            title: "Test",
            transcript: "Здравейте".to_string(),
            transcript_segments: &segments,
            summary: Some("Резюме"),
        })
        .unwrap();

        assert_eq!(payload["transcriptSegments"][0]["speaker"], "Microphone");
        assert_eq!(payload["summary"], "Резюме");
    }
}
