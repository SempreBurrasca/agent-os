//! Router LLM — seleziona il backend giusto per ogni richiesta.
//!
//! Supporta tre backend: Ollama (locale), Claude API, OpenAI API.
//! Il router sceglie in base alla configurazione e gestisce il fallback
//! automatico se un backend non è disponibile.

pub mod ollama;
pub mod claude;
pub mod openai;

use anyhow::{Result, anyhow};
use agentos_common::config::AgentOsConfig;
use tracing::{info, warn, error};

/// Messaggio nella conversazione con l'LLM.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,    // "system", "user", "assistant"
    pub content: String,
}

/// Risposta dall'LLM.
#[derive(Debug, Clone)]
pub struct LlmResponse {
    pub content: String,
    pub backend_used: String,
    pub tokens_used: Option<u32>,
}

/// Trait per i backend LLM.
#[async_trait::async_trait]
pub trait LlmBackend: Send + Sync {
    /// Nome del backend (per logging e diagnostica).
    fn name(&self) -> &str;

    /// Verifica se il backend è disponibile e configurato.
    async fn is_available(&self) -> bool;

    /// Invia messaggi e riceve la risposta.
    async fn chat(&self, messages: &[ChatMessage], temperature: f32) -> Result<LlmResponse>;
}

/// Router LLM — sceglie il backend e gestisce il fallback.
pub struct LlmRouter {
    backends: Vec<(String, Box<dyn LlmBackend>)>,
    default_backend: String,
    complex_backend: String,
    fallback_backend: String,
}

impl LlmRouter {
    /// Crea un nuovo router dalla configurazione.
    pub fn new(config: &AgentOsConfig) -> Self {
        let mut backends: Vec<(String, Box<dyn LlmBackend>)> = Vec::new();

        // Registra Ollama
        backends.push((
            "ollama".to_string(),
            Box::new(ollama::OllamaBackend::new(&config.ollama)),
        ));

        // Registra Claude (se configurato)
        if !config.claude.api_key.is_empty() {
            backends.push((
                "claude".to_string(),
                Box::new(claude::ClaudeBackend::new(&config.claude)),
            ));
        }

        // Registra OpenAI (se configurato)
        if !config.openai.api_key.is_empty() {
            backends.push((
                "openai".to_string(),
                Box::new(openai::OpenAiBackend::new(&config.openai)),
            ));
        }

        Self {
            backends,
            default_backend: config.llm.default_backend.clone(),
            complex_backend: config.llm.complex_backend.clone(),
            fallback_backend: config.llm.fallback_backend.clone(),
        }
    }

    /// Invia una richiesta al backend predefinito, con fallback automatico.
    pub async fn chat(&self, messages: &[ChatMessage], temperature: f32) -> Result<LlmResponse> {
        self.chat_with_strategy(messages, temperature, &self.default_backend).await
    }

    /// Invia una richiesta al backend per compiti complessi, con fallback.
    pub async fn chat_complex(&self, messages: &[ChatMessage], temperature: f32) -> Result<LlmResponse> {
        self.chat_with_strategy(messages, temperature, &self.complex_backend).await
    }

    /// Strategia di fallback: prova il backend primario, poi il fallback.
    async fn chat_with_strategy(
        &self,
        messages: &[ChatMessage],
        temperature: f32,
        primary: &str,
    ) -> Result<LlmResponse> {
        // Prova il backend primario
        if let Some(backend) = self.get_backend(primary) {
            match backend.chat(messages, temperature).await {
                Ok(response) => return Ok(response),
                Err(e) => warn!(backend = primary, error = %e, "Backend primario fallito, provo fallback"),
            }
        } else {
            warn!(backend = primary, "Backend primario non trovato");
        }

        // Prova il fallback
        if primary != self.fallback_backend {
            if let Some(backend) = self.get_backend(&self.fallback_backend) {
                match backend.chat(messages, temperature).await {
                    Ok(response) => {
                        info!(backend = %self.fallback_backend, "Usato backend di fallback");
                        return Ok(response);
                    }
                    Err(e) => error!(backend = %self.fallback_backend, error = %e, "Anche il fallback è fallito"),
                }
            }
        }

        // Prova tutti gli altri backend come ultima risorsa
        for (name, backend) in &self.backends {
            if name != primary && name != &self.fallback_backend {
                if let Ok(response) = backend.chat(messages, temperature).await {
                    info!(backend = %name, "Usato backend di ultima risorsa");
                    return Ok(response);
                }
            }
        }

        Err(anyhow!("Nessun backend LLM disponibile"))
    }

    /// Trova un backend per nome.
    fn get_backend(&self, name: &str) -> Option<&dyn LlmBackend> {
        self.backends.iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_ref())
    }

    /// Verifica quali backend sono disponibili.
    pub async fn check_backends(&self) -> Vec<(String, bool)> {
        let mut results = Vec::new();
        for (name, backend) in &self.backends {
            let available = backend.is_available().await;
            results.push((name.clone(), available));
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chat_message_creation() {
        let msg = ChatMessage {
            role: "user".to_string(),
            content: "Ciao".to_string(),
        };
        assert_eq!(msg.role, "user");
    }

    #[test]
    fn test_llm_response_creation() {
        let resp = LlmResponse {
            content: "Ecco i file".to_string(),
            backend_used: "ollama".to_string(),
            tokens_used: Some(42),
        };
        assert_eq!(resp.backend_used, "ollama");
    }
}
