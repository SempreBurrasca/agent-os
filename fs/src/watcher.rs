//! File Watcher — monitora le directory per cambiamenti tramite inotify.
//!
//! Usa il crate `notify` per osservare /home/ (o le directory configurate)
//! e invia i path dei file modificati alla pipeline di indicizzazione.

use anyhow::Result;
use notify::{Watcher, RecommendedWatcher, RecursiveMode, Event, EventKind};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use std::path::{Path, PathBuf};

/// Evento di cambiamento file.
#[derive(Debug, Clone)]
pub struct FileChangeEvent {
    pub path: PathBuf,
    pub kind: FileChangeKind,
}

/// Tipo di cambiamento.
#[derive(Debug, Clone)]
pub enum FileChangeKind {
    Created,
    Modified,
    Renamed,
}

/// File Watcher — osserva le directory per cambiamenti.
pub struct FileWatcher {
    /// Directory da monitorare
    watch_paths: Vec<String>,
    /// Estensioni da ignorare
    ignore_extensions: Vec<String>,
    /// Directory da ignorare
    ignore_dirs: Vec<String>,
}

impl FileWatcher {
    /// Crea un nuovo FileWatcher dalla configurazione.
    pub fn new(
        watch_paths: Vec<String>,
        ignore_extensions: Vec<String>,
        ignore_dirs: Vec<String>,
    ) -> Self {
        Self {
            watch_paths,
            ignore_extensions,
            ignore_dirs,
        }
    }

    /// Verifica se un path deve essere ignorato.
    fn should_ignore(&self, path: &Path) -> bool {
        // Controlla estensione
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext_with_dot = format!(".{}", ext);
            if self.ignore_extensions.contains(&ext_with_dot) {
                return true;
            }
        }

        // Controlla directory nel path
        for component in path.components() {
            if let Some(name) = component.as_os_str().to_str() {
                if self.ignore_dirs.contains(&name.to_string()) {
                    return true;
                }
            }
        }

        false
    }

    /// Avvia il monitoraggio. Restituisce un receiver per gli eventi.
    pub fn start(&self) -> Result<mpsc::Receiver<FileChangeEvent>> {
        let (tx, rx) = mpsc::channel::<FileChangeEvent>(256);
        let ignore_ext = self.ignore_extensions.clone();
        let ignore_dirs = self.ignore_dirs.clone();

        let watcher_self = Self {
            watch_paths: self.watch_paths.clone(),
            ignore_extensions: ignore_ext,
            ignore_dirs,
        };

        let (notify_tx, mut notify_rx) = mpsc::channel::<Event>(256);

        // Crea il watcher notify
        let mut watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Ok(event) = res {
                    let _ = notify_tx.blocking_send(event);
                }
            },
            notify::Config::default(),
        )?;

        // Registra le directory da monitorare
        for path in &self.watch_paths {
            let p = Path::new(path);
            if p.exists() {
                watcher.watch(p, RecursiveMode::Recursive)?;
                info!(path = path, "Monitoraggio avviato");
            } else {
                warn!(path = path, "Directory non esistente — saltata");
            }
        }

        // Task che processa gli eventi e filtra
        tokio::spawn(async move {
            // Mantieni il watcher vivo
            let _watcher = watcher;

            while let Some(event) = notify_rx.recv().await {
                let kind = match event.kind {
                    EventKind::Create(_) => FileChangeKind::Created,
                    EventKind::Modify(_) => FileChangeKind::Modified,
                    EventKind::Other => continue,  // Ignora
                    _ => continue,
                };

                for path in event.paths {
                    if !watcher_self.should_ignore(&path) && path.is_file() {
                        debug!(path = %path.display(), "Cambiamento rilevato");
                        let _ = tx.send(FileChangeEvent {
                            path,
                            kind: kind.clone(),
                        }).await;
                    }
                }
            }
        });

        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_ignore_extension() {
        let watcher = FileWatcher::new(
            vec![],
            vec![".pyc".to_string(), ".tmp".to_string()],
            vec![".git".to_string()],
        );

        assert!(watcher.should_ignore(Path::new("/home/user/cache.pyc")));
        assert!(watcher.should_ignore(Path::new("/tmp/file.tmp")));
        assert!(!watcher.should_ignore(Path::new("/home/user/doc.pdf")));
    }

    #[test]
    fn test_should_ignore_directory() {
        let watcher = FileWatcher::new(
            vec![],
            vec![],
            vec![".git".to_string(), "node_modules".to_string(), ".cache".to_string()],
        );

        assert!(watcher.should_ignore(Path::new("/home/user/project/.git/objects/abc")));
        assert!(watcher.should_ignore(Path::new("/home/user/app/node_modules/lodash/index.js")));
        assert!(!watcher.should_ignore(Path::new("/home/user/documents/report.pdf")));
    }
}
