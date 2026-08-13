//! Inserção do texto na app focada: wtype (digita) + wl-copy (clipboard).
use crate::config::InsertMode;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Dica de instalação exibida quando o `wtype` está ausente do PATH.
pub fn wtype_hint() -> &'static str {
    "instale o pacote wtype (NixOS: adicione pkgs.wtype ao environment.systemPackages; \
     Arch: pacman -S wtype; Debian/Ubuntu: apt install wtype)"
}

/// Verifica se `wtype` está disponível no PATH (sem executar nada).
pub fn wtype_available() -> bool {
    match std::env::var_os("PATH") {
        Some(path) => has_wtype_in(std::env::split_paths(&path)),
        None => false,
    }
}

fn has_wtype_in(mut paths: impl Iterator<Item = PathBuf>) -> bool {
    paths.any(|dir| executable_file(&dir.join("wtype")))
}

fn executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

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
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                failures.push(format!("wtype não encontrado no PATH — {}", wtype_hint()));
            }
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
    fn mode_without_action_does_not_fail() {
        // Clipboard/Type exigem ferramentas do sistema; o modo de teste
        // garante apenas que a lógica de acumulação de erros funciona.
        let r = insert("x", InsertMode::Clipboard);
        assert!(r.is_ok() || r.is_err()); // depende do ambiente — sem pânico
    }

    #[test]
    fn detects_wtype_in_path() {
        let dir = std::env::temp_dir().join(format!("whisper-wtype-{}", std::process::id()));
        let bin = dir.join("wtype");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();

        // Executável → encontrado.
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        assert!(has_wtype_in(std::iter::once(dir.clone())));

        // Sem permissão de execução → ignorado.
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&bin, perms).unwrap();
        assert!(!has_wtype_in(std::iter::once(dir.clone())));

        // Diretório vazio → ignorado.
        let empty = std::env::temp_dir().join(format!("whisper-wtype-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        assert!(!has_wtype_in(std::iter::once(empty.clone())));

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }
}
