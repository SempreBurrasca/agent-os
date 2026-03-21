//! Backend OpenAI API — LLM remoto via OpenAI API.

use anyhow::{Result, anyhow};
use agentos_common::config::OpenAiConfig;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::{LlmBackend, ChatMessage, LlmResponse};

const OPENAI_API_URL: &str = "https://api.openai.com/v1/chat/completions";

/// Richiesta all'API OpenAI.
#[derive(Debug, Serialize)]
struct OpenAiRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    max_completion_tokens: u32,
    temperature: f32,
}

#[derive(Debug, Serialize)]
struct OpenAiMessage {
    role: String,
    content: String,
}

/// Risposta dall'API OpenAI.
#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    choices: Vec<OpenAiChoice>,
    usage: OpenAiUsage,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiResponseMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiResponseMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    completion_tokens: u32,
}

/// Backend per OpenAI API.
pub struct OpenAiBackend {
    api_key: String,
    model: String,
    max_tokens: u32,
    client: reqwest::Client,
}

impl OpenAiBackend {
    pub fn new(config: &OpenAiConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("Errore creazione client HTTP");

        Self {
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            max_tokens: config.max_tokens,
            client,
        }
    }
}

#[async_trait::async_trait]
impl LlmBackend for OpenAiBackend {
    fn name(&self) -> &str {
        "openai"
    }

    async fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    async fn chat(&self, messages: &[ChatMessage], temperature: f32) -> Result<LlmResponse> {
        if self.api_key.is_empty() {
            return Err(anyhow!("API key OpenAI non configurata"));
        }

        let openai_messages: Vec<OpenAiMessage> = messages.iter().map(|m| {
            OpenAiMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            }
        }).collect();

        let request = OpenAiRequest {
            model: self.model.clone(),
            messages: openai_messages,
            max_completion_tokens: self.max_tokens,
            temperature,
        };

        debug!(model = %self.model, "Invio richiesta a OpenAI API");

        let response = self.client
            .post(OPENAI_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow!("Errore connessione OpenAI: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            warn!(status = %status, "OpenAI API errore");
            return Err(anyhow!("OpenAI errore HTTP {}: {}", status, body));
        }

        let openai_resp: OpenAiResponse = response.json().await
            .map_err(|e| anyhow!("Errore parsing risposta OpenAI: {}", e))?;

        let content = openai_resp.choices.into_iter()
            .next()
            .map(|c| c.message.content)
            .unwrap_or_default();

        Ok(LlmResponse {
            content,
            backend_used: "openai".to_string(),
            tokens_used: Some(openai_resp.usage.completion_tokens),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_backend_creation() {
        let config = OpenAiConfig {
            api_key: "test-key".to_string(),
            model: "gpt-4o".to_string(),
            max_tokens: 4096,
        };
        let backend = OpenAiBackend::new(&config);
        assert_eq!(backend.name(), "openai");
        assert_eq!(backend.model, "gpt-4o");
    }
}
