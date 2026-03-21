//! Embedder — generazione embedding semantici via Ollama API.
//!
//! Converte chunk di testo in vettori 768-dim usando nomic-embed-text.
//! Gira come task background con bassa priorità.

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Embedding di un chunk di testo.
#[derive(Debug, Clone)]
pub struct Embedding {
    /// Vettore di embedding (768 dimensioni per nomic-embed-text)
    pub vector: Vec<f32>,
    /// Percorso del file sorgente
    pub source_path: String,
    /// Indice del chunk nel file
    pub chunk_index: usize,
}

/// Richiesta all'API Ollama per embedding.
#[derive(Debug, Serialize)]
struct OllamaEmbedRequest {
    model: String,
    prompt: String,
}

/// Risposta dall'API Ollama per embedding.
#[derive(Debug, Deserialize)]
struct OllamaEmbedResponse {
    embedding: Vec<f32>,
}

/// L'Embedder genera embedding semantici tramite Ollama.
pub struct Embedder {
    /// URL del server Ollama
    ollama_url: String,
    /// Modello di embedding
    model: String,
    /// Client HTTP
    client: reqwest::Client,
}

impl Embedder {
    /// Crea un nuovo Embedder.
    pub fn new(ollama_url: &str, model: &str) -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("Errore creazione client HTTP");

        Self {
            ollama_url: ollama_url.to_string(),
            model: model.to_string(),
            client,
        }
    }

    /// Genera l'embedding per un singolo testo.
    pub async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let request = OllamaEmbedRequest {
            model: self.model.clone(),
            prompt: text.to_string(),
        };

        let url = format!("{}/api/embeddings", self.ollama_url);
        debug!(model = %self.model, text_len = text.len(), "Generazione embedding");

        let response = self.client
            .post(&url)
            .json(&request)
            .send()
            .await
            .map_err(|e| anyhow!("Errore connessione Ollama per embedding: {}", e))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            warn!(status = %status, "Errore Ollama embedding");
            return Err(anyhow!("Ollama embedding errore HTTP {}: {}", status, body));
        }

        let resp: OllamaEmbedResponse = response.json().await
            .map_err(|e| anyhow!("Errore parsing risposta embedding: {}", e))?;

        Ok(resp.embedding)
    }

    /// Genera embedding per una lista di chunk.
    pub async fn embed_batch(
        &self,
        chunks: &[(String, String, usize)], // (text, source_path, chunk_index)
    ) -> Vec<Result<Embedding>> {
        let mut results = Vec::new();

        for (text, source_path, chunk_index) in chunks {
            let result = self.embed(text).await.map(|vector| Embedding {
                vector,
                source_path: source_path.clone(),
                chunk_index: *chunk_index,
            });
            results.push(result);
        }

        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedder_creation() {
        let embedder = Embedder::new("http://localhost:11434", "nomic-embed-text");
        assert_eq!(embedder.model, "nomic-embed-text");
    }
}
