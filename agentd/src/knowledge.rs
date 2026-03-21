//! Knowledge Graph + Auto-RAG — grafo di conoscenza locale con ricerca automatica.
//!
//! Estrae entità (persone, progetti, aziende, argomenti) dai file e documenti
//! che l'agente elabora, e le organizza in un grafo con relazioni.
//!
//! Ogni volta che l'utente interagisce con l'agente, il grafo viene consultato
//! automaticamente per arricchire il contesto (Auto-RAG).
//!
//! Persistenza: ~/.agentos/knowledge.json

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, warn};

// ============================================================
// Tipi
// ============================================================

/// Tipo di entità riconosciuta nel testo.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityType {
    /// Persona (da email, @menzioni, nomi capitalizzati)
    Person,
    /// Progetto (parole dopo "progetto", "project")
    Project,
    /// Azienda o cliente (parole dopo "cliente", "client", "azienda")
    Company,
    /// Argomento generico (parole ricorrenti capitalizzate)
    Topic,
}

/// Una singola entità nel grafo di conoscenza.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// Nome normalizzato dell'entità (lowercase per matching)
    pub name: String,
    /// Nome originale (come trovato nel testo, con capitalizzazione)
    pub display_name: String,
    /// Tipo di entità
    pub entity_type: EntityType,
    /// Menzioni: dove questa entità appare
    pub mentions: Vec<Mention>,
    /// Ultima volta che l'entità è stata vista
    pub last_seen: DateTime<Utc>,
}

/// Una menzione di un'entità in un documento.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mention {
    /// File sorgente (o "chat" per interazioni dirette)
    pub source_file: String,
    /// Snippet di testo circostante
    pub snippet: String,
    /// Quando è stata trovata
    pub timestamp: DateTime<Utc>,
}

/// Relazione tra due entità.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    /// Entità sorgente (nome normalizzato)
    pub from_entity: String,
    /// Entità destinazione (nome normalizzato)
    pub to_entity: String,
    /// Tipo di relazione (es. "lavora_su", "menzionato_con", "invia_email_a")
    pub relation_type: String,
    /// Documento sorgente della relazione
    pub source: String,
}

/// Evento nella timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEvent {
    /// Descrizione dell'evento
    pub description: String,
    /// Entità coinvolte (nomi normalizzati)
    pub entities: Vec<String>,
    /// Quando è successo
    pub timestamp: DateTime<Utc>,
    /// Sorgente dell'evento
    pub source: String,
}

/// Contesto rilevante restituito da una query.
#[derive(Debug, Clone)]
pub struct RelevantContext {
    /// Nome dell'entità trovata
    pub entity_name: String,
    /// Tipo di entità
    pub entity_type: EntityType,
    /// Snippet più rilevante
    pub snippet: String,
    /// File sorgente
    pub source_file: String,
    /// Punteggio di rilevanza (0.0-1.0)
    pub relevance: f64,
}

// ============================================================
// Knowledge Graph
// ============================================================

/// Grafo di conoscenza locale con entità, relazioni e timeline.
#[derive(Debug, Serialize, Deserialize)]
pub struct KnowledgeGraph {
    /// Mappa nome_normalizzato → Entity
    pub entities: HashMap<String, Entity>,
    /// Relazioni tra entità
    pub relations: Vec<Relation>,
    /// Timeline degli eventi
    pub timeline: Vec<TimelineEvent>,
}

/// Percorso del file di persistenza.
fn knowledge_path() -> std::path::PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    home.join(".agentos").join("knowledge.json")
}

impl KnowledgeGraph {
    /// Crea un nuovo grafo vuoto.
    pub fn new() -> Self {
        Self {
            entities: HashMap::new(),
            relations: Vec::new(),
            timeline: Vec::new(),
        }
    }

    /// Carica il grafo dal disco, o crea uno nuovo se non esiste.
    pub fn load() -> Self {
        let path = knowledge_path();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(json) => match serde_json::from_str(&json) {
                    Ok(graph) => {
                        debug!("Knowledge graph caricato da {:?}", path);
                        return graph;
                    }
                    Err(e) => {
                        warn!("Errore parsing knowledge graph: {} — creo uno nuovo", e);
                    }
                },
                Err(e) => {
                    warn!("Errore lettura knowledge graph: {} — creo uno nuovo", e);
                }
            }
        }
        Self::new()
    }

    /// Salva il grafo su disco.
    pub fn save(&self) {
        let path = knowledge_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    warn!("Errore salvataggio knowledge graph: {}", e);
                }
            }
            Err(e) => {
                warn!("Errore serializzazione knowledge graph: {}", e);
            }
        }
    }

    /// Aggiunge un documento al grafo: estrae le entità e le relazioni.
    pub fn add_document(&mut self, path: &str, content: &str) {
        let entities = extract_entities(content);
        let now = Utc::now();

        // Per ogni entità trovata, aggiorna il grafo
        let entity_names: Vec<String> = entities.iter().map(|(name, _, _)| name.clone()).collect();

        for (name, display_name, entity_type) in &entities {
            let normalized = name.to_lowercase();

            // Crea lo snippet (primi 200 caratteri del contesto)
            let snippet = find_snippet(content, display_name, 200);

            let mention = Mention {
                source_file: path.to_string(),
                snippet,
                timestamp: now,
            };

            // Aggiorna o crea l'entità
            let entry = self.entities.entry(normalized.clone()).or_insert_with(|| Entity {
                name: normalized.clone(),
                display_name: display_name.clone(),
                entity_type: entity_type.clone(),
                mentions: Vec::new(),
                last_seen: now,
            });

            // Limita le menzioni a 50 per entità (mantieni le più recenti)
            if entry.mentions.len() >= 50 {
                entry.mentions.remove(0);
            }
            entry.mentions.push(mention);
            entry.last_seen = now;
        }

        // Crea relazioni tra entità che appaiono nello stesso documento
        for i in 0..entity_names.len() {
            for j in (i + 1)..entity_names.len() {
                let from = entity_names[i].to_lowercase();
                let to = entity_names[j].to_lowercase();

                // Evita duplicati: controlla se la relazione esiste già per questo source
                let exists = self.relations.iter().any(|r| {
                    r.source == path
                        && ((r.from_entity == from && r.to_entity == to)
                            || (r.from_entity == to && r.to_entity == from))
                });

                if !exists {
                    self.relations.push(Relation {
                        from_entity: from,
                        to_entity: to,
                        relation_type: "menzionato_con".to_string(),
                        source: path.to_string(),
                    });
                }
            }
        }

        // Limita relazioni a 500 totali
        if self.relations.len() > 500 {
            self.relations = self.relations.split_off(self.relations.len() - 500);
        }

        // Aggiungi evento timeline
        if !entity_names.is_empty() {
            // Limita timeline a 200 eventi
            if self.timeline.len() >= 200 {
                self.timeline.remove(0);
            }
            self.timeline.push(TimelineEvent {
                description: format!("Documento analizzato: {}", path),
                entities: entity_names.iter().map(|n| n.to_lowercase()).collect(),
                timestamp: now,
                source: path.to_string(),
            });
        }
    }

    /// Interroga il grafo con il testo dell'utente.
    /// Restituisce contesti rilevanti ordinati per rilevanza.
    pub fn query(&self, text: &str) -> Vec<RelevantContext> {
        if self.entities.is_empty() {
            return vec![];
        }

        let text_lower = text.to_lowercase();
        let words: Vec<&str> = text_lower.split_whitespace().collect();
        let mut results: Vec<RelevantContext> = Vec::new();

        for (normalized_name, entity) in &self.entities {
            // Calcola la rilevanza: match esatto del nome, match parziale, match su parole
            let mut relevance = 0.0;

            // Match esatto del nome nel testo
            if text_lower.contains(normalized_name) {
                relevance += 0.8;
            }

            // Match su parole individuali del nome dell'entità
            let name_words: Vec<&str> = normalized_name.split_whitespace().collect();
            for name_word in &name_words {
                if name_word.len() >= 3 && words.contains(name_word) {
                    relevance += 0.4 / name_words.len() as f64;
                }
            }

            // Boost per entità viste di recente
            let hours_ago = (Utc::now() - entity.last_seen).num_hours() as f64;
            if hours_ago < 24.0 {
                relevance += 0.1;
            }

            // Boost per entità con molte menzioni
            if entity.mentions.len() > 3 {
                relevance += 0.05;
            }

            // Aggiungi solo se rilevanza significativa (> 0.3) e non è solo chat
            if relevance > 0.3 {
                // Prendi lo snippet più recente da un file reale (non chat)
                let best_mention = entity.mentions.iter().rev()
                    .find(|m| m.source_file != "chat")
                    .or_else(|| entity.mentions.last());
                let (snippet, source) = match best_mention {
                    Some(m) => (m.snippet.clone(), m.source_file.clone()),
                    None => (String::new(), String::new()),
                };

                results.push(RelevantContext {
                    entity_name: entity.display_name.clone(),
                    entity_type: entity.entity_type.clone(),
                    snippet,
                    source_file: source,
                    relevance: relevance.min(1.0),
                });
            }
        }

        // Cerca anche nelle relazioni per entità correlate
        let matched_entities: Vec<String> = results.iter().map(|r| r.entity_name.to_lowercase()).collect();
        for entity_name in &matched_entities {
            for relation in &self.relations {
                let related = if &relation.from_entity == entity_name {
                    Some(&relation.to_entity)
                } else if &relation.to_entity == entity_name {
                    Some(&relation.from_entity)
                } else {
                    None
                };

                if let Some(related_name) = related {
                    // Aggiungi l'entità correlata con rilevanza ridotta (se non già presente)
                    if !results.iter().any(|r| r.entity_name.to_lowercase() == *related_name) {
                        if let Some(related_entity) = self.entities.get(related_name) {
                            let latest = related_entity.mentions.last();
                            results.push(RelevantContext {
                                entity_name: related_entity.display_name.clone(),
                                entity_type: related_entity.entity_type.clone(),
                                snippet: latest.map(|m| m.snippet.clone()).unwrap_or_default(),
                                source_file: latest.map(|m| m.source_file.clone()).unwrap_or_default(),
                                relevance: 0.2,
                            });
                        }
                    }
                }
            }
        }

        // Ordina per rilevanza decrescente e limita a 3 risultati (per non sovraccaricare il prompt)
        results.sort_by(|a, b| b.relevance.partial_cmp(&a.relevance).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(3);

        results
    }

    /// Formatta il contesto rilevante come stringa per il prompt LLM.
    /// Restituisce una stringa vuota se non c'è contesto rilevante.
    pub fn format_context(&self, text: &str) -> String {
        let contexts = self.query(text);
        if contexts.is_empty() {
            return String::new();
        }

        let mut parts = Vec::new();
        parts.push("CONTESTO DAI TUOI FILE LOCALI:".to_string());

        for ctx in &contexts {
            let type_label = match ctx.entity_type {
                EntityType::Person => "persona",
                EntityType::Project => "progetto",
                EntityType::Company => "azienda",
                EntityType::Topic => "argomento",
            };

            let source = if ctx.source_file.is_empty() || ctx.source_file == "chat" {
                String::new()
            } else {
                format!(" [{}]", ctx.source_file)
            };

            let snippet = if ctx.snippet.is_empty() {
                String::new()
            } else {
                format!(" — {}", ctx.snippet)
            };

            parts.push(format!(
                "- {} ({}){}{} [{:.0}%]",
                ctx.entity_name,
                type_label,
                source,
                snippet,
                ctx.relevance * 100.0
            ));
        }

        let result = parts.join("\n");
        // Limita il contesto a 2000 caratteri — seleziona solo le info più rilevanti
        if result.len() > 2000 {
            let mut end = 2000;
            while end > 0 && !result.is_char_boundary(end) { end -= 1; }
            // Taglia all'ultima riga completa
            if let Some(last_nl) = result[..end].rfind('\n') { end = last_nl; }
            format!("{}\n[...altri risultati omessi]", &result[..end])
        } else {
            result
        }
    }

    /// Numero totale di entità nel grafo.
    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    /// Numero totale di relazioni nel grafo.
    pub fn relation_count(&self) -> usize {
        self.relations.len()
    }
}

// ============================================================
// Estrazione entità (NER semplice)
// ============================================================

/// Estrae entità dal testo con NER regola-based.
/// Restituisce vettore di (nome_normalizzato, nome_display, tipo).
fn extract_entities(text: &str) -> Vec<(String, String, EntityType)> {
    let mut entities: Vec<(String, String, EntityType)> = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // 1. Email → Person
    for word in text.split_whitespace() {
        let clean = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '@' && c != '.' && c != '_' && c != '-');
        if clean.contains('@') && clean.contains('.') && clean.len() > 5 {
            let name = clean.to_lowercase();
            if !seen.contains(&name) {
                seen.insert(name.clone());
                entities.push((name, clean.to_string(), EntityType::Person));
            }
        }
    }

    // 2. @menzioni → Person
    for word in text.split_whitespace() {
        if word.starts_with('@') && word.len() > 2 {
            let name = word[1..].trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
            if name.len() >= 2 {
                let normalized = name.to_lowercase();
                if !seen.contains(&normalized) {
                    seen.insert(normalized.clone());
                    entities.push((normalized, name.to_string(), EntityType::Person));
                }
            }
        }
    }

    // 3. Parole dopo keyword "progetto/project" → Project
    let project_keywords = ["progetto", "project", "repo", "repository"];
    let text_lower = text.to_lowercase();
    for keyword in &project_keywords {
        for (idx, _) in text_lower.match_indices(keyword) {
            let after = &text[idx + keyword.len()..];
            if let Some(name) = extract_name_after(after) {
                let normalized = name.to_lowercase();
                if !seen.contains(&normalized) && normalized.len() >= 2 {
                    seen.insert(normalized.clone());
                    entities.push((normalized, name, EntityType::Project));
                }
            }
        }
    }

    // 4. Parole dopo keyword "cliente/client/azienda/company" → Company
    let company_keywords = ["cliente", "client", "azienda", "company", "società", "ditta"];
    for keyword in &company_keywords {
        for (idx, _) in text_lower.match_indices(keyword) {
            let after = &text[idx + keyword.len()..];
            if let Some(name) = extract_name_after(after) {
                let normalized = name.to_lowercase();
                if !seen.contains(&normalized) && normalized.len() >= 2 {
                    seen.insert(normalized.clone());
                    entities.push((normalized, name, EntityType::Company));
                }
            }
        }
    }

    // 5. Parole capitalizzate ricorrenti → Topic (solo se appaiono 2+ volte)
    let mut capitalized_counts: HashMap<String, (String, usize)> = HashMap::new();
    // Stopwords italiane e inglesi da escludere
    let stopwords: std::collections::HashSet<&str> = [
        "il", "la", "le", "lo", "i", "gli", "un", "una", "uno", "di", "da", "in", "con",
        "su", "per", "tra", "fra", "che", "non", "del", "della", "delle", "dei", "degli",
        "al", "alla", "alle", "ai", "agli", "dal", "dalla", "dalle", "dai", "dagli",
        "nel", "nella", "nelle", "nei", "negli", "sul", "sulla", "sulle", "sui", "sugli",
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being", "have", "has",
        "had", "do", "does", "did", "will", "would", "shall", "should", "may", "might",
        "can", "could", "and", "but", "or", "nor", "not", "so", "yet", "for", "with",
        "from", "to", "of", "at", "by", "on", "in", "up", "out", "as", "if", "it",
        "its", "this", "that", "these", "those", "my", "your", "his", "her", "our", "their",
        // Parole comuni nei testi tecnici
        "todo", "nota", "note", "file", "http", "https", "www", "com", "org",
        "json", "html", "css", "null", "true", "false", "none",
        // Mesi e giorni
        "gennaio", "febbraio", "marzo", "aprile", "maggio", "giugno",
        "luglio", "agosto", "settembre", "ottobre", "novembre", "dicembre",
        "lunedì", "martedì", "mercoledì", "giovedì", "venerdì", "sabato", "domenica",
    ].iter().cloned().collect();

    for word in text.split(|c: char| !c.is_alphanumeric() && c != '\'' && c != '-') {
        let trimmed = word.trim();
        if trimmed.len() < 3 || trimmed.len() > 30 {
            continue;
        }
        // Deve iniziare con una maiuscola e non essere tutto maiuscolo
        let first = trimmed.chars().next().unwrap_or(' ');
        if first.is_uppercase() && !trimmed.chars().all(char::is_uppercase) {
            let lower = trimmed.to_lowercase();
            if !stopwords.contains(lower.as_str()) && !seen.contains(&lower) {
                capitalized_counts
                    .entry(lower.clone())
                    .and_modify(|(_, count)| *count += 1)
                    .or_insert((trimmed.to_string(), 1));
            }
        }
    }

    // Aggiungi solo parole capitalizzate che appaiono 2+ volte
    for (normalized, (display, count)) in &capitalized_counts {
        if *count >= 2 && !seen.contains(normalized) {
            seen.insert(normalized.clone());
            entities.push((normalized.clone(), display.clone(), EntityType::Topic));
        }
    }

    entities
}

/// Estrae il nome dopo una keyword (es. "progetto Alpha" → "Alpha").
/// Prende la prima parola capitalizzata o la prossima parola significativa.
fn extract_name_after(text: &str) -> Option<String> {
    let trimmed = text.trim_start();

    // Salta separatori come ":", " - ", ecc.
    let clean = trimmed.trim_start_matches(|c: char| c == ':' || c == '-' || c == '=' || c.is_whitespace());

    // Prendi le prime 1-3 parole che iniziano con maiuscola (o la prima parola significativa)
    let mut name_parts = Vec::new();
    for word in clean.split_whitespace().take(3) {
        let w = word.trim_matches(|c: char| !c.is_alphanumeric());
        if w.is_empty() {
            break;
        }
        // Se inizia con maiuscola o è la prima parola, aggiungila
        if w.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) || name_parts.is_empty() {
            name_parts.push(w.to_string());
        } else {
            break; // Stop alla prima parola non capitalizzata
        }
    }

    if name_parts.is_empty() {
        None
    } else {
        Some(name_parts.join(" "))
    }
}

/// Trova uno snippet di testo attorno alla prima occorrenza di `target`.
fn find_snippet(text: &str, target: &str, max_len: usize) -> String {
    let lower_text = text.to_lowercase();
    let lower_target = target.to_lowercase();

    if let Some(pos) = lower_text.find(&lower_target) {
        let start = if pos > max_len / 2 { pos - max_len / 2 } else { 0 };
        let end = (pos + target.len() + max_len / 2).min(text.len());

        // Allinea a bordi di carattere UTF-8 validi
        let mut start = start;
        while start > 0 && !text.is_char_boundary(start) { start -= 1; }
        let mut end = end.min(text.len());
        while end < text.len() && !text.is_char_boundary(end) { end += 1; }
        let end = end.min(text.len());
        let slice = &text[start..end];
        let clean = slice.replace('\n', " ").replace('\r', "");
        let trimmed = clean.trim();

        if start > 0 {
            format!("...{}", trimmed)
        } else {
            trimmed.to_string()
        }
    } else {
        // Prendi i primi max_len caratteri
        text.chars().take(max_len).collect::<String>().trim().to_string()
    }
}

// ============================================================
// Test
// ============================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_entities_email() {
        let text = "Contatta mario.rossi@example.com per il progetto Alpha";
        let entities = extract_entities(text);
        assert!(entities.iter().any(|(_, _, t)| *t == EntityType::Person));
        assert!(entities.iter().any(|(n, _, _)| n.contains("mario.rossi")));
    }

    #[test]
    fn test_extract_entities_mention() {
        let text = "Messaggio da @giovanni per il team";
        let entities = extract_entities(text);
        assert!(entities.iter().any(|(n, _, _)| n == "giovanni"));
    }

    #[test]
    fn test_extract_entities_project() {
        let text = "Il progetto Alpha è quasi completato. Riguardo al progetto Alpha...";
        let entities = extract_entities(text);
        assert!(entities.iter().any(|(_, _, t)| *t == EntityType::Project));
    }

    #[test]
    fn test_extract_entities_company() {
        let text = "Il cliente Acme Corp ha richiesto un preventivo";
        let entities = extract_entities(text);
        assert!(entities.iter().any(|(_, _, t)| *t == EntityType::Company));
    }

    #[test]
    fn test_extract_entities_topic() {
        let text = "Rust è un linguaggio di programmazione. Rust offre sicurezza della memoria. Rust è veloce.";
        let entities = extract_entities(text);
        assert!(entities.iter().any(|(n, _, t)| n == "rust" && *t == EntityType::Topic));
    }

    #[test]
    fn test_extract_name_after() {
        assert_eq!(extract_name_after(" Alpha Beta"), Some("Alpha Beta".to_string()));
        assert_eq!(extract_name_after(": Gamma"), Some("Gamma".to_string()));
        assert_eq!(extract_name_after(""), None);
    }

    #[test]
    fn test_find_snippet() {
        let text = "Lorem ipsum dolor sit amet, Alpha consectetur adipiscing elit";
        let snippet = find_snippet(text, "Alpha", 40);
        assert!(snippet.contains("Alpha"));
    }

    #[test]
    fn test_knowledge_graph_add_and_query() {
        let mut graph = KnowledgeGraph::new();

        graph.add_document(
            "/tmp/test.txt",
            "Il progetto Alpha è gestito da mario@example.com. Il cliente Acme ha richiesto modifiche.",
        );

        assert!(graph.entity_count() > 0);

        // Cerca "Alpha" — deve trovare l'entità progetto
        let results = graph.query("Come va il progetto Alpha?");
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.entity_name.to_lowercase().contains("alpha")));
    }

    #[test]
    fn test_knowledge_graph_relations() {
        let mut graph = KnowledgeGraph::new();

        graph.add_document(
            "/tmp/doc.txt",
            "mario@example.com lavora sul progetto Beta per il cliente Delta Corp",
        );

        // Devono esserci relazioni tra le entità nello stesso documento
        assert!(graph.relation_count() > 0);
    }

    #[test]
    fn test_knowledge_graph_format_context() {
        let mut graph = KnowledgeGraph::new();

        graph.add_document(
            "/tmp/notes.txt",
            "Il progetto Gamma è quasi finito. Il progetto Gamma usa Rust.",
        );

        let context = graph.format_context("stato del progetto Gamma");
        assert!(context.contains("CONTESTO DAI TUOI FILE LOCALI"));
    }

    #[test]
    fn test_knowledge_graph_empty_query() {
        let graph = KnowledgeGraph::new();
        let results = graph.query("qualsiasi cosa");
        assert!(results.is_empty());
    }

    #[test]
    fn test_knowledge_graph_serialization() {
        let mut graph = KnowledgeGraph::new();
        graph.add_document("/tmp/test.txt", "Il progetto Alpha è importante");

        let json = serde_json::to_string(&graph).unwrap();
        let restored: KnowledgeGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.entity_count(), graph.entity_count());
    }

    #[test]
    fn test_extract_entities_no_entities() {
        let text = "ciao come stai oggi";
        let entities = extract_entities(text);
        // Nessuna entità da testo tutto minuscolo senza keyword
        assert!(entities.is_empty());
    }

    #[test]
    fn test_knowledge_graph_dedup_relations() {
        let mut graph = KnowledgeGraph::new();

        // Aggiungi lo stesso documento due volte
        graph.add_document("/tmp/doc.txt", "mario@test.com lavora sul progetto Alpha");
        let count1 = graph.relation_count();

        graph.add_document("/tmp/doc.txt", "mario@test.com lavora sul progetto Alpha");
        let count2 = graph.relation_count();

        // Le relazioni dalla stessa sorgente non devono essere duplicate
        assert_eq!(count1, count2);
    }

    #[test]
    fn test_format_context_empty_graph() {
        let graph = KnowledgeGraph::new();
        let context = graph.format_context("qualsiasi query");
        assert!(context.is_empty());
    }
}
