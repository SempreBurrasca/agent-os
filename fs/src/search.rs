//! Search — ricerca semantica nei file indicizzati.
//!
//! Genera l'embedding della query, cerca i vettori più simili,
//! e calcola il ranking finale combinando similarità, recenza e frequenza.

use anyhow::Result;
use agentos_common::types::SearchResult;
use chrono::Utc;
use tracing::{debug, warn};

use crate::embedder::Embedder;
use crate::storage::{Storage, StorageSearchHit};

/// Parametri di ricerca.
pub struct SearchParams {
    /// Query testuale
    pub query: String,
    /// Filtro per tipo file (opzionale)
    pub file_type: Option<String>,
    /// Filtro per cartella (opzionale)
    pub folder: Option<String>,
    /// Numero massimo di risultati
    pub max_results: usize,
}

/// Risultato grezzo dalla ricerca vettoriale (prima del ranking finale).
#[derive(Debug)]
pub struct RawSearchHit {
    pub path: String,
    pub name: String,
    pub snippet: String,
    pub semantic_score: f64,
    pub file_type: String,
    pub modified_at: chrono::DateTime<Utc>,
    pub access_count: u32,
}

/// Motore di ricerca semantica.
pub struct SearchEngine {
    /// Peso della similarità semantica nel ranking (default 0.7)
    semantic_weight: f64,
    /// Peso della recenza nel ranking (default 0.2)
    recency_weight: f64,
    /// Peso della frequenza d'uso nel ranking (default 0.1)
    frequency_weight: f64,
}

impl SearchEngine {
    /// Crea un nuovo SearchEngine con i pesi di default.
    pub fn new() -> Self {
        Self {
            semantic_weight: 0.7,
            recency_weight: 0.2,
            frequency_weight: 0.1,
        }
    }

    /// Calcola il punteggio finale combinato per un risultato.
    pub fn compute_score(&self, hit: &RawSearchHit) -> f64 {
        let recency_score = self.compute_recency_score(&hit.modified_at);
        let frequency_score = self.compute_frequency_score(hit.access_count);

        self.semantic_weight * hit.semantic_score
            + self.recency_weight * recency_score
            + self.frequency_weight * frequency_score
    }

    /// Punteggio di recenza: più il file è recente, più alto il punteggio.
    fn compute_recency_score(&self, modified_at: &chrono::DateTime<Utc>) -> f64 {
        let age_days = (Utc::now() - *modified_at).num_days() as f64;
        // Decadimento esponenziale: dimezza ogni 30 giorni
        (-age_days / 30.0).exp()
    }

    /// Punteggio di frequenza: normalizzato logaritmicamente.
    fn compute_frequency_score(&self, access_count: u32) -> f64 {
        if access_count == 0 {
            0.0
        } else {
            (access_count as f64).ln() / 10.0_f64.ln() // Normalizzato su log10
        }
    }

    /// Ricerca completa: query → embedding → storage → ranking → risultati.
    pub async fn search(
        &self,
        params: &SearchParams,
        storage: &Storage,
        embedder: &Embedder,
    ) -> Result<Vec<SearchResult>> {
        // 1. Genera embedding della query
        let query_vector = embedder.embed(&params.query).await?;

        // 2. Cerca nel database (sovra-campiona per permettere il filtraggio)
        let overfetch = params.max_results * 3;
        let candidates = storage.cosine_search(&query_vector, overfetch)?;

        // 3. Filtra per tipo file e cartella
        let filtered: Vec<StorageSearchHit> = candidates.into_iter()
            .filter(|hit| {
                if let Some(ref ft) = params.file_type {
                    if !hit.mime_type.contains(ft) {
                        return false;
                    }
                }
                if let Some(ref folder) = params.folder {
                    if !hit.file_path.starts_with(folder) {
                        return false;
                    }
                }
                true
            })
            .collect();

        // 4. Converti in RawSearchHit per il ranking
        let raw_hits: Vec<RawSearchHit> = filtered.into_iter()
            .map(|h| RawSearchHit {
                path: h.file_path,
                name: h.file_name,
                snippet: h.chunk_content,
                semantic_score: h.cosine_score,
                file_type: h.mime_type,
                modified_at: h.modified_at,
                access_count: h.access_count,
            })
            .collect();

        // 5. Ranking finale (semantico + recenza + frequenza)
        let results = self.rank_results(raw_hits, params.max_results);

        // 6. Incrementa contatore accesso per il primo risultato
        if let Some(first) = results.first() {
            if let Err(e) = storage.increment_access_count(&first.path) {
                warn!(error = %e, "Errore aggiornamento contatore accessi");
            }
        }

        Ok(results)
    }

    /// Classifica i risultati e applica il ranking finale.
    pub fn rank_results(&self, hits: Vec<RawSearchHit>, max_results: usize) -> Vec<SearchResult> {
        let mut scored: Vec<(f64, RawSearchHit)> = hits.into_iter()
            .map(|hit| {
                let score = self.compute_score(&hit);
                (score, hit)
            })
            .collect();

        // Ordina per punteggio decrescente
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        scored.into_iter()
            .take(max_results)
            .map(|(score, hit)| {
                debug!(path = %hit.path, score = score, "Risultato classificato");
                SearchResult {
                    path: hit.path,
                    name: hit.name,
                    snippet: hit.snippet,
                    score,
                    file_type: hit.file_type,
                    modified_at: hit.modified_at,
                }
            })
            .collect()
    }
}

impl Default for SearchEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn make_hit(semantic_score: f64, days_old: i64, access_count: u32) -> RawSearchHit {
        RawSearchHit {
            path: "/test/file.txt".to_string(),
            name: "file.txt".to_string(),
            snippet: "contenuto test".to_string(),
            semantic_score,
            file_type: "txt".to_string(),
            modified_at: Utc::now() - Duration::days(days_old),
            access_count,
        }
    }

    #[test]
    fn test_recency_score() {
        let engine = SearchEngine::new();

        // File appena modificato → punteggio alto
        let recent = engine.compute_recency_score(&Utc::now());
        assert!(recent > 0.9);

        // File vecchio di 30 giorni → dimezzato
        let old = engine.compute_recency_score(&(Utc::now() - Duration::days(30)));
        assert!(old < 0.5);
        assert!(old > 0.3);
    }

    #[test]
    fn test_frequency_score() {
        let engine = SearchEngine::new();

        assert_eq!(engine.compute_frequency_score(0), 0.0);
        assert!(engine.compute_frequency_score(10) > engine.compute_frequency_score(1));
    }

    #[test]
    fn test_rank_results() {
        let engine = SearchEngine::new();

        let hits = vec![
            make_hit(0.5, 1, 5),    // Medio, recente, usato
            make_hit(0.9, 100, 0),   // Alto semantico, vecchio, mai usato
            make_hit(0.8, 1, 10),    // Alto, recente, molto usato
        ];

        let results = engine.rank_results(hits, 3);
        assert_eq!(results.len(), 3);

        // Il terzo hit dovrebbe essere primo (alto semantico + recente + usato)
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn test_rank_results_limit() {
        let engine = SearchEngine::new();
        let hits: Vec<RawSearchHit> = (0..10)
            .map(|i| make_hit(i as f64 / 10.0, 1, 1))
            .collect();

        let results = engine.rank_results(hits, 3);
        assert_eq!(results.len(), 3);
    }
}
