//! Audit Trail — log append-only di tutte le azioni eseguite.
//!
//! Registra ogni comando con zona di rischio, approvazione, risultato
//! e spiegazione dell'agente. Esportabile in JSON.

use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use agentos_common::types::RiskZone;

/// Entry nel log di audit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: i64,
    pub timestamp: DateTime<Utc>,
    pub command: String,
    pub zone: RiskZone,
    pub approved: bool,
    pub result_code: i32,
    pub agent_explanation: String,
}

/// Audit Trail — log immutabile delle azioni.
pub struct AuditTrail {
    db: Connection,
}

impl AuditTrail {
    /// Crea un nuovo AuditTrail con database SQLite al percorso specificato.
    ///
    /// Dopo la creazione, imposta permessi 600 sul file .db per proteggere i log.
    ///
    /// TODO: Integrare SQLCipher per crittografia trasparente del database audit.
    /// L'audit trail contiene comandi eseguiti e spiegazioni — dati potenzialmente sensibili.
    /// Con SQLCipher, aggiungere `PRAGMA key = '...'` subito dopo `Connection::open()`.
    pub fn new(db_path: &str) -> Result<Self> {
        let db = Connection::open(db_path)?;

        // Imposta permessi sicuri sul file database (solo su file reali, non :memory:)
        if db_path != ":memory:" {
            crate::crypto::ensure_secure_permissions(db_path);
        }

        db.execute_batch("
            CREATE TABLE IF NOT EXISTS audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                command TEXT NOT NULL,
                zone TEXT NOT NULL,
                approved INTEGER NOT NULL,
                result_code INTEGER NOT NULL DEFAULT 0,
                agent_explanation TEXT NOT NULL DEFAULT ''
            );

            CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_log(timestamp);
            CREATE INDEX IF NOT EXISTS idx_audit_zone ON audit_log(zone);
        ")?;

        info!(db_path = db_path, "Audit Trail inizializzato");
        Ok(Self { db })
    }

    /// Crea un AuditTrail in memoria (per i test).
    pub fn in_memory() -> Result<Self> {
        Self::new(":memory:")
    }

    /// Registra un'azione nel log di audit.
    pub fn log_action(
        &self,
        command: &str,
        zone: RiskZone,
        approved: bool,
        result_code: i32,
        agent_explanation: &str,
    ) -> Result<i64> {
        let now = Utc::now().to_rfc3339();
        let zone_str = serde_json::to_string(&zone)?;

        self.db.execute(
            "INSERT INTO audit_log (timestamp, command, zone, approved, result_code, agent_explanation) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![now, command, zone_str, approved as i32, result_code, agent_explanation],
        )?;

        let id = self.db.last_insert_rowid();
        debug!(id = id, command = command, zone = ?zone, "Azione registrata nell'audit");
        Ok(id)
    }

    /// Recupera le ultime N entry dall'audit.
    pub fn get_recent(&self, n: usize) -> Result<Vec<AuditEntry>> {
        let mut stmt = self.db.prepare(
            "SELECT id, timestamp, command, zone, approved, result_code, agent_explanation FROM audit_log ORDER BY id DESC LIMIT ?1"
        )?;

        let entries = stmt.query_map(params![n as i64], |row| {
            let timestamp_str: String = row.get(1)?;
            let zone_str: String = row.get(3)?;
            let approved_int: i32 = row.get(4)?;

            Ok(AuditEntry {
                id: row.get(0)?,
                timestamp: DateTime::parse_from_rfc3339(&timestamp_str)
                    .unwrap_or_else(|_| Utc::now().into())
                    .with_timezone(&Utc),
                command: row.get(2)?,
                zone: serde_json::from_str(&zone_str).unwrap_or(RiskZone::Green),
                approved: approved_int != 0,
                result_code: row.get(5)?,
                agent_explanation: row.get(6)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

        Ok(entries)
    }

    /// Esporta l'intero audit log in formato JSON.
    pub fn export_json(&self, path: &str) -> Result<usize> {
        let mut stmt = self.db.prepare(
            "SELECT id, timestamp, command, zone, approved, result_code, agent_explanation FROM audit_log ORDER BY id ASC"
        )?;

        let entries: Vec<AuditEntry> = stmt.query_map([], |row| {
            let timestamp_str: String = row.get(1)?;
            let zone_str: String = row.get(3)?;
            let approved_int: i32 = row.get(4)?;

            Ok(AuditEntry {
                id: row.get(0)?,
                timestamp: DateTime::parse_from_rfc3339(&timestamp_str)
                    .unwrap_or_else(|_| Utc::now().into())
                    .with_timezone(&Utc),
                command: row.get(2)?,
                zone: serde_json::from_str(&zone_str).unwrap_or(RiskZone::Green),
                approved: approved_int != 0,
                result_code: row.get(5)?,
                agent_explanation: row.get(6)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

        let count = entries.len();
        let json = serde_json::to_string_pretty(&entries)?;
        std::fs::write(path, json)?;

        info!(path = path, count = count, "Audit esportato");
        Ok(count)
    }

    /// Conta le entry totali nell'audit.
    pub fn count(&self) -> Result<i64> {
        self.db.query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0))
            .map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_action() {
        let audit = AuditTrail::in_memory().unwrap();

        let id = audit.log_action(
            "ls -la",
            RiskZone::Green,
            true,
            0,
            "Elenco file nella directory",
        ).unwrap();

        assert!(id > 0);
        assert_eq!(audit.count().unwrap(), 1);
    }

    #[test]
    fn test_get_recent() {
        let audit = AuditTrail::in_memory().unwrap();

        audit.log_action("ls", RiskZone::Green, true, 0, "").unwrap();
        audit.log_action("sudo apt install vim", RiskZone::Yellow, true, 0, "Installazione pacchetto").unwrap();
        audit.log_action("rm -rf /", RiskZone::Red, false, -1, "BLOCCATO").unwrap();

        let recent = audit.get_recent(10).unwrap();
        assert_eq!(recent.len(), 3);

        // Ordinati per id DESC
        assert_eq!(recent[0].zone, RiskZone::Red);
        assert!(!recent[0].approved);
        assert_eq!(recent[1].zone, RiskZone::Yellow);
        assert!(recent[1].approved);
    }

    #[test]
    fn test_export_json() {
        let audit = AuditTrail::in_memory().unwrap();

        audit.log_action("ls", RiskZone::Green, true, 0, "").unwrap();
        audit.log_action("pwd", RiskZone::Green, true, 0, "").unwrap();

        let dir = tempfile::TempDir::new().unwrap();
        let export_path = dir.path().join("audit.json");

        let count = audit.export_json(export_path.to_str().unwrap()).unwrap();
        assert_eq!(count, 2);

        // Verifica che il file JSON sia valido
        let content = std::fs::read_to_string(&export_path).unwrap();
        let entries: Vec<AuditEntry> = serde_json::from_str(&content).unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn test_count() {
        let audit = AuditTrail::in_memory().unwrap();
        assert_eq!(audit.count().unwrap(), 0);

        audit.log_action("ls", RiskZone::Green, true, 0, "").unwrap();
        assert_eq!(audit.count().unwrap(), 1);
    }
}
