//! Claude API client for LLM-generated function summaries (SQ-6).
//!
//! Uses `reqwest::blocking` to avoid nested tokio runtime issues
//! (the Store already uses `rt.block_on()`).

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::Store;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2023-06-01";
const MODEL: &str = "claude-haiku-4-5";
const MAX_TOKENS: u32 = 100;
const MAX_CONTENT_CHARS: usize = 8000;
const MAX_RETRIES: u32 = 3;
const MIN_CONTENT_CHARS: usize = 50;

/// Claude API client for generating summaries.
pub struct Client {
    http: reqwest::blocking::Client,
    api_key: String,
}

#[derive(Serialize)]
struct MessagesRequest<'a> {
    model: &'a str,
    max_tokens: u32,
    messages: Vec<Message<'a>>,
}

#[derive(Serialize)]
struct Message<'a> {
    role: &'a str,
    content: &'a str,
}

#[derive(Deserialize)]
struct MessagesResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct ApiError {
    error: ApiErrorDetail,
}

#[derive(Deserialize)]
struct ApiErrorDetail {
    message: String,
}

impl Client {
    pub fn new(api_key: &str) -> Self {
        Self {
            http: reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("Failed to create HTTP client"),
            api_key: api_key.to_string(),
        }
    }

    /// Generate a one-sentence summary for a code chunk.
    ///
    /// Returns None if the API returns an empty or invalid response.
    pub fn summarize_chunk(
        &self,
        content: &str,
        chunk_type: &str,
        language: &str,
    ) -> Result<Option<String>> {
        let truncated = if content.len() > MAX_CONTENT_CHARS {
            tracing::debug!(
                len = content.len(),
                max = MAX_CONTENT_CHARS,
                "Truncating chunk content for LLM summary"
            );
            &content[..MAX_CONTENT_CHARS]
        } else {
            content
        };

        let prompt = format!(
            "Summarize this {} in one sentence. Focus on what it does, not how.\n\n```{}\n{}\n```",
            chunk_type, language, truncated
        );

        let request = MessagesRequest {
            model: MODEL,
            max_tokens: MAX_TOKENS,
            messages: vec![Message {
                role: "user",
                content: &prompt,
            }],
        };

        let mut last_err = None;
        for attempt in 0..MAX_RETRIES {
            let start = std::time::Instant::now();
            let response = self
                .http
                .post(API_URL)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", API_VERSION)
                .header("content-type", "application/json")
                .json(&request)
                .send();

            let response = match response {
                Ok(r) => r,
                Err(e) => {
                    if attempt < MAX_RETRIES - 1 {
                        let delay = Duration::from_secs(1 << attempt);
                        tracing::warn!(attempt, error = %e, delay_secs = delay.as_secs(), "API request failed, retrying");
                        std::thread::sleep(delay);
                        last_err = Some(format!("Connection error: {e}"));
                        continue;
                    }
                    bail!("LLM summary API unreachable after {MAX_RETRIES} attempts: {e}");
                }
            };

            let status = response.status();
            let latency = start.elapsed();
            tracing::debug!(
                status = status.as_u16(),
                latency_ms = latency.as_millis() as u64,
                "API response"
            );

            if status == 401 {
                bail!("Invalid ANTHROPIC_API_KEY (401 Unauthorized)");
            }

            if status == 429 || status.is_server_error() {
                if attempt < MAX_RETRIES - 1 {
                    let delay = Duration::from_secs(1 << attempt);
                    tracing::warn!(
                        attempt,
                        status = status.as_u16(),
                        delay_secs = delay.as_secs(),
                        "Rate limited or server error, retrying"
                    );
                    std::thread::sleep(delay);
                    last_err = Some(format!("HTTP {status}"));
                    continue;
                }
                let body = response.text().unwrap_or_default();
                bail!("LLM summary API failed after {MAX_RETRIES} attempts: HTTP {status}: {body}");
            }

            if !status.is_success() {
                let body = response.text().unwrap_or_default();
                if let Ok(err) = serde_json::from_str::<ApiError>(&body) {
                    bail!("LLM summary API error: {}", err.error.message);
                }
                bail!("LLM summary API error: HTTP {status}: {body}");
            }

            let resp: MessagesResponse = response
                .json()
                .context("Failed to parse LLM summary API response")?;

            let text = resp
                .content
                .into_iter()
                .find(|b| b.block_type == "text")
                .and_then(|b| b.text);

            match text {
                Some(s) if !s.trim().is_empty() && s.len() < 500 => {
                    return Ok(Some(s.trim().to_string()))
                }
                Some(s) => {
                    tracing::warn!(
                        len = s.len(),
                        "LLM returned empty or oversized summary, skipping"
                    );
                    return Ok(None);
                }
                None => {
                    tracing::warn!("LLM returned no text block, skipping");
                    return Ok(None);
                }
            }
        }

        bail!(
            "LLM summary API failed after {MAX_RETRIES} attempts: {}",
            last_err.unwrap_or_else(|| "unknown error".to_string())
        );
    }
}

/// A summary entry ready for storage.
pub struct SummaryEntry {
    pub content_hash: String,
    pub summary: String,
    pub model: String,
}

/// Run the LLM summary pass: generate summaries for uncached callable chunks.
///
/// Only callable chunks (Function, Method, Macro) with content >= 50 chars
/// and no existing doc comment are sent to the API. Chunks with doc comments
/// use the first sentence of the doc as the summary (free).
///
/// Returns the number of new summaries generated via API.
pub fn llm_summary_pass(store: &Store, quiet: bool) -> Result<usize> {
    let _span = tracing::info_span!("llm_summary_pass").entered();

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .context("--llm-summaries requires ANTHROPIC_API_KEY environment variable")?;
    let client = Client::new(&api_key);

    let mut api_generated = 0usize;
    let mut doc_extracted = 0usize;
    let mut cached = 0usize;
    let mut skipped = 0usize;
    let mut cursor = 0i64;
    const PAGE_SIZE: usize = 500;

    let stats = store.stats().context("Failed to get index stats")?;
    let progress = if quiet {
        indicatif::ProgressBar::hidden()
    } else {
        let pb = indicatif::ProgressBar::new(stats.total_chunks);
        pb.set_style(
            indicatif::ProgressStyle::default_bar()
                .template("{spinner:.green} [{bar:40}] {pos}/{len} LLM summaries ({eta})")
                .expect("valid template")
                .progress_chars("=>-"),
        );
        pb
    };

    loop {
        let (chunks, next) = store
            .chunks_paged(cursor, PAGE_SIZE)
            .context("Failed to page chunks")?;
        if chunks.is_empty() {
            break;
        }
        cursor = next;

        // Batch-check existing summaries
        let hashes: Vec<&str> = chunks.iter().map(|c| c.content_hash.as_str()).collect();
        let existing = store
            .get_summaries_by_hashes(&hashes)
            .context("Failed to fetch existing summaries")?;

        let mut to_store: Vec<(String, String, String)> = Vec::new();

        for cs in &chunks {
            progress.inc(1);

            // Already cached
            if existing.contains_key(&cs.content_hash) {
                cached += 1;
                continue;
            }

            // Only callable chunks
            if !cs.chunk_type.is_callable() {
                skipped += 1;
                continue;
            }

            // Skip tiny chunks
            if cs.content.len() < MIN_CONTENT_CHARS {
                skipped += 1;
                continue;
            }

            // Skip windowed chunks after the first (only summarize first/only window)
            if cs.window_idx.is_some_and(|idx| idx > 0) {
                skipped += 1;
                continue;
            }

            // Doc comment shortcut: use first sentence if available
            if let Some(ref doc) = cs.doc {
                if doc.len() > 10 {
                    let first_sentence = extract_first_sentence(doc);
                    if !first_sentence.is_empty() {
                        to_store.push((
                            cs.content_hash.clone(),
                            first_sentence,
                            "doc-comment".to_string(),
                        ));
                        doc_extracted += 1;
                        continue;
                    }
                }
            }

            // Call Claude API
            let language = cs.language.to_string();
            let type_name = cs.chunk_type.to_string();
            match client.summarize_chunk(&cs.content, &type_name, &language) {
                Ok(Some(summary)) => {
                    to_store.push((cs.content_hash.clone(), summary, MODEL.to_string()));
                    api_generated += 1;
                }
                Ok(None) => {
                    skipped += 1;
                }
                Err(e) => {
                    progress.finish_and_clear();
                    return Err(e);
                }
            }

            // Flush stored summaries periodically
            if to_store.len() >= 50 {
                store
                    .upsert_summaries_batch(&to_store)
                    .context("Failed to store LLM summaries")?;
                to_store.clear();
            }
        }

        // Flush remaining
        if !to_store.is_empty() {
            store
                .upsert_summaries_batch(&to_store)
                .context("Failed to store LLM summaries")?;
        }
    }

    progress.finish_and_clear();

    tracing::info!(
        api_generated,
        doc_extracted,
        cached,
        skipped,
        "LLM summary pass complete"
    );
    if !quiet {
        eprintln!(
            "LLM summaries: {} from API, {} from doc comments, {} cached, {} skipped",
            api_generated, doc_extracted, cached, skipped
        );
    }

    Ok(api_generated + doc_extracted)
}

/// Extract the first sentence from a doc comment.
fn extract_first_sentence(doc: &str) -> String {
    let trimmed = doc.trim();
    // Find first sentence-ending punctuation followed by space or end
    if let Some(pos) = trimmed.find(['.', '!', '?']) {
        let sentence = trimmed[..=pos].trim();
        if sentence.len() > 10 {
            return sentence.to_string();
        }
    }
    // If no sentence boundary, use first line if it's substantial
    let first_line = trimmed.lines().next().unwrap_or("").trim();
    if first_line.len() > 10 {
        first_line.to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_first_sentence_period() {
        assert_eq!(
            extract_first_sentence("Parse a config file. Returns validated settings."),
            "Parse a config file."
        );
    }

    #[test]
    fn test_extract_first_sentence_no_period() {
        assert_eq!(
            extract_first_sentence("Parse a config file and return settings"),
            "Parse a config file and return settings"
        );
    }

    #[test]
    fn test_extract_first_sentence_short() {
        // Too short to be useful
        assert_eq!(extract_first_sentence("Hi."), "");
    }

    #[test]
    fn test_extract_first_sentence_multiline() {
        assert_eq!(
            extract_first_sentence("Parse a config file.\n\nThis handles TOML and JSON."),
            "Parse a config file."
        );
    }

    #[test]
    fn test_content_truncation() {
        let long_content = "x".repeat(10000);
        let truncated = if long_content.len() > MAX_CONTENT_CHARS {
            &long_content[..MAX_CONTENT_CHARS]
        } else {
            &long_content
        };
        assert_eq!(truncated.len(), MAX_CONTENT_CHARS);
    }
}
