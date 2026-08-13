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
    pub ok: bool,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
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
            let mut reader = BufReader::new(stream.try_clone().unwrap());
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
                let mut stream = stream;
                let _ = writeln!(stream, "{}", serde_json::to_string(&resp).unwrap());
            }
        });
    }
    Ok(())
}
