//! Memory Manager — tre livelli di memoria per l'agente.
//!
//! 1. Working memory: HashMap in RAM (contesto sessione corrente)
//! 2. Episodic memory: SQLite (interazioni passate, retention 90 giorni)
//! 3. Semantic memory: SQLite (pattern e routine apprese)

use anyhow::Result;
use chrono::{DateTime, Utc, Duration as ChronoDuration};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info};

/// Interazione salvata nella memoria episodica.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interaction {
    pub id: i64,
    pub timestamp: DateTime<Utc>,
    pub user_input: String,
    pub agent_response: String,
    pub commands: Vec<String>,
    pub success: bool,
}

/// Pattern appreso nella memoria semantica.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    pub id: i64,
    pub pattern_type: String,  // "routine", "preference", "shortcut"
    pub pattern_data: String,  // JSON con i dettagli del pattern
    pub weight: f64,           // Quanto è forte questo pattern (0.0 - 1.0)
    pub last_seen: DateTime<Utc>,
}

/// Memory Manager con tre livelli.
pub struct MemoryManager {
    /// Working memory — contesto sessione corrente (in RAM)
    working: HashMap<String, String>,
    /// Connessione SQLite per memoria episodica e semantica
    db: Connection,
}

impl MemoryManager {
    /// Crea un nuovo MemoryManager con database SQLite al percorso specificato.
    ///
    /// Dopo la creazione, imposta permessi 600 sul file .db per proteggere i dati.
    ///
    /// TODO: Integrare SQLCipher per crittografia trasparente del database.
    /// Con SQLCipher, aggiungere `PRAGMA key = '...'` subito dopo `Connection::open()`.
    pub fn new(db_path: &str) -> Result<Self> {
        let db = Connection::open(db_path)?;

        // Imposta permessi sicuri sul file database (solo su file reali, non :memory:)
        if db_path != ":memory:" {
            crate::crypto::ensure_secure_permissions(db_path);
        }

        // Crea le tabelle se non esistono
        db.execute_batch("
            CREATE TABLE IF NOT EXISTS interactions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                user_input TEXT NOT NULL,
                agent_response TEXT NOT NULL,
                commands TEXT NOT NULL DEFAULT '[]',
                success INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS patterns (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                pattern_type TEXT NOT NULL,
                pattern_data TEXT NOT NULL,
                weight REAL NOT NULL DEFAULT 0.5,
                last_seen TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_interactions_timestamp ON interactions(timestamp);
            CREATE INDEX IF NOT EXISTS idx_patterns_type ON patterns(pattern_type);
        ")?;

        info!(db_path = db_path, "Memory Manager inizializzato");

        Ok(Self {
            working: HashMap::new(),
            db,
        })
    }

    /// Crea un MemoryManager con database in memoria (per i test).
    pub fn in_memory() -> Result<Self> {
        Self::new(":memory:")
    }

    // === Working Memory (RAM) ===

    /// Salva un valore nella working memory.
    pub fn set(&mut self, key: &str, value: &str) {
        self.working.insert(key.to_string(), value.to_string());
    }

    /// Legge un valore dalla working memory.
    pub fn get(&self, key: &str) -> Option<&String> {
        self.working.get(key)
    }

    /// Rimuove un valore dalla working memory.
    pub fn remove(&mut self, key: &str) -> Option<String> {
        self.working.remove(key)
    }

    /// Pulisce tutta la working memory.
    pub fn clear_working(&mut self) {
        self.working.clear();
    }

    // === Episodic Memory (SQLite) ===

    /// Salva un'interazione nella memoria episodica.
    pub fn add_interaction(
        &self,
        user_input: &str,
        agent_response: &str,
        commands: &[String],
        success: bool,
    ) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        let commands_json = serde_json::to_string(commands)?;

        self.db.execute(
            "INSERT INTO interactions (timestamp, user_input, agent_response, commands, success) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![now, user_input, agent_response, commands_json, success as i32],
        )?;

        let id = self.db.last_insert_rowid();
        debug!(id = id, "Interazione salvata");
        Ok(id)
    }

    /// Recupera le ultime N interazioni.
    pub fn get_recent(&self, n: usize) -> Result<Vec<Interaction>> {
        let mut stmt = self.db.prepare(
            "SELECT id, timestamp, user_input, agent_response, commands, success FROM interactions ORDER BY id DESC LIMIT ?1"
        )?;

        let interactions = stmt.query_map(params![n as i64], |row| {
            let timestamp_str: String = row.get(1)?;
            let commands_json: String = row.get(4)?;
            let success_int: i32 = row.get(5)?;

            Ok(Interaction {
                id: row.get(0)?,
                timestamp: DateTime::parse_from_rfc3339(&timestamp_str)
                    .unwrap_or_else(|_| Utc::now().into())
                    .with_timezone(&Utc),
                user_input: row.get(2)?,
                agent_response: row.get(3)?,
                commands: serde_json::from_str(&commands_json).unwrap_or_default(),
                success: success_int != 0,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

        Ok(interactions)
    }

    /// Pulisce le interazioni più vecchie di N giorni.
    pub fn cleanup_old_interactions(&self, retention_days: i64) -> Result<usize> {
        let cutoff = (Utc::now() - ChronoDuration::days(retention_days)).to_rfc3339();
        let deleted = self.db.execute(
            "DELETE FROM interactions WHERE timestamp < ?1",
            params![cutoff],
        )?;
        info!(deleted = deleted, retention_days = retention_days, "Pulizia interazioni vecchie");
        Ok(deleted)
    }

    // === Semantic Memory (SQLite) ===

    /// Salva o aggiorna un pattern nella memoria semantica.
    pub fn learn_pattern(&self, pattern_type: &str, pattern_data: &str, weight: f64) -> Result<i64> {
        let now = Utc::now().to_rfc3339();

        // Cerca se il pattern esiste già (stesso tipo e dati)
        let existing: Option<i64> = self.db.query_row(
            "SELECT id FROM patterns WHERE pattern_type = ?1 AND pattern_data = ?2",
            params![pattern_type, pattern_data],
            |row| row.get(0),
        ).ok();

        if let Some(id) = existing {
            // Aggiorna il peso e la data
            self.db.execute(
                "UPDATE patterns SET weight = MIN(1.0, weight + ?1), last_seen = ?2 WHERE id = ?3",
                params![weight * 0.1, now, id],
            )?;
            debug!(id = id, "Pattern aggiornato");
            Ok(id)
        } else {
            // Crea nuovo pattern
            self.db.execute(
                "INSERT INTO patterns (pattern_type, pattern_data, weight, last_seen) VALUES (?1, ?2, ?3, ?4)",
                params![pattern_type, pattern_data, weight, now],
            )?;
            let id = self.db.last_insert_rowid();
            debug!(id = id, pattern_type = pattern_type, "Nuovo pattern appreso");
            Ok(id)
        }
    }

    /// Recupera i pattern per un dato contesto/tipo.
    pub fn get_patterns_for_context(&self, pattern_type: &str) -> Result<Vec<Pattern>> {
        let mut stmt = self.db.prepare(
            "SELECT id, pattern_type, pattern_data, weight, last_seen FROM patterns WHERE pattern_type = ?1 ORDER BY weight DESC"
        )?;

        let patterns = stmt.query_map(params![pattern_type], |row| {
            let last_seen_str: String = row.get(4)?;
            Ok(Pattern {
                id: row.get(0)?,
                pattern_type: row.get(1)?,
                pattern_data: row.get(2)?,
                weight: row.get(3)?,
                last_seen: DateTime::parse_from_rfc3339(&last_seen_str)
                    .unwrap_or_else(|_| Utc::now().into())
                    .with_timezone(&Utc),
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

        Ok(patterns)
    }

    /// Decadimento dei pattern: riduce il peso dei pattern non visti di recente.
    pub fn decay_patterns(&self, decay_factor: f64) -> Result<usize> {
        let cutoff = (Utc::now() - ChronoDuration::days(30)).to_rfc3339();
        let affected = self.db.execute(
            "UPDATE patterns SET weight = weight * ?1 WHERE last_seen < ?2",
            params![decay_factor, cutoff],
        )?;
        Ok(affected)
    }

    /// Restituisce i top N pattern per peso (i più rilevanti appresi).
    pub fn get_relevant_patterns(&self) -> Result<Vec<Pattern>> {
        let mut stmt = self.db.prepare(
            "SELECT id, pattern_type, pattern_data, weight, last_seen FROM patterns ORDER BY weight DESC LIMIT 5"
        )?;

        let patterns = stmt.query_map([], |row| {
            let last_seen_str: String = row.get(4)?;
            Ok(Pattern {
                id: row.get(0)?,
                pattern_type: row.get(1)?,
                pattern_data: row.get(2)?,
                weight: row.get(3)?,
                last_seen: DateTime::parse_from_rfc3339(&last_seen_str)
                    .unwrap_or_else(|_| Utc::now().into())
                    .with_timezone(&Utc),
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

        Ok(patterns)
    }

    /// Analizza le interazioni recenti per individuare sequenze ripetute
    /// e le salva come pattern nella memoria semantica.
    pub fn detect_patterns(&self) -> Result<()> {
        let recent = self.get_recent(20)?;
        if recent.len() < 3 {
            return Ok(());
        }

        // Cerca comandi ripetuti frequentemente
        let mut command_freq: HashMap<String, usize> = HashMap::new();
        for interaction in &recent {
            for cmd in &interaction.commands {
                *command_freq.entry(cmd.clone()).or_insert(0) += 1;
            }
        }

        // Salva come pattern le sequenze che appaiono almeno 3 volte
        for (cmd, count) in &command_freq {
            if *count >= 3 {
                let data = serde_json::json!({
                    "command": cmd,
                    "frequency": count,
                }).to_string();
                self.learn_pattern("routine", &data, 0.5)?;
                debug!(command = %cmd, count = count, "Pattern ripetuto rilevato");
            }
        }

        // Cerca coppie di input utente simili (stessa richiesta ripetuta)
        let mut input_freq: HashMap<String, usize> = HashMap::new();
        for interaction in &recent {
            let key = interaction.user_input.to_lowercase();
            *input_freq.entry(key).or_insert(0) += 1;
        }

        for (input, count) in &input_freq {
            if *count >= 2 {
                let data = serde_json::json!({
                    "user_input": input,
                    "frequency": count,
                }).to_string();
                self.learn_pattern("preference", &data, 0.3)?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_working_memory() {
        let mut mem = MemoryManager::in_memory().unwrap();

        mem.set("cwd", "/home/user");
        assert_eq!(mem.get("cwd"), Some(&"/home/user".to_string()));

        mem.set("cwd", "/tmp");
        assert_eq!(mem.get("cwd"), Some(&"/tmp".to_string()));

        mem.remove("cwd");
        assert_eq!(mem.get("cwd"), None);
    }

    #[test]
    fn test_working_memory_clear() {
        let mut mem = MemoryManager::in_memory().unwrap();
        mem.set("a", "1");
        mem.set("b", "2");
        mem.clear_working();
        assert_eq!(mem.get("a"), None);
        assert_eq!(mem.get("b"), None);
    }

    #[test]
    fn test_episodic_memory_add_and_get() {
        let mem = MemoryManager::in_memory().unwrap();

        mem.add_interaction("mostrami i file", "Ecco i file:", &["ls -la".to_string()], true).unwrap();
        mem.add_interaction("che ore sono", "Sono le 14:30", &["date".to_string()], true).unwrap();

        let recent = mem.get_recent(10).unwrap();
        assert_eq!(recent.len(), 2);
        // Ordinati per id DESC, quindi il più recente è primo
        assert_eq!(recent[0].user_input, "che ore sono");
        assert_eq!(recent[1].user_input, "mostrami i file");
    }

    #[test]
    fn test_episodic_memory_limit() {
        let mem = MemoryManager::in_memory().unwrap();

        for i in 0..10 {
            mem.add_interaction(&format!("input {}", i), &format!("output {}", i), &[], true).unwrap();
        }

        let recent = mem.get_recent(3).unwrap();
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].user_input, "input 9");
    }

    #[test]
    fn test_semantic_memory_learn_pattern() {
        let mem = MemoryManager::in_memory().unwrap();

        let id = mem.learn_pattern("routine", "mattina: git pull, cargo build", 0.5).unwrap();
        assert!(id > 0);

        let patterns = mem.get_patterns_for_context("routine").unwrap();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0].pattern_type, "routine");
    }

    #[test]
    fn test_semantic_memory_update_existing() {
        let mem = MemoryManager::in_memory().unwrap();

        let id1 = mem.learn_pattern("routine", "morning: check email", 0.5).unwrap();
        let id2 = mem.learn_pattern("routine", "morning: check email", 0.3).unwrap();

        // Deve aggiornare lo stesso record
        assert_eq!(id1, id2);

        let patterns = mem.get_patterns_for_context("routine").unwrap();
        assert_eq!(patterns.len(), 1);
        // Il peso deve essere aumentato
        assert!(patterns[0].weight > 0.5);
    }

    #[test]
    fn test_cleanup_old_interactions() {
        let mem = MemoryManager::in_memory().unwrap();

        // Inserisci un'interazione con timestamp vecchio
        let old_timestamp = (Utc::now() - ChronoDuration::days(100)).to_rfc3339();
        mem.db.execute(
            "INSERT INTO interactions (timestamp, user_input, agent_response, commands, success) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![old_timestamp, "vecchio", "risposta", "[]", 1],
        ).unwrap();

        mem.add_interaction("recente", "risposta", &[], true).unwrap();

        let deleted = mem.cleanup_old_interactions(90).unwrap();
        assert_eq!(deleted, 1);

        let recent = mem.get_recent(10).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].user_input, "recente");
    }
}
