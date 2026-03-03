use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{error, info, warn};

/// A single chat message in the OpenAI conversation format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }

    #[allow(dead_code)]
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

/// Request payload for the OpenAI-compatible chat completions API.
#[derive(Debug, Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
    temperature: f32,
    top_p: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_k: Option<u32>,
    stream: bool,
}

/// Response from the OpenAI-compatible chat completions API.
#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Debug, Deserialize)]
struct ChatChoiceMessage {
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
}

/// AI client for OpenAI-compatible APIs (NVIDIA, OpenAI, etc.) with automatic model fallback.
pub struct AiClient {
    http: Client,
    api_url: String,
    api_key: String,
    fallback_api_url: String,
    fallback_api_key: String,
    default_model: String,
    fallback_model: String,
    timeout: Duration,
}

impl AiClient {
    /// Create a new AI client.
    pub fn new(
        api_url: String,
        api_key: String,
        fallback_api_url: String,
        fallback_api_key: String,
        default_model: String,
        fallback_model: String,
        timeout_secs: u64,
    ) -> Self {
        let timeout = Duration::from_secs(timeout_secs);
        let http = Client::builder()
            .timeout(timeout)
            .build()
            .expect("Failed to build HTTP client");

        Self {
            http,
            api_url,
            api_key,
            fallback_api_url,
            fallback_api_key,
            default_model,
            fallback_model,
            timeout,
        }
    }

    /// Send a chat request using the primary model.
    /// If it fails or times out, automatically retry with the fallback model.
    pub async fn chat(&self, system_prompt: &str, history: &[ChatMessage]) -> Result<String> {
        let mut messages = vec![ChatMessage::system(system_prompt)];
        messages.extend_from_slice(history);

        // ── Attempt 1: Default model ────────────────────────
        info!(model = %self.default_model, "Sending request to primary model");
        match self
            .send_chat_request(&self.api_url, &self.api_key, &self.default_model, &messages)
            .await
        {
            Ok(response) => {
                info!(model = %self.default_model, "Primary model responded successfully");
                return Ok(response);
            }
            Err(e) => {
                warn!(
                    model = %self.default_model,
                    error = %e,
                    "Primary model failed, falling back to secondary model"
                );
            }
        }

        // ── Attempt 2: Fallback model ───────────────────────
        if self.fallback_model.is_empty() {
            return Err(anyhow::anyhow!(
                "Primary model failed and no fallback model is configured"
            ));
        }

        info!(model = %self.fallback_model, "Sending request to fallback model");
        match self
            .send_chat_request(
                &self.fallback_api_url,
                &self.fallback_api_key,
                &self.fallback_model,
                &messages,
            )
            .await
        {
            Ok(response) => {
                info!(model = %self.fallback_model, "Fallback model responded successfully");
                Ok(response)
            }
            Err(e) => {
                error!(
                    model = %self.fallback_model,
                    error = %e,
                    "Fallback model also failed"
                );
                Err(e).context("Both primary and fallback AI models failed")
            }
        }
    }

    /// Make the actual HTTP POST to the OpenAI-compatible chat completions endpoint.
    async fn send_chat_request(
        &self,
        api_url: &str,
        api_key: &str,
        model: &str,
        messages: &[ChatMessage],
    ) -> Result<String> {
        let request_body = ChatCompletionRequest {
            model: model.to_string(),
            messages: messages.to_vec(),
            max_tokens: 16384,
            temperature: 1.0,
            top_p: 0.9,
            top_k: Some(20),
            stream: false,
        };

        let mut req = self
            .http
            .post(api_url)
            .header("Content-Type", "application/json")
            .json(&request_body)
            .timeout(self.timeout);

        if !api_key.trim().is_empty() {
            req = req.header("Authorization", format!("Bearer {}", api_key));
        }

        let response = req
            .send()
            .await
            .with_context(|| format!("HTTP request to AI API failed (model: {})", model))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!(
                "AI API returned HTTP {} for model '{}': {}",
                status,
                model,
                body
            );
        }

        let chat_response: ChatCompletionResponse = response
            .json()
            .await
            .with_context(|| format!("Failed to parse AI API response for model '{}'", model))?;

        // Extract the actual content from the first choice
        let content = chat_response
            .choices
            .first()
            .and_then(|c| c.message.content.as_ref())
            .cloned()
            .unwrap_or_default();

        if content.is_empty() {
            // Some models put thinking in reasoning_content and leave content empty
            let reasoning = chat_response
                .choices
                .first()
                .and_then(|c| c.message.reasoning_content.as_ref())
                .cloned()
                .unwrap_or_default();
            if !reasoning.is_empty() {
                warn!("AI returned only reasoning content, no main content. Using reasoning as fallback.");
                return Ok(reasoning);
            }
            anyhow::bail!("AI returned empty response for model '{}'", model);
        }

        Ok(content)
    }
}
