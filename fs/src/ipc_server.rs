//! IPC Server per agent-fs — ascolta su Unix socket per messaggi da agentd.
//!
//! Protocollo: JSON-RPC 2.0 con messaggi AgentToFs/FsToAgent.
//! Socket path: /run/agentd-fs.sock (Linux) o /tmp/agentd-fs.sock (dev).

use anyhow::{Result, anyhow};
use agentos_common::ipc::{AgentToFs, JsonRpcRequest, JsonRpcResponse};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{debug, info, warn, error};

/// Messaggio in arrivo con canale per la risposta.
pub struct IncomingFsMessage {
    /// Il messaggio parsato
    pub message: AgentToFs,
    /// ID della richiesta JSON-RPC
    pub request_id: serde_json::Value,
    /// Canale per inviare la risposta
    pub reply_tx: mpsc::Sender<JsonRpcResponse>,
}

/// Server IPC per agent-fs.
pub struct IpcServer {
    socket_path: String,
}

impl IpcServer {
    /// Crea un nuovo server IPC.
    pub fn new(socket_path: &str) -> Self {
        Self {
            socket_path: socket_path.to_string(),
        }
    }

    /// Determina il percorso del socket in base alla piattaforma.
    pub fn default_socket_path() -> String {
        // Su Linux usiamo /run, altrimenti /tmp (macOS/dev)
        if cfg!(target_os = "linux") {
            if std::path::Path::new("/run").exists() {
                return "/run/agentd-fs.sock".to_string();
            }
        }
        // Fallback per macOS e sviluppo
        let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
            .unwrap_or_else(|_| "/tmp".to_string());
        format!("{}/agentd-fs.sock", runtime_dir)
    }

    /// Avvia il server. Restituisce un receiver per i messaggi in arrivo.
    pub async fn start(&self) -> Result<mpsc::Receiver<IncomingFsMessage>> {
        // Rimuovi socket precedente
        let _ = std::fs::remove_file(&self.socket_path);

        let listener = UnixListener::bind(&self.socket_path)?;
        info!(path = %self.socket_path, "IPC agent-fs in ascolto");

        // Permessi socket (0660)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &self.socket_path,
                std::fs::Permissions::from_mode(0o660),
            )?;
        }

        let (tx, rx) = mpsc::channel::<IncomingFsMessage>(64);

        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let tx = tx.clone();
                        tokio::spawn(Self::handle_connection(stream, tx));
                    }
                    Err(e) => {
                        error!(error = %e, "Errore accettazione connessione agent-fs");
                    }
                }
            }
        });

        Ok(rx)
    }

    /// Gestisce una singola connessione.
    async fn handle_connection(stream: UnixStream, tx: mpsc::Sender<IncomingFsMessage>) {
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    debug!("Client agentd disconnesso");
                    break;
                }
                Ok(_) => {
                    match Self::process_line(&line, &tx).await {
                        Ok(response) => {
                            let json = serde_json::to_string(&response).unwrap_or_default();
                            if let Err(e) = writer.write_all(format!("{}\n", json).as_bytes()).await {
                                warn!(error = %e, "Errore invio risposta");
                                break;
                            }
                        }
                        Err(e) => {
                            warn!(error = %e, "Errore processamento messaggio");
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Errore lettura dal socket");
                    break;
                }
            }
        }
    }

    /// Processa una riga JSON-RPC.
    async fn process_line(
        line: &str,
        tx: &mpsc::Sender<IncomingFsMessage>,
    ) -> Result<JsonRpcResponse> {
        let request: JsonRpcRequest = serde_json::from_str(line.trim())
            .map_err(|e| anyhow!("JSON-RPC invalido: {}", e))?;

        let request_id = request.id.clone().unwrap_or(serde_json::Value::Null);

        // Parsa il messaggio AgentToFs
        let message: AgentToFs = serde_json::from_value(request.params)
            .map_err(|e| anyhow!("Parametri AgentToFs non validi: {}", e))?;

        debug!(method = %request.method, "Messaggio ricevuto da agentd");

        let (reply_tx, mut reply_rx) = mpsc::channel::<JsonRpcResponse>(1);

        tx.send(IncomingFsMessage {
            message,
            request_id: request_id.clone(),
            reply_tx,
        }).await
        .map_err(|_| anyhow!("Canale messaggi chiuso"))?;

        reply_rx.recv().await
            .ok_or_else(|| anyhow!("Nessuna risposta dal loop principale"))
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
        debug!(path = %self.socket_path, "Socket IPC agent-fs rimosso");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ipc_server_creation() {
        let server = IpcServer::new("/tmp/test-fs.sock");
        assert_eq!(server.socket_path, "/tmp/test-fs.sock");
    }

    #[test]
    fn test_default_socket_path() {
        let path = IpcServer::default_socket_path();
        assert!(path.contains("agentd-fs.sock"));
    }

    #[tokio::test]
    async fn test_socket_lifecycle() {
        let path = format!("/tmp/test-agentfs-{}.sock", std::process::id());
        let server = IpcServer::new(&path);
        let _rx = server.start().await.unwrap();

        assert!(std::path::Path::new(&path).exists());
        drop(server);
    }
}
