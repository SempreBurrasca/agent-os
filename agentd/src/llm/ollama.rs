//! Backend Ollama — LLM locale via HTTP API.

use anyhow::{Result, anyhow};
use agentos_common::config::OllamaConfig;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::{LlmBackend, ChatMessage, LlmResponse};

/// Messaggio nel formato Ollama API.
#[derive(Debug, Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaChatMessage>,
    stream: bool,
    options: OllamaOptions,
}

#[derive(Debug, Serialize)]
struct OllamaChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    temperature: f32,
}

/// Risposta dall'API Ollama.
#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: OllamaResponseMessage,
    #[serde(default)]
    eval_count: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseMessage {
    content: String,
}

/// Backend per Ollama (LLM locale).
pub struct OllamaBackend {
    url: String,
    model: String,
    client: reqwest::Client,
}

impl OllamaBackend {
    pub fn new(config: &OllamaConfig) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .expect("Errore creazione client HTTP");

        Self {
            url: config.url.clone(),
            model: config.model.clone(),
            client,
        }
    }
}

#[async_trait::async_trait]
impl LlmBackend for OllamaBackend {
    fn name(&self) -> &str {
        "ollama"
    }

    async fn is_available(&self) -> bool {
        // Verifica che Ollama sia raggiungibile
        match self.client.get(&self.url).send().await {
            Ok(resp) => resp.status().is_success(),
            Err(_) => false,
        }
    }

    async fn chat(&self, messages: &[ChatMessage], temperature: f32) -> Result<LlmResponse> {
        let ollama_messages: Vec<OllamaChatMessage> = messages.iter().map(|m| {
            OllamaChatMessage {
                role: m.role.clone(),
                content: m.content.clone(),
            }
        }).collect();

        let request = OllamaChatRequest {
            model: self.model.clone(),
            messages: ollama_messages,
            stream: false,
            options: OllamaOptions { temperature },
        };

        let url = format!("{}/api/chat", self.url);
        debug!(url = %url, model = %self.model, "Invio richiesta a Ollama");

        let response = self.client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow!("Errore connessione Ollama: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            warn!(status = %status, body = %body, "Ollama ha risposto con errore");
            return Err(anyhow!("Ollama errore HTTP {}: {}", status, body));
        }

        let ollama_resp: OllamaChatResponse = response.json().await
            .map_err(|e| anyhow!("Errore parsing risposta Ollama: {}", e))?;

        Ok(LlmResponse {
            content: ollama_resp.message.content,
            backend_used: "ollama".to_string(),
            tokens_used: ollama_resp.eval_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ollama_backend_creation() {
        let config = OllamaConfig {
            url: "http://localhost:11434".to_string(),
            model: "llama3.2".to_string(),
            embedding_model: "nomic-embed-text".to_string(),
        };
        let backend = OllamaBackend::new(&config);
        assert_eq!(backend.name(), "ollama");
        assert_eq!(backend.model, "llama3.2");
    }

    #[test]
    fn test_chat_request_serialization() {
        let request = OllamaChatRequest {
            model: "llama3.2".to_string(),
            messages: vec![OllamaChatMessage {
                role: "user".to_string(),
                content: "ciao".to_string(),
            }],
            stream: false,
            options: OllamaOptions { temperature: 0.7 },
        };
        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("llama3.2"));
        assert!(json.contains("\"stream\":false"));
    }
}
