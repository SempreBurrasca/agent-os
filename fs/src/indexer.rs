//! Indexer — pipeline di indicizzazione contenuti file.
//!
//! Estrae contenuto testuale in base al tipo file:
//! - PDF → pdftotext
//! - Immagini → tesseract OCR
//! - Documenti office → pandoc
//! - Testo/codice → lettura diretta
//! - Audio/video → metadata con ffprobe

use anyhow::{Result, anyhow};
use tracing::{debug, warn};
use std::path::Path;

/// Chunk di testo indicizzato.
#[derive(Debug, Clone)]
pub struct TextChunk {
    /// Percorso del file sorgente
    pub source_path: String,
    /// Indice del chunk nel file
    pub chunk_index: usize,
    /// Contenuto testuale del chunk
    pub content: String,
    /// Tipo MIME del file sorgente
    pub mime_type: String,
}

/// L'Indexer estrae e chunka il contenuto dei file.
pub struct Indexer {
    /// Dimensione target dei chunk (in caratteri approssimativi)
    chunk_size: usize,
}

impl Indexer {
    /// Crea un nuovo Indexer.
    pub fn new(chunk_size: usize) -> Self {
        Self { chunk_size }
    }

    /// Indicizza un file: estrai il testo e dividilo in chunk.
    pub async fn index_file(&self, path: &Path) -> Result<Vec<TextChunk>> {
        let mime = Self::detect_mime(path);
        let text = self.extract_text(path, &mime).await?;

        if text.is_empty() {
            return Ok(vec![]);
        }

        let chunks = self.chunk_text(&text, path.to_str().unwrap_or(""), &mime);
        debug!(path = %path.display(), chunks = chunks.len(), "File indicizzato");
        Ok(chunks)
    }

    /// Rileva il tipo MIME in base all'estensione.
    fn detect_mime(path: &Path) -> String {
        match path.extension().and_then(|e| e.to_str()) {
            Some("pdf") => "application/pdf".to_string(),
            Some("txt" | "md" | "rst") => "text/plain".to_string(),
            Some("rs" | "py" | "js" | "ts" | "c" | "cpp" | "h" | "go" | "java") => "text/x-source".to_string(),
            Some("html" | "htm") => "text/html".to_string(),
            Some("json" | "yaml" | "yml" | "toml" | "xml") => "text/structured".to_string(),
            Some("png" | "jpg" | "jpeg" | "gif" | "bmp" | "tiff") => "image/generic".to_string(),
            Some("doc" | "docx" | "odt") => "application/document".to_string(),
            Some("xls" | "xlsx" | "ods") => "application/spreadsheet".to_string(),
            Some("mp3" | "wav" | "ogg" | "flac") => "audio/generic".to_string(),
            Some("mp4" | "avi" | "mkv" | "webm") => "video/generic".to_string(),
            _ => "application/octet-stream".to_string(),
        }
    }

    /// Estrae il contenuto testuale dal file.
    async fn extract_text(&self, path: &Path, mime: &str) -> Result<String> {
        match mime {
            "text/plain" | "text/x-source" | "text/html" | "text/structured" => {
                // Lettura diretta
                tokio::fs::read_to_string(path).await
                    .map_err(|e| anyhow!("Errore lettura {}: {}", path.display(), e))
            }
            "application/pdf" => {
                Self::extract_pdf(path).await
            }
            "image/generic" => {
                Self::extract_ocr(path).await
            }
            "application/document" | "application/spreadsheet" => {
                Self::extract_pandoc(path).await
            }
            "audio/generic" | "video/generic" => {
                Self::extract_media_metadata(path).await
            }
            _ => {
                warn!(path = %path.display(), mime = mime, "Tipo file non supportato");
                Ok(String::new())
            }
        }
    }

    /// Estrae testo da PDF con pdftotext.
    async fn extract_pdf(path: &Path) -> Result<String> {
        let output = tokio::process::Command::new("pdftotext")
            .arg(path)
            .arg("-")
            .output()
            .await
            .map_err(|e| anyhow!("pdftotext non disponibile: {}", e))?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Estrae testo da immagine con tesseract OCR.
    async fn extract_ocr(path: &Path) -> Result<String> {
        let output = tokio::process::Command::new("tesseract")
            .arg(path)
            .arg("stdout")
            .arg("-l").arg("ita+eng")
            .output()
            .await
            .map_err(|e| anyhow!("tesseract non disponibile: {}", e))?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Estrae testo da documenti office con pandoc.
    async fn extract_pandoc(path: &Path) -> Result<String> {
        let output = tokio::process::Command::new("pandoc")
            .arg(path)
            .arg("-t").arg("plain")
            .output()
            .await
            .map_err(|e| anyhow!("pandoc non disponibile: {}", e))?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Estrae metadata da file audio/video con ffprobe.
    async fn extract_media_metadata(path: &Path) -> Result<String> {
        let output = tokio::process::Command::new("ffprobe")
            .arg("-v").arg("quiet")
            .arg("-print_format").arg("json")
            .arg("-show_format")
            .arg(path)
            .output()
            .await
            .map_err(|e| anyhow!("ffprobe non disponibile: {}", e))?;

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Divide il testo in chunk di dimensione approssimativa.
    /// Preferisce dividere su paragrafi o frasi.
    fn chunk_text(&self, text: &str, source_path: &str, mime: &str) -> Vec<TextChunk> {
        let paragraphs: Vec<&str> = text.split("\n\n").collect();
        let mut chunks = Vec::new();
        let mut current_chunk = String::new();
        let mut chunk_index = 0;

        for paragraph in paragraphs {
            if current_chunk.len() + paragraph.len() > self.chunk_size && !current_chunk.is_empty() {
                chunks.push(TextChunk {
                    source_path: source_path.to_string(),
                    chunk_index,
                    content: current_chunk.trim().to_string(),
                    mime_type: mime.to_string(),
                });
                current_chunk = String::new();
                chunk_index += 1;
            }
            current_chunk.push_str(paragraph);
            current_chunk.push_str("\n\n");
        }

        // Ultimo chunk
        if !current_chunk.trim().is_empty() {
            chunks.push(TextChunk {
                source_path: source_path.to_string(),
                chunk_index,
                content: current_chunk.trim().to_string(),
                mime_type: mime.to_string(),
            });
        }

        chunks
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_mime() {
        assert_eq!(Indexer::detect_mime(Path::new("file.pdf")), "application/pdf");
        assert_eq!(Indexer::detect_mime(Path::new("file.rs")), "text/x-source");
        assert_eq!(Indexer::detect_mime(Path::new("file.txt")), "text/plain");
        assert_eq!(Indexer::detect_mime(Path::new("file.png")), "image/generic");
        assert_eq!(Indexer::detect_mime(Path::new("file.xyz")), "application/octet-stream");
    }

    #[test]
    fn test_chunk_text_small() {
        let indexer = Indexer::new(500);
        let text = "Primo paragrafo.\n\nSecondo paragrafo.";
        let chunks = indexer.chunk_text(text, "/test.txt", "text/plain");
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].content.contains("Primo"));
        assert!(chunks[0].content.contains("Secondo"));
    }

    #[test]
    fn test_chunk_text_large() {
        let indexer = Indexer::new(50);
        let text = "Primo paragrafo con un po' di testo.\n\nSecondo paragrafo con altro testo.\n\nTerzo paragrafo finale.";
        let chunks = indexer.chunk_text(text, "/test.txt", "text/plain");
        assert!(chunks.len() >= 2);

        // Verifica che gli indici siano corretti
        for (i, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.chunk_index, i);
        }
    }

    #[test]
    fn test_chunk_text_empty() {
        let indexer = Indexer::new(500);
        let chunks = indexer.chunk_text("", "/test.txt", "text/plain");
        assert!(chunks.is_empty());
    }
}
