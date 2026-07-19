use crate::database::repositories::setting::SettingsRepository;
use crate::state::AppState;
use crate::summary::llm_client::{generate_summary, LLMProvider};
use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sqlx::{Connection, SqlitePool};
use std::path::PathBuf;
use tauri::{AppHandle, Manager, Runtime};

const SYSTEM_PROMPT: &str = r#"You correct automatic speech-recognition transcripts, especially Bulgarian.
Return only a JSON array. Every output object must contain exactly the same `id` and one corrected `text`.
Correct misheard words, spelling, punctuation, and grammar by using the surrounding meaning.
Do not summarize, translate, censor, add facts, merge segments, split segments, or change IDs.
Keep the original language and preserve the speaker's meaning."#;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EnhancementItem {
    id: String,
    text: String,
}

struct ModelRequestConfig {
    provider: LLMProvider,
    model: String,
    api_key: String,
    ollama_endpoint: Option<String>,
    custom_endpoint: Option<String>,
    max_tokens: Option<u32>,
    temperature: Option<f32>,
    top_p: Option<f32>,
    app_data_dir: Option<PathBuf>,
}

#[tauri::command]
pub async fn api_enhance_transcript<R: Runtime>(
    app: AppHandle<R>,
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> std::result::Result<(), String> {
    if meeting_id.trim().is_empty() {
        return Err("meeting_id cannot be empty".to_string());
    }

    let pool = state.db_manager.pool().clone();
    let status: Option<String> = sqlx::query_scalar(
        "SELECT transcript_enhancement_status FROM meetings WHERE id = ?",
    )
    .bind(&meeting_id)
    .fetch_optional(&pool)
    .await
    .map_err(|error| error.to_string())?;

    match status.as_deref() {
        None => return Err("Meeting not found".to_string()),
        Some("processing") | Some("completed") => return Ok(()),
        _ => {}
    }

    sqlx::query(
        "UPDATE meetings SET transcript_enhancement_status = 'processing', transcript_enhancement_error = NULL WHERE id = ?",
    )
    .bind(&meeting_id)
    .execute(&pool)
    .await
    .map_err(|error| error.to_string())?;

    tauri::async_runtime::spawn(async move {
        if let Err(error) = enhance_meeting(&app, &pool, &meeting_id).await {
            log::warn!("Transcript enhancement failed for {}: {:#}", meeting_id, error);
            let _ = sqlx::query(
                "UPDATE meetings SET transcript_enhancement_status = 'failed', transcript_enhancement_error = ? WHERE id = ?",
            )
            .bind(error.to_string())
            .bind(&meeting_id)
            .execute(&pool)
            .await;
        }
    });

    Ok(())
}

async fn enhance_meeting<R: Runtime>(
    app: &AppHandle<R>,
    pool: &SqlitePool,
    meeting_id: &str,
) -> Result<()> {
    let raw_segments = sqlx::query_as::<_, (String, String)>(
        "SELECT id, transcript FROM transcripts WHERE meeting_id = ? ORDER BY audio_start_time ASC, rowid ASC",
    )
    .bind(meeting_id)
    .fetch_all(pool)
    .await?;

    if raw_segments.is_empty() {
        return Err(anyhow!("Meeting has no transcript segments"));
    }

    let config = load_model_config(app, pool).await?;
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?;
    let input = raw_segments
        .into_iter()
        .map(|(id, text)| EnhancementItem { id, text })
        .collect::<Vec<_>>();
    let mut enhanced = Vec::with_capacity(input.len());

    for batch in build_batches(&input) {
        let user_prompt = serde_json::to_string(batch)?;
        let response = generate_summary(
            &client,
            &config.provider,
            &config.model,
            &config.api_key,
            SYSTEM_PROMPT,
            &user_prompt,
            config.ollama_endpoint.as_deref(),
            config.custom_endpoint.as_deref(),
            config.max_tokens.or(Some(4096)),
            config.temperature.or(Some(0.1)),
            config.top_p,
            config.app_data_dir.as_ref(),
            None,
        )
        .await
        .map_err(|error| anyhow!(error))?;
        enhanced.extend(parse_and_validate_response(batch, &response)?);
    }

    let mut connection = pool.acquire().await?;
    let mut transaction = connection.begin().await?;
    for item in &enhanced {
        sqlx::query("UPDATE transcripts SET enhanced_transcript = ? WHERE id = ? AND meeting_id = ?")
            .bind(&item.text)
            .bind(&item.id)
            .bind(meeting_id)
            .execute(&mut *transaction)
            .await?;
    }
    sqlx::query(
        "UPDATE meetings SET transcript_enhancement_status = 'completed', transcript_enhancement_error = NULL, updated_at = ? WHERE id = ?",
    )
    .bind(chrono::Utc::now())
    .bind(meeting_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;

    if let Err(error) = crate::connections::publish_saved_meeting(pool, meeting_id, None).await {
        log::warn!("Failed to publish enhanced transcript to Connections: {}", error);
    }
    Ok(())
}

async fn load_model_config<R: Runtime>(
    app: &AppHandle<R>,
    pool: &SqlitePool,
) -> Result<ModelRequestConfig> {
    let setting = SettingsRepository::get_model_config(pool)
        .await?
        .ok_or_else(|| anyhow!("Configure an AI summary model before automatic transcript enhancement"))?;
    if setting.model.trim().is_empty() {
        return Err(anyhow!("Configure an AI summary model before automatic transcript enhancement"));
    }
    let provider = LLMProvider::from_str(&setting.provider).map_err(|error| anyhow!(error))?;
    let mut api_key = String::new();
    let mut custom_endpoint = None;
    let mut max_tokens = None;
    let mut temperature = None;
    let mut top_p = None;

    if provider == LLMProvider::CustomOpenAI {
        let custom = setting
            .get_custom_openai_config()
            .ok_or_else(|| anyhow!("Custom OpenAI configuration is missing"))?;
        api_key = custom.api_key.unwrap_or_default();
        custom_endpoint = Some(custom.endpoint);
        max_tokens = custom.max_tokens.map(|value| value as u32);
        temperature = custom.temperature;
        top_p = custom.top_p;
    } else if provider != LLMProvider::Ollama && provider != LLMProvider::BuiltInAI {
        api_key = SettingsRepository::get_api_key(pool, &setting.provider)
            .await?
            .filter(|key| !key.trim().is_empty())
            .ok_or_else(|| anyhow!("API key not found for {}", setting.provider))?;
    }

    Ok(ModelRequestConfig {
        provider,
        model: setting.model,
        api_key,
        ollama_endpoint: setting.ollama_endpoint,
        custom_endpoint,
        max_tokens,
        temperature,
        top_p,
        app_data_dir: app.path().app_data_dir().ok(),
    })
}

fn build_batches(items: &[EnhancementItem]) -> Vec<&[EnhancementItem]> {
    let mut batches = Vec::new();
    let mut start = 0;
    while start < items.len() {
        let mut end = start;
        let mut characters = 0;
        while end < items.len() && end - start < 12 {
            let next = items[end].text.chars().count();
            if end > start && characters + next > 5_000 {
                break;
            }
            characters += next;
            end += 1;
        }
        batches.push(&items[start..end]);
        start = end;
    }
    batches
}

fn parse_and_validate_response(
    input: &[EnhancementItem],
    response: &str,
) -> Result<Vec<EnhancementItem>> {
    let start = response.find('[').context("LLM response did not contain a JSON array")?;
    let end = response.rfind(']').context("LLM response did not contain a complete JSON array")?;
    let output: Vec<EnhancementItem> = serde_json::from_str(&response[start..=end])?;
    if output.len() != input.len() {
        return Err(anyhow!("LLM changed the transcript segment count"));
    }
    for (expected, actual) in input.iter().zip(&output) {
        if actual.id != expected.id {
            return Err(anyhow!("LLM changed transcript segment identity or order"));
        }
        if actual.text.trim().is_empty() && !expected.text.trim().is_empty() {
            return Err(anyhow!("LLM removed transcript text"));
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_ids_and_extracts_fenced_json() {
        let input = vec![EnhancementItem { id: "a".into(), text: "здравейте".into() }];
        let parsed = parse_and_validate_response(
            &input,
            "```json\n[{\"id\":\"a\",\"text\":\"Здравейте!\"}]\n```",
        )
        .unwrap();
        assert_eq!(parsed[0].text, "Здравейте!");
    }

    #[test]
    fn rejects_changed_segment_identity() {
        let input = vec![EnhancementItem { id: "a".into(), text: "text".into() }];
        assert!(parse_and_validate_response(
            &input,
            "[{\"id\":\"b\",\"text\":\"text\"}]",
        )
        .is_err());
    }
}
