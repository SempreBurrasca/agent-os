//! FUSE Layer — mount directory virtuale con viste semantiche.
//!
//! Monta /agent-fs/ usando il crate `fuser` con directory virtuali
//! organizzate per tipo, data, progetto.
//! Opzionale per l'MVP — la ricerca via IPC è la priorità.
//! Implementazione completa nella Fase 4.

/// Stato del mount FUSE.
pub struct FuseMount {
    _mount_point: String,
}

impl FuseMount {
    /// Crea una nuova istanza (non monta ancora).
    pub fn new(mount_point: &str) -> Self {
        Self {
            _mount_point: mount_point.to_string(),
        }
    }

    // TODO Fase 4: implementare mount(), unmount(), readdir(), read()
}
