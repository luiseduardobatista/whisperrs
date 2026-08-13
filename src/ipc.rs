//! Protocolo de IPC entre o CLI e o daemon: socket Unix + JSON por linha.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cmd {
    Toggle,
    Status,
    Stop,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub cmd: Cmd,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Binário que executa o daemon (preenchido em `status`). Ausente em
    /// daemons de versões antigas — o CLI trata como "outra origem" e
    /// reinicia.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
}

/// Envia um comando ao daemon e aguarda a resposta (lado CLI).
pub fn request(cmd: Cmd) -> Result<Response> {
    let path = crate::config::socket_path();
    let mut stream = UnixStream::connect(&path)
        .with_context(|| format!("daemon não está rodando ({})", path.display()))?;
    writeln!(stream, "{}", serde_json::to_string(&Request { cmd })?)?;
    let mut line = String::new();
    BufReader::new(&mut stream).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

/// Serve o socket (lado daemon). Cada conexão vira uma chamada de `handler`,
/// que deve responder via `Sender<Response>` (estado pós-processamento).
pub fn serve<F>(handler: F) -> Result<()>
where
    F: Fn(Cmd, Sender<Response>) + Send + Sync + 'static,
{
    let handler = std::sync::Arc::new(handler);
    let path = crate::config::socket_path();
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            bail!("outro daemon já está em execução ({})", path.display());
        }
        std::fs::remove_file(&path)?; // socket órfão de uma execução anterior
    }
    let listener = UnixListener::bind(&path)
        .with_context(|| format!("bindando socket {}", path.display()))?;
    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let handler = std::sync::Arc::clone(&handler);
        std::thread::spawn(move || {
            let mut stream = stream;
            let Ok(clone) = stream.try_clone() else {
                return;
            };
            let mut reader = BufReader::new(clone);
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                return;
            }
            let Ok(req) = serde_json::from_str::<Request>(&line) else {
                return;
            };
            let (reply_tx, reply_rx) = std::sync::mpsc::channel();
            handler(req.cmd, reply_tx);
            if let Ok(resp) = reply_rx.recv() {
                let Ok(json) = serde_json::to_string(&resp) else {
                    return;
                };
                let _ = writeln!(stream, "{json}");
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_daemon_response_without_exe() {
        // Compatibilidade: daemon de versão anterior não envia `exe` — o
        // campo deve desserializar como None (e o CLI reinicia o daemon).
        let resp: Response = serde_json::from_str(r#"{"ok":true,"state":"idle"}"#).unwrap();
        assert_eq!(resp.exe, None);
    }

    #[test]
    fn exe_serializes_and_roundtrips() {
        let resp = Response {
            state: "idle".to_string(),
            error: None,
            exe: Some("/nix/store/x-whisper/bin/.whisper-wrapped".to_string()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"exe\":\"/nix/store/x-whisper/bin/.whisper-wrapped\""));
        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(back.exe.as_deref(), Some("/nix/store/x-whisper/bin/.whisper-wrapped"));
    }
}
