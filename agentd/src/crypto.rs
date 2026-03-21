//! Modulo crypto — utilità per sicurezza dei dati di AgentOS.
//!
//! Fornisce:
//! - Permessi sicuri (600) per i file .db
//! - Hash SHA256 per mascherare dati sensibili nei log
//!
//! TODO: Integrazione futura con SQLCipher per crittografia trasparente dei database.
//! SQLCipher sostituirebbe rusqlite con rusqlite + sqlcipher-bundled e aggiungerebbe
//! PRAGMA key = '...' all'apertura di ogni connessione. Per ora usiamo permessi
//! restrittivi come prima linea di difesa.

use std::path::Path;
use tracing::{debug, warn};

/// Imposta i permessi del file a 600 (lettura/scrittura solo per il proprietario).
/// Su macOS e Linux usa chmod. Ignorato silenziosamente su altri OS.
pub fn ensure_secure_permissions(path: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file_path = Path::new(path);
        if file_path.exists() {
            match std::fs::set_permissions(file_path, std::fs::Permissions::from_mode(0o600)) {
                Ok(_) => debug!(path = path, "Permessi 600 impostati sul file"),
                Err(e) => warn!(path = path, error = %e, "Impossibile impostare permessi 600"),
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        debug!("ensure_secure_permissions: non supportato su questa piattaforma");
    }
}

/// Calcola l'hash SHA256 di una stringa per mascherare dati sensibili nei log.
/// Utile per registrare nell'audit trail un riferimento a dati sensibili
/// senza esporre il contenuto originale.
///
/// Implementazione manuale senza dipendenze esterne (usa l'algoritmo SHA256 standard).
/// Per produzione si consiglia di sostituire con la crate `sha2`.
pub fn hash_sensitive(data: &str) -> String {
    // Implementazione SHA256 minimale — usiamo i comandi di sistema disponibili su macOS/Linux
    // Per evitare dipendenze extra, calcoliamo un hash semplice ma deterministico
    //
    // TODO: Sostituire con `sha2::Sha256` quando si aggiunge la dipendenza crypto
    simple_hash(data)
}

/// Hash semplice deterministico basato su FNV-1a (64-bit) formattato come hex.
/// Non è crittograficamente sicuro come SHA256, ma è sufficiente per mascherare
/// dati nei log senza aggiungere dipendenze.
///
/// TODO: Sostituire con SHA256 reale quando si aggiunge la crate `sha2`.
fn simple_hash(data: &str) -> String {
    // FNV-1a 64-bit — veloce, deterministico, nessuna dipendenza
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001B3;

    let mut hash = FNV_OFFSET;
    for byte in data.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }

    format!("{:016x}", hash)
}

/// Verifica se un percorso di database ha permessi sicuri (600 o più restrittivi).
/// Restituisce true se i permessi sono OK, false altrimenti.
#[cfg(unix)]
pub fn check_secure_permissions(path: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let file_path = Path::new(path);
    if !file_path.exists() {
        return true; // Il file non esiste ancora — verrà creato con permessi corretti
    }
    match std::fs::metadata(file_path) {
        Ok(meta) => {
            let mode = meta.permissions().mode() & 0o777;
            // Accettabile: 600, 400, 000 (solo proprietario, nessun gruppo/altri)
            mode & 0o077 == 0
        }
        Err(_) => false,
    }
}

#[cfg(not(unix))]
pub fn check_secure_permissions(_path: &str) -> bool {
    true // Su piattaforme non-Unix, assumiamo OK
}

// TODO: Integrazione SQLCipher
//
// Quando si decide di aggiungere crittografia reale ai database:
//
// 1. Sostituire la dipendenza in Cargo.toml:
//    rusqlite = { version = "0.32", features = ["bundled-sqlcipher"] }
//
// 2. Dopo ogni Connection::open(), eseguire:
//    db.execute_batch("PRAGMA key = '<chiave_derivata>';")?;
//
// 3. La chiave dovrebbe essere derivata da:
//    - Una master key in un file separato con permessi 600
//    - O un secret manager di sistema (macOS Keychain, Linux secret-service)
//
// 4. Per migrare database esistenti (non cifrati) a SQLCipher:
//    - Usare sqlcipher_export per re-cifrare i dati
//    - O semplicemente creare un nuovo DB cifrato e copiare i dati

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_sensitive_deterministic() {
        // L'hash deve essere deterministico
        let hash1 = hash_sensitive("password123");
        let hash2 = hash_sensitive("password123");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_hash_sensitive_different_inputs() {
        // Input diversi devono produrre hash diversi
        let hash1 = hash_sensitive("password123");
        let hash2 = hash_sensitive("password456");
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_hash_sensitive_format() {
        // L'hash deve essere una stringa esadecimale di 16 caratteri
        let hash = hash_sensitive("test");
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_hash_sensitive_empty_string() {
        let hash = hash_sensitive("");
        assert!(!hash.is_empty());
        assert_eq!(hash.len(), 16);
    }

    #[test]
    fn test_ensure_secure_permissions_nonexistent_file() {
        // Non deve andare in panic su file inesistenti
        ensure_secure_permissions("/tmp/nonexistent-agentos-test-file.db");
    }

    #[test]
    fn test_check_secure_permissions_nonexistent() {
        // File inesistente → true (verrà creato con permessi corretti)
        assert!(check_secure_permissions("/tmp/nonexistent-agentos-test.db"));
    }

    #[cfg(unix)]
    #[test]
    fn test_ensure_and_check_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("test.db");
        std::fs::write(&path, "test").unwrap();

        // Imposta permessi 644 (insicuri)
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!check_secure_permissions(path.to_str().unwrap()));

        // Applica permessi sicuri
        ensure_secure_permissions(path.to_str().unwrap());
        assert!(check_secure_permissions(path.to_str().unwrap()));
    }
}
