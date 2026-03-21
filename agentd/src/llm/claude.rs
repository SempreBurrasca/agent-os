//! Backend Claude API — LLM remoto via Anthropic API.

use anyhow::{Result, anyhow};
use agentos_common::config::ClaudeConfig;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::{LlmBackend, ChatMessage, LlmResponse};

const CLAUDE_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Richiesta all'API Claude.
#[derive(Debug, Serialize)]
struct ClaudeRequest {
    model: String,
    max_tokens: u32,
    messages: Vec<ClaudeMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    temperature: f32,
}

#[derive(Debug, Serialize)]
struct ClaudeMessage {
    role: String,
    content: String,
}

/// Risposta dall'API Claude.
#[derive(Debug, Deserialize)]
struct ClaudeResponse {
    content: Vec<ClaudeContentBlock>,
    usage: ClaudeUsage,
}

#[derive(Debug, Deserialize)]
struct ClaudeContentBlock {
    text: String,
}

#[derive(Debug, Deserialize)]
struct ClaudeUsage {
    output_tokens: u32,
}

/// Errore dall'API Claude.
#[derive(Debug, Deserialize)]
struct ClaudeError {
    error: ClaudeErrorDetail,
}

#[derive(Debug, Deserialize)]
struct ClaudeErrorDetail {
    message: String,
}

/// Backend per Claude API (Anthropic).
pub struct ClaudeBackend {
    api_key: String,
    model: String,
    max_tokens: u32,
    client: reqwest::Client,
}

impl ClaudeBackend {
    pub fn new(config: &ClaudeConfig) -> Self {
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
impl LlmBackend for ClaudeBackend {
    fn name(&self) -> &str {
        "claude"
    }

    async fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    async fn chat(&self, messages: &[ChatMessage], temperature: f32) -> Result<LlmResponse> {
        if self.api_key.is_empty() {
            return Err(anyhow!("API key Claude non configurata"));
        }

        // Separa il system prompt dai messaggi
        let mut system_prompt = None;
        let claude_messages: Vec<ClaudeMessage> = messages.iter()
            .filter_map(|m| {
                if m.role == "system" {
                    system_prompt = Some(m.content.clone());
                    None
                } else {
                    Some(ClaudeMessage {
                        role: m.role.clone(),
                        content: m.content.clone(),
                    })
                }
            })
            .collect();

        let request = ClaudeRequest {
            model: self.model.clone(),
            max_tokens: self.max_tokens,
            messages: claude_messages,
            system: system_prompt,
            temperature,
        };

        debug!(model = %self.model, "Invio richiesta a Claude API");

        let response = self.client
            .post(CLAUDE_API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow!("Errore connessione Claude: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();

            // Prova a parsare l'errore strutturato
            if let Ok(err) = serde_json::from_str::<ClaudeError>(&body) {
                warn!(status = %status, error = %err.error.message, "Claude API errore");
                return Err(anyhow!("Claude errore: {}", err.error.message));
            }

            return Err(anyhow!("Claude errore HTTP {}: {}", status, body));
        }

        let claude_resp: ClaudeResponse = response.json().await
            .map_err(|e| anyhow!("Errore parsing risposta Claude: {}", e))?;

        let content = claude_resp.content.into_iter()
            .map(|block| block.text)
            .collect::<Vec<_>>()
            .join("\n");

        Ok(LlmResponse {
            content,
            backend_used: "claude".to_string(),
            tokens_used: Some(claude_resp.usage.output_tokens),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_backend_creation() {
        let config = ClaudeConfig {
            api_key: "test-key".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 4096,
        };
        let backend = ClaudeBackend::new(&config);
        assert_eq!(backend.name(), "claude");
        assert_eq!(backend.model, "claude-sonnet-4-20250514");
    }

    #[test]
    fn test_claude_backend_no_key() {
        let config = ClaudeConfig {
            api_key: String::new(),
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 4096,
        };
        let backend = ClaudeBackend::new(&config);
        // is_available va testato con tokio
        assert_eq!(backend.api_key, "");
    }

    #[test]
    fn test_request_serialization() {
        let request = ClaudeRequest {
            model: "claude-sonnet-4-20250514".to_string(),
            max_tokens: 4096,
            messages: vec![ClaudeMessage {
                role: "user".to_string(),
                content: "ciao".to_string(),
            }],
            system: Some("Sei un assistente.".to_string()),
            temperature: 0.7,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("claude-sonnet"));
        assert!(json.contains("Sei un assistente"));
    }
}
