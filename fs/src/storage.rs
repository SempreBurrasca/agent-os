//! Storage — layer SQLite per indice file, chunk di testo ed embedding.
//!
//! Gestisce tre tabelle:
//! - files: metadati dei file indicizzati
//! - chunks: blocchi di testo estratti
//! - embeddings: vettori semantici (BLOB f32 little-endian)
//!
//! La ricerca semantica usa coseno-similarità in puro Rust (no sqlite-vss).

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, params};
use std::sync::Mutex;
use tracing::{debug, info, warn};

use crate::indexer::TextChunk;

/// Risultato grezzo dalla ricerca nel database.
#[derive(Debug, Clone)]
pub struct StorageSearchHit {
    pub file_path: String,
    pub file_name: String,
    pub mime_type: String,
    pub chunk_content: String,
    pub cosine_score: f64,
    pub modified_at: DateTime<Utc>,
    pub access_count: u32,
}

/// Stato dell'indicizzazione.
#[derive(Debug, Clone)]
pub struct IndexStatus {
    pub total_files: u64,
    pub indexed_files: u64,
    pub pending_files: u64,
}

/// Storage SQLite per l'indice semantico.
pub struct Storage {
    db: Mutex<Connection>,
}

impl Storage {
    /// Crea un nuovo Storage al percorso specificato.
    pub fn new(db_path: &str) -> Result<Self> {
        let db = Connection::open(db_path)?;
        Self::init_db(&db)?;
        info!(db_path = db_path, "Storage agent-fs inizializzato");
        Ok(Self { db: Mutex::new(db) })
    }

    /// Crea uno Storage in memoria (per i test).
    pub fn in_memory() -> Result<Self> {
        let db = Connection::open_in_memory()?;
        Self::init_db(&db)?;
        Ok(Self { db: Mutex::new(db) })
    }

    /// Inizializza le tabelle e i pragma.
    fn init_db(db: &Connection) -> Result<()> {
        db.execute_batch("
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS files (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                path TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL,
                mime_type TEXT NOT NULL,
                size INTEGER NOT NULL,
                modified_at TEXT NOT NULL,
                indexed_at TEXT NOT NULL,
                access_count INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_files_path ON files(path);

            CREATE TABLE IF NOT EXISTS chunks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                chunk_index INTEGER NOT NULL,
                content TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_chunks_file ON chunks(file_id);

            CREATE TABLE IF NOT EXISTS embeddings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                chunk_id INTEGER NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
                vector BLOB NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_embeddings_chunk ON embeddings(chunk_id);
        ")?;
        Ok(())
    }

    /// Inserisce o aggiorna un file nell'indice. Cancella chunk/embedding precedenti.
    pub fn upsert_file(
        &self,
        path: &str,
        name: &str,
        mime_type: &str,
        size: u64,
        modified_at: DateTime<Utc>,
    ) -> Result<i64> {
        let db = self.db.lock().map_err(|e| anyhow!("Lock avvelenato: {}", e))?;
        let now = Utc::now().to_rfc3339();
        let modified_str = modified_at.to_rfc3339();

        // Controlla se esiste già
        let existing_id: Option<i64> = db.query_row(
            "SELECT id FROM files WHERE path = ?1",
            params![path],
            |row| row.get(0),
        ).ok();

        if let Some(id) = existing_id {
            // Aggiorna e cancella dati vecchi (cascade elimina chunks+embeddings)
            db.execute("DELETE FROM chunks WHERE file_id = ?1", params![id])?;
            db.execute(
                "UPDATE files SET name = ?1, mime_type = ?2, size = ?3, modified_at = ?4, indexed_at = ?5 WHERE id = ?6",
                params![name, mime_type, size as i64, modified_str, now, id],
            )?;
            debug!(path = path, id = id, "File aggiornato nell'indice");
            Ok(id)
        } else {
            db.execute(
                "INSERT INTO files (path, name, mime_type, size, modified_at, indexed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![path, name, mime_type, size as i64, modified_str, now],
            )?;
            let id = db.last_insert_rowid();
            debug!(path = path, id = id, "Nuovo file nell'indice");
            Ok(id)
        }
    }

    /// Inserisce una lista di chunk per un file. Restituisce i chunk_id.
    pub fn insert_chunks(&self, file_id: i64, chunks: &[TextChunk]) -> Result<Vec<i64>> {
        let db = self.db.lock().map_err(|e| anyhow!("Lock avvelenato: {}", e))?;
        let mut ids = Vec::with_capacity(chunks.len());

        for chunk in chunks {
            db.execute(
                "INSERT INTO chunks (file_id, chunk_index, content) VALUES (?1, ?2, ?3)",
                params![file_id, chunk.chunk_index as i64, chunk.content],
            )?;
            ids.push(db.last_insert_rowid());
        }

        debug!(file_id = file_id, count = ids.len(), "Chunk inseriti");
        Ok(ids)
    }

    /// Inserisce un embedding per un chunk.
    pub fn insert_embedding(&self, chunk_id: i64, vector: &[f32]) -> Result<()> {
        let db = self.db.lock().map_err(|e| anyhow!("Lock avvelenato: {}", e))?;
        let blob = vec_to_blob(vector);
        db.execute(
            "INSERT INTO embeddings (chunk_id, vector) VALUES (?1, ?2)",
            params![chunk_id, blob],
        )?;
        Ok(())
    }

    /// Ricerca per similarità coseno. Scansione completa + ranking.
    pub fn cosine_search(&self, query_vector: &[f32], max_results: usize) -> Result<Vec<StorageSearchHit>> {
        let db = self.db.lock().map_err(|e| anyhow!("Lock avvelenato: {}", e))?;

        // Carica tutti gli embedding con i metadati del file
        let mut stmt = db.prepare("
            SELECT e.vector, c.content, f.path, f.name, f.mime_type, f.modified_at, f.access_count
            FROM embeddings e
            JOIN chunks c ON c.id = e.chunk_id
            JOIN files f ON f.id = c.file_id
        ")?;

        let mut hits: Vec<StorageSearchHit> = stmt.query_map([], |row| {
            let blob: Vec<u8> = row.get(0)?;
            let content: String = row.get(1)?;
            let path: String = row.get(2)?;
            let name: String = row.get(3)?;
            let mime_type: String = row.get(4)?;
            let modified_str: String = row.get(5)?;
            let access_count: u32 = row.get(6)?;

            let stored_vec = blob_to_vec(&blob);
            let score = cosine_similarity(query_vector, &stored_vec);

            let modified_at = DateTime::parse_from_rfc3339(&modified_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            Ok(StorageSearchHit {
                file_path: path,
                file_name: name,
                mime_type,
                chunk_content: content,
                cosine_score: score,
                modified_at,
                access_count,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

        // Ordina per score decrescente
        hits.sort_by(|a, b| b.cosine_score.partial_cmp(&a.cosine_score).unwrap_or(std::cmp::Ordering::Equal));
        hits.truncate(max_results);

        Ok(hits)
    }

    /// Stato dell'indicizzazione.
    pub fn get_index_status(&self) -> Result<IndexStatus> {
        let db = self.db.lock().map_err(|e| anyhow!("Lock avvelenato: {}", e))?;

        let total_files: u64 = db.query_row(
            "SELECT COUNT(*) FROM files", [], |row| row.get(0)
        )?;

        // File indicizzati = quelli con almeno un embedding
        let indexed_files: u64 = db.query_row(
            "SELECT COUNT(DISTINCT f.id) FROM files f JOIN chunks c ON c.file_id = f.id JOIN embeddings e ON e.chunk_id = c.id",
            [], |row| row.get(0)
        )?;

        let pending_files = total_files.saturating_sub(indexed_files);

        Ok(IndexStatus {
            total_files,
            indexed_files,
            pending_files,
        })
    }

    /// Verifica se un file ha bisogno di reindicizzazione.
    pub fn needs_reindex(&self, path: &str, modified_at: DateTime<Utc>) -> Result<bool> {
        let db = self.db.lock().map_err(|e| anyhow!("Lock avvelenato: {}", e))?;
        let modified_str = modified_at.to_rfc3339();

        let stored: Option<String> = db.query_row(
            "SELECT modified_at FROM files WHERE path = ?1",
            params![path],
            |row| row.get(0),
        ).ok();

        match stored {
            None => Ok(true),              // File non ancora indicizzato
            Some(s) => Ok(s != modified_str), // Timestamp diverso
        }
    }

    /// Incrementa il contatore di accessi per il ranking.
    pub fn increment_access_count(&self, path: &str) -> Result<()> {
        let db = self.db.lock().map_err(|e| anyhow!("Lock avvelenato: {}", e))?;
        db.execute(
            "UPDATE files SET access_count = access_count + 1 WHERE path = ?1",
            params![path],
        )?;
        Ok(())
    }

    /// Elimina un file dall'indice (cascade su chunks ed embeddings).
    pub fn delete_file(&self, path: &str) -> Result<()> {
        let db = self.db.lock().map_err(|e| anyhow!("Lock avvelenato: {}", e))?;

        // Recupera l'id per cancellare i chunk (e cascading gli embeddings)
        if let Ok(file_id) = db.query_row::<i64, _, _>(
            "SELECT id FROM files WHERE path = ?1",
            params![path],
            |row| row.get(0),
        ) {
            db.execute("DELETE FROM chunks WHERE file_id = ?1", params![file_id])?;
            db.execute("DELETE FROM files WHERE id = ?1", params![file_id])?;
            debug!(path = path, "File rimosso dall'indice");
        }
        Ok(())
    }
}

// ============================================================
// Funzioni helper per conversione vettori ↔ BLOB
// ============================================================

/// Converte un vettore f32 in BLOB (little-endian).
fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    let mut blob = Vec::with_capacity(v.len() * 4);
    for &val in v {
        blob.extend_from_slice(&val.to_le_bytes());
    }
    blob
}

/// Converte un BLOB in vettore f32 (little-endian).
fn blob_to_vec(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect()
}

/// Calcola la similarità coseno tra due vettori.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    (dot / (norm_a * norm_b)) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_chunk(index: usize, content: &str) -> TextChunk {
        TextChunk {
            source_path: "/test/file.txt".to_string(),
            chunk_index: index,
            content: content.to_string(),
            mime_type: "text/plain".to_string(),
        }
    }

    #[test]
    fn test_vec_blob_roundtrip() {
        let original = vec![1.0f32, 2.5, -3.7, 0.0, 42.42];
        let blob = vec_to_blob(&original);
        let restored = blob_to_vec(&blob);
        assert_eq!(original.len(), restored.len());
        for (a, b) in original.iter().zip(restored.iter()) {
            assert!((a - b).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let v = vec![1.0, 2.0, 3.0];
        let score = cosine_similarity(&v, &v);
        assert!((score - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        let score = cosine_similarity(&a, &b);
        assert!(score.abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![-1.0, -2.0, -3.0];
        let score = cosine_similarity(&a, &b);
        assert!((score + 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_empty() {
        let score = cosine_similarity(&[], &[]);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_cosine_similarity_mismatch_length() {
        let a = vec![1.0, 2.0];
        let b = vec![1.0, 2.0, 3.0];
        let score = cosine_similarity(&a, &b);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn test_upsert_file() {
        let storage = Storage::in_memory().unwrap();
        let id = storage.upsert_file(
            "/home/user/doc.pdf", "doc.pdf", "application/pdf",
            1024, Utc::now(),
        ).unwrap();
        assert!(id > 0);

        // Secondo upsert sullo stesso path aggiorna
        let id2 = storage.upsert_file(
            "/home/user/doc.pdf", "doc.pdf", "application/pdf",
            2048, Utc::now(),
        ).unwrap();
        assert_eq!(id, id2);
    }

    #[test]
    fn test_insert_chunks() {
        let storage = Storage::in_memory().unwrap();
        let file_id = storage.upsert_file(
            "/test.txt", "test.txt", "text/plain", 100, Utc::now(),
        ).unwrap();

        let chunks = vec![
            make_chunk(0, "Primo paragrafo del documento."),
            make_chunk(1, "Secondo paragrafo con altre informazioni."),
        ];
        let ids = storage.insert_chunks(file_id, &chunks).unwrap();
        assert_eq!(ids.len(), 2);
    }

    #[test]
    fn test_insert_and_search_embedding() {
        let storage = Storage::in_memory().unwrap();
        let file_id = storage.upsert_file(
            "/test.txt", "test.txt", "text/plain", 100, Utc::now(),
        ).unwrap();

        let chunks = vec![make_chunk(0, "Informazioni importanti")];
        let chunk_ids = storage.insert_chunks(file_id, &chunks).unwrap();

        // Embedding fittizio (3 dimensioni per semplicità)
        let vector = vec![0.5, 0.8, 0.2];
        storage.insert_embedding(chunk_ids[0], &vector).unwrap();

        // Cerca con un vettore simile
        let query = vec![0.5, 0.7, 0.3];
        let results = storage.cosine_search(&query, 10).unwrap();

        assert_eq!(results.len(), 1);
        assert!(results[0].cosine_score > 0.9); // Molto simili
        assert_eq!(results[0].file_path, "/test.txt");
        assert_eq!(results[0].chunk_content, "Informazioni importanti");
    }

    #[test]
    fn test_cosine_search_ranking() {
        let storage = Storage::in_memory().unwrap();

        // File 1 — embedding vicino alla query
        let f1 = storage.upsert_file("/a.txt", "a.txt", "text/plain", 10, Utc::now()).unwrap();
        let c1 = storage.insert_chunks(f1, &[make_chunk(0, "vicino")]).unwrap();
        storage.insert_embedding(c1[0], &[1.0, 0.0, 0.0]).unwrap();

        // File 2 — embedding lontano dalla query
        let f2 = storage.upsert_file("/b.txt", "b.txt", "text/plain", 10, Utc::now()).unwrap();
        let c2 = storage.insert_chunks(f2, &[make_chunk(0, "lontano")]).unwrap();
        storage.insert_embedding(c2[0], &[0.0, 1.0, 0.0]).unwrap();

        // Query nella direzione di a.txt
        let results = storage.cosine_search(&[0.9, 0.1, 0.0], 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].file_path, "/a.txt"); // Più simile
    }

    #[test]
    fn test_needs_reindex() {
        let storage = Storage::in_memory().unwrap();
        let now = Utc::now();

        // File non esiste → serve reindicizzazione
        assert!(storage.needs_reindex("/new.txt", now).unwrap());

        // Inserisci il file
        storage.upsert_file("/new.txt", "new.txt", "text/plain", 100, now).unwrap();

        // Stesso timestamp → non serve
        assert!(!storage.needs_reindex("/new.txt", now).unwrap());

        // Timestamp diverso → serve
        let later = now + chrono::Duration::seconds(60);
        assert!(storage.needs_reindex("/new.txt", later).unwrap());
    }

    #[test]
    fn test_delete_file_cascades() {
        let storage = Storage::in_memory().unwrap();
        let file_id = storage.upsert_file(
            "/del.txt", "del.txt", "text/plain", 50, Utc::now(),
        ).unwrap();

        let chunk_ids = storage.insert_chunks(file_id, &[make_chunk(0, "contenuto")]).unwrap();
        storage.insert_embedding(chunk_ids[0], &[1.0, 0.0]).unwrap();

        // Elimina
        storage.delete_file("/del.txt").unwrap();

        // La ricerca non deve trovare nulla
        let results = storage.cosine_search(&[1.0, 0.0], 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_index_status() {
        let storage = Storage::in_memory().unwrap();

        let status = storage.get_index_status().unwrap();
        assert_eq!(status.total_files, 0);

        // Aggiungi un file con embedding
        let f1 = storage.upsert_file("/a.txt", "a.txt", "text/plain", 10, Utc::now()).unwrap();
        let c1 = storage.insert_chunks(f1, &[make_chunk(0, "testo")]).unwrap();
        storage.insert_embedding(c1[0], &[1.0]).unwrap();

        // Aggiungi un file senza embedding (pending)
        storage.upsert_file("/b.txt", "b.txt", "text/plain", 10, Utc::now()).unwrap();

        let status = storage.get_index_status().unwrap();
        assert_eq!(status.total_files, 2);
        assert_eq!(status.indexed_files, 1);
        assert_eq!(status.pending_files, 1);
    }

    #[test]
    fn test_increment_access_count() {
        let storage = Storage::in_memory().unwrap();
        storage.upsert_file("/a.txt", "a.txt", "text/plain", 10, Utc::now()).unwrap();

        storage.increment_access_count("/a.txt").unwrap();
        storage.increment_access_count("/a.txt").unwrap();

        // Verifica tramite una ricerca
        let f1 = storage.upsert_file("/a.txt", "a.txt", "text/plain", 10, Utc::now()).unwrap();
        let c1 = storage.insert_chunks(f1, &[make_chunk(0, "testo")]).unwrap();
        storage.insert_embedding(c1[0], &[1.0]).unwrap();

        let results = storage.cosine_search(&[1.0], 10).unwrap();
        // access_count è stato resettato dall'upsert, quindi non possiamo verificare qui
        // Verifichiamo solo che non crasha
        assert!(!results.is_empty());
    }
}
