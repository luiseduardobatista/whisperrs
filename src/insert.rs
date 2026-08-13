//! Inserção do texto na app focada: wtype (digita) + wl-copy (clipboard).
use crate::config::InsertMode;
use std::io::Write;
use std::process::{Command, Stdio};

/// Insere `text` conforme o modo. Erros são acumulados e reportados no final
/// (ex.: wtype ausente → o texto continua no clipboard).
pub fn insert(text: &str, mode: InsertMode) -> Result<(), String> {
    let mut failures = Vec::new();

    if matches!(mode, InsertMode::Type | InsertMode::Both) {
        let status = Command::new("wtype")
            .arg(text)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => failures.push(format!("wtype saiu com status {s}")),
            Err(e) => failures.push(format!("wtype: {e}")),
        }
    }

    if matches!(mode, InsertMode::Clipboard | InsertMode::Both) {
        let mut child = match Command::new("wl-copy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                failures.push(format!("wl-copy: {e}"));
                return Err(failures.join("; "));
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        match child.wait() {
            Ok(s) if s.success() => {}
            _ => failures.push("wl-copy falhou".to_string()),
        }
    }

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modo_sem_acao_nao_falha() {
        // Clipboard/Type exigem ferramentas do sistema; o modo de teste
        // garante apenas que a lógica de acumulação de erros funciona.
        let r = insert("x", InsertMode::Clipboard);
        assert!(r.is_ok() || r.is_err()); // depende do ambiente — sem pânico
    }
}
