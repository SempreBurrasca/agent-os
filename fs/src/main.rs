//! agent-fs — filesystem semantico FUSE per AgentOS.
//!
//! Indicizza i file dell'utente, genera embedding semantici,
//! e permette ricerche per significato oltre che per nome.
//!
//! Pipeline: watcher → indexer → embedder → storage → search
//! IPC: agentd invia AgentToFs, agent-fs risponde con FsToAgent

mod connectors;
mod embedder;
mod fuse_layer;
mod indexer;
mod ipc_server;
mod search;
mod storage;
mod watcher;

use anyhow::Result;
use agentos_common::config::AgentOsConfig;
use agentos_common::ipc::{AgentToFs, FsToAgent, JsonRpcResponse};
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn, debug};

/// Directory dati di default per l'indice
const DATA_DIR: &str = "/var/lib/agent-fs";

#[tokio::main]
async fn main() -> Result<()> {
    // Inizializza il logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("agent_fs=debug".parse().unwrap())
        )
        .init();

    info!("agent-fs avviato");

    // Carica configurazione
    let config = load_config()?;
    info!(
        watch_paths = ?config.fs.watch_paths,
        chunk_size = config.fs.chunk_size,
        "Configurazione caricata"
    );

    // Crea directory dati
    let data_dir = resolve_data_dir();
    std::fs::create_dir_all(&data_dir)?;

    // Inizializza i componenti
    let db_path = data_dir.join("index.db");
    let storage = Arc::new(storage::Storage::new(db_path.to_str().unwrap())?);
    let embedder = Arc::new(embedder::Embedder::new(
        &config.ollama.url,
        &config.ollama.embedding_model,
    ));
    let indexer = indexer::Indexer::new(config.fs.chunk_size);
    let search_engine = search::SearchEngine::new();

    // Avvia il file watcher
    let file_watcher = watcher::FileWatcher::new(
        config.fs.watch_paths.clone(),
        config.fs.ignore_extensions.clone(),
        config.fs.ignore_dirs.clone(),
    );
    let mut event_rx = file_watcher.start()?;
    info!("File watcher avviato");

    // Avvia il server IPC
    let socket_path = ipc_server::IpcServer::default_socket_path();
    let ipc = ipc_server::IpcServer::new(&socket_path);
    let mut ipc_rx = ipc.start().await?;

    // Stampa lo stato iniziale dell'indice
    if let Ok(status) = storage.get_index_status() {
        info!(
            total = status.total_files,
            indexed = status.indexed_files,
            pending = status.pending_files,
            "Stato indice"
        );
    }

    // Spawna indicizzazione iniziale in background
    {
        let storage = Arc::clone(&storage);
        let embedder = Arc::clone(&embedder);
        let watch_paths = config.fs.watch_paths.clone();
        let ignore_ext = config.fs.ignore_extensions.clone();
        let ignore_dirs = config.fs.ignore_dirs.clone();
        let chunk_size = config.fs.chunk_size;

        tokio::spawn(async move {
            info!("Indicizzazione iniziale avviata");
            if let Err(e) = initial_indexing(
                &watch_paths, &ignore_ext, &ignore_dirs,
                chunk_size, &storage, &embedder,
            ).await {
                warn!(error = %e, "Errore durante indicizzazione iniziale");
            }
            info!("Indicizzazione iniziale completata");
        });
    }

    // Gestione segnali
    let mut sigterm = tokio::signal::unix::signal(
        tokio::signal::unix::SignalKind::terminate()
    )?;

    // === Loop principale ===
    info!("agent-fs pronto");
    loop {
        tokio::select! {
            // Evento dal file watcher
            Some(event) = event_rx.recv() => {
                let storage = Arc::clone(&storage);
                let embedder = Arc::clone(&embedder);
                let chunk_size = config.fs.chunk_size;

                // Processa in background per non bloccare il loop
                tokio::spawn(async move {
                    if let Err(e) = handle_file_event(
                        &event.path, chunk_size, &storage, &embedder,
                    ).await {
                        debug!(
                            path = %event.path.display(),
                            error = %e,
                            "Errore indicizzazione file"
                        );
                    }
                });
            }

            // Messaggio IPC da agentd
            Some(msg) = ipc_rx.recv() => {
                let response = handle_ipc_message(
                    msg.message,
                    &search_engine,
                    &storage,
                    &embedder,
                    &config,
                ).await;

                let rpc_response = match response {
                    Ok(fs_msg) => {
                        let result = serde_json::to_value(&fs_msg).unwrap_or_default();
                        JsonRpcResponse::success(result, msg.request_id)
                    }
                    Err(e) => {
                        JsonRpcResponse::error(
                            agentos_common::ipc::INTERNAL_ERROR,
                            &e.to_string(),
                            msg.request_id,
                        )
                    }
                };

                if let Err(e) = msg.reply_tx.send(rpc_response).await {
                    warn!(error = %e, "Errore invio risposta IPC");
                }
            }

            // Shutdown graceful
            _ = sigterm.recv() => {
                info!("SIGTERM — shutdown agent-fs");
                break;
            }
        }
    }

    info!("agent-fs terminato");
    Ok(())
}

/// Carica la configurazione (stessa logica di agentd).
fn load_config() -> Result<AgentOsConfig> {
    let paths = [
        "/etc/agentos/config.yaml",
        "config.yaml",
        "../config.yaml",
    ];
    for path in &paths {
        if let Ok(config) = AgentOsConfig::from_file_with_env(path) {
            return Ok(config);
        }
    }
    anyhow::bail!("Configurazione non trovata")
}

/// Determina la directory dati per l'indice.
fn resolve_data_dir() -> PathBuf {
    // Su Linux usiamo /var/lib/agent-fs, altrimenti ~/.local/share/agent-fs
    if cfg!(target_os = "linux") && Path::new(DATA_DIR).parent().map(|p| p.exists()).unwrap_or(false) {
        PathBuf::from(DATA_DIR)
    } else {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("agent-fs")
    }
}

/// Indicizzazione iniziale: cammina le directory e indicizza i file non ancora presenti.
async fn initial_indexing(
    watch_paths: &[String],
    ignore_ext: &[String],
    ignore_dirs: &[String],
    chunk_size: usize,
    storage: &storage::Storage,
    embedder: &embedder::Embedder,
) -> Result<()> {
    let indexer = indexer::Indexer::new(chunk_size);
    let mut indexed = 0u64;

    for watch_path in watch_paths {
        let path = Path::new(watch_path);
        if !path.exists() {
            warn!(path = %watch_path, "Directory non esistente — saltata");
            continue;
        }

        // Cammina ricorsivamente
        let walker = walkdir::WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| {
                // Filtra directory da ignorare
                if e.file_type().is_dir() {
                    let name = e.file_name().to_str().unwrap_or("");
                    return !ignore_dirs.contains(&name.to_string());
                }
                true
            });

        for entry in walker.filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() {
                continue;
            }

            let file_path = entry.path();

            // Filtra per estensione
            if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
                let ext_with_dot = format!(".{}", ext);
                if ignore_ext.contains(&ext_with_dot) {
                    continue;
                }
            }

            // Controlla se serve reindicizzare
            if let Ok(meta) = std::fs::metadata(file_path) {
                let modified = meta.modified()
                    .ok()
                    .and_then(|t| chrono::DateTime::<Utc>::from(t).into())
                    .unwrap_or_else(chrono::Utc::now);

                let path_str = file_path.to_str().unwrap_or("");
                if let Ok(false) = storage.needs_reindex(path_str, modified) {
                    continue; // Già indicizzato e aggiornato
                }

                if let Err(e) = index_single_file(file_path, &indexer, storage, embedder).await {
                    debug!(path = %file_path.display(), error = %e, "Errore indicizzazione");
                } else {
                    indexed += 1;
                    if indexed % 100 == 0 {
                        info!(count = indexed, "File indicizzati...");
                    }
                }
            }
        }
    }

    info!(total = indexed, "Indicizzazione iniziale completata");
    Ok(())
}

/// Gestisce un evento di cambiamento file dal watcher.
async fn handle_file_event(
    path: &Path,
    chunk_size: usize,
    storage: &storage::Storage,
    embedder: &embedder::Embedder,
) -> Result<()> {
    // Controlla se il file esiste ancora (potrebbe essere stato cancellato)
    if !path.exists() {
        if let Some(path_str) = path.to_str() {
            storage.delete_file(path_str)?;
            debug!(path = path_str, "File rimosso dall'indice");
        }
        return Ok(());
    }

    let indexer = indexer::Indexer::new(chunk_size);
    index_single_file(path, &indexer, storage, embedder).await
}

/// Indicizza un singolo file: estrai testo → chunk → embedding → storage.
async fn index_single_file(
    path: &Path,
    indexer: &indexer::Indexer,
    storage: &storage::Storage,
    embedder: &embedder::Embedder,
) -> Result<()> {
    let path_str = path.to_str().unwrap_or("");
    let name = path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    // Metadati del file
    let meta = std::fs::metadata(path)?;
    let size = meta.len();
    let modified = meta.modified()
        .ok()
        .map(chrono::DateTime::<Utc>::from)
        .unwrap_or_else(chrono::Utc::now);

    // Estrai e chunka il testo
    let chunks = indexer.index_file(path).await?;
    if chunks.is_empty() {
        return Ok(());
    }

    let mime_type = chunks.first()
        .map(|c| c.mime_type.clone())
        .unwrap_or_else(|| "application/octet-stream".to_string());

    // Salva nel database
    let file_id = storage.upsert_file(path_str, name, &mime_type, size, modified)?;
    let chunk_ids = storage.insert_chunks(file_id, &chunks)?;

    // Genera embedding per ogni chunk
    for (chunk, chunk_id) in chunks.iter().zip(chunk_ids.iter()) {
        match embedder.embed(&chunk.content).await {
            Ok(vector) => {
                storage.insert_embedding(*chunk_id, &vector)?;
            }
            Err(e) => {
                debug!(chunk_index = chunk.chunk_index, error = %e, "Errore embedding chunk");
                // Continua con gli altri chunk
            }
        }
    }

    debug!(path = path_str, chunks = chunks.len(), "File indicizzato");
    Ok(())
}

/// Gestisce un messaggio IPC da agentd.
async fn handle_ipc_message(
    message: AgentToFs,
    search_engine: &search::SearchEngine,
    storage: &storage::Storage,
    embedder: &embedder::Embedder,
    _config: &AgentOsConfig,
) -> Result<FsToAgent> {
    match message {
        AgentToFs::Search { query, file_type, folder, max_results } => {
            let params = search::SearchParams {
                query: query.clone(),
                file_type,
                folder,
                max_results: max_results as usize,
            };

            let results = search_engine.search(&params, storage, embedder).await?;

            Ok(FsToAgent::SearchResults {
                query,
                results,
            })
        }

        AgentToFs::Reindex { path } => {
            if let Some(p) = path {
                let file_path = Path::new(&p);
                if file_path.exists() {
                    let indexer = indexer::Indexer::new(500); // default chunk size
                    index_single_file(file_path, &indexer, storage, embedder).await?;
                }
            }
            // Per reindicizzazione completa, il main loop spawnerà initial_indexing

            let status = storage.get_index_status()?;
            Ok(FsToAgent::IndexStatus {
                total_files: status.total_files,
                indexed_files: status.indexed_files,
                pending_files: status.pending_files,
            })
        }

        AgentToFs::StatusRequest => {
            let status = storage.get_index_status()?;
            Ok(FsToAgent::IndexStatus {
                total_files: status.total_files,
                indexed_files: status.indexed_files,
                pending_files: status.pending_files,
            })
        }
    }
}
