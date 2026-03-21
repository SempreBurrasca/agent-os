//! Input Handler — gestione delle connessioni IPC su Unix socket.
//!
//! Ascolta su /run/agentd.sock per messaggi JSON-RPC da agent-shell
//! e gestisce il dispatch dei messaggi al loop principale.

use anyhow::{Result, anyhow};
use agentos_common::ipc::{JsonRpcRequest, JsonRpcResponse, ShellToAgent};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{info, warn, debug, error};

/// Messaggio ricevuto dal socket con canale per la risposta.
pub struct IncomingMessage {
    /// Il messaggio parsato
    pub message: ShellToAgent,
    /// ID della richiesta JSON-RPC (per la risposta)
    pub request_id: serde_json::Value,
    /// Canale per inviare la risposta
    pub reply_tx: mpsc::Sender<JsonRpcResponse>,
}

/// Input Handler — ascolta su Unix socket e produce messaggi.
pub struct InputHandler {
    socket_path: String,
}

impl InputHandler {
    /// Crea un nuovo InputHandler.
    pub fn new(socket_path: &str) -> Self {
        Self {
            socket_path: socket_path.to_string(),
        }
    }

    /// Avvia l'ascolto sul socket. Restituisce un receiver per i messaggi in arrivo.
    pub async fn start(&self) -> Result<mpsc::Receiver<IncomingMessage>> {
        // Rimuovi il socket se esiste già (da una sessione precedente)
        let _ = std::fs::remove_file(&self.socket_path);

        let listener = UnixListener::bind(&self.socket_path)?;
        info!(path = %self.socket_path, "Socket IPC in ascolto");

        // Imposta i permessi del socket (0660)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                &self.socket_path,
                std::fs::Permissions::from_mode(0o660),
            )?;
        }

        let (tx, rx) = mpsc::channel::<IncomingMessage>(64);

        // Spawna il task di ascolto
        tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _addr)) => {
                        let tx = tx.clone();
                        tokio::spawn(Self::handle_connection(stream, tx));
                    }
                    Err(e) => {
                        error!(error = %e, "Errore accettazione connessione");
                    }
                }
            }
        });

        Ok(rx)
    }

    /// Gestisce una singola connessione client.
    async fn handle_connection(stream: UnixStream, tx: mpsc::Sender<IncomingMessage>) {
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();

        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    debug!("Client disconnesso");
                    break;
                }
                Ok(_) => {
                    match Self::process_line(&line, &tx).await {
                        Ok(response) => {
                            let response_json = serde_json::to_string(&response).unwrap_or_default();
                            if let Err(e) = writer.write_all(format!("{}\n", response_json).as_bytes()).await {
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

    /// Processa una singola riga JSON-RPC ricevuta.
    async fn process_line(
        line: &str,
        tx: &mpsc::Sender<IncomingMessage>,
    ) -> Result<JsonRpcResponse> {
        let request: JsonRpcRequest = serde_json::from_str(line.trim())
            .map_err(|e| anyhow!("JSON-RPC invalido: {}", e))?;

        let request_id = request.id.clone().unwrap_or(serde_json::Value::Null);

        // Parsa il messaggio ShellToAgent dai params
        let message: ShellToAgent = serde_json::from_value(request.params)
            .map_err(|e| anyhow!("Parametri non validi: {}", e))?;

        debug!(method = %request.method, "Messaggio ricevuto");

        // Crea il canale per la risposta
        let (reply_tx, mut reply_rx) = mpsc::channel::<JsonRpcResponse>(1);

        // Invia il messaggio al loop principale
        tx.send(IncomingMessage {
            message,
            request_id: request_id.clone(),
            reply_tx,
        }).await
        .map_err(|_| anyhow!("Canale messaggi chiuso"))?;

        // Attendi la risposta dal loop principale
        reply_rx.recv().await
            .ok_or_else(|| anyhow!("Nessuna risposta dal loop principale"))
    }
}

/// Cleanup del socket alla chiusura.
impl Drop for InputHandler {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
        debug!(path = %self.socket_path, "Socket IPC rimosso");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_input_handler_creation() {
        let handler = InputHandler::new("/tmp/test-agentd.sock");
        assert_eq!(handler.socket_path, "/tmp/test-agentd.sock");
    }

    #[tokio::test]
    async fn test_socket_lifecycle() {
        let socket_path = format!("/tmp/test-agentd-{}.sock", std::process::id());

        // Crea e avvia l'handler
        let handler = InputHandler::new(&socket_path);
        let _rx = handler.start().await.unwrap();

        // Verifica che il socket esista
        assert!(std::path::Path::new(&socket_path).exists());

        // Il Drop dovrebbe rimuovere il socket
        drop(handler);
        // Nota: il socket potrebbe non essere rimosso immediatamente
        // perché il task di ascolto è ancora attivo
    }
}
