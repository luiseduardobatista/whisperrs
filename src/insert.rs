use crate::config::InsertMode;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn wtype_hint() -> &'static str {
    "instale o pacote wtype (NixOS: adicione pkgs.wtype ao environment.systemPackages; \
     Arch: pacman -S wtype; Debian/Ubuntu: apt install wtype)"
}

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

pub fn insert(text: &str, mode: InsertMode) -> Result<(), String> {
    let mut failures = Vec::new();
    let mut clipboard_ok = false;
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
                return finish(&failures);
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        match child.wait() {
            Ok(s) if s.success() => clipboard_ok = true,
            _ => failures.push("wl-copy falhou".to_string()),
        }
    }
    if clipboard_ok && !failures.is_empty() {
        failures.push("texto no clipboard — dá para colar manualmente".to_string());
    }
    finish(&failures)
}

fn finish(failures: &[String]) -> Result<(), String> {
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
    fn finish_returns_ok_when_no_failures() {
        assert_eq!(finish(&[]), Ok(()));
    }
    #[test]
    fn finish_joins_all_failures() {
        let failures = vec!["wtype: x".to_string(), "wl-copy: y".to_string()];
        assert_eq!(finish(&failures), Err("wtype: x; wl-copy: y".to_string()));
    }
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("whisper-wtype-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
    fn fake_wtype_dir(tag: &str, mode: u32) -> PathBuf {
        let dir = temp_dir(tag);
        let bin = dir.join("wtype");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        perms.set_mode(mode);
        std::fs::set_permissions(&bin, perms).unwrap();
        dir
    }
    #[test]
    fn finds_executable_wtype_in_path() {
        let dir = fake_wtype_dir("exec", 0o755);
        assert!(has_wtype_in(std::iter::once(dir.clone())));
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn ignores_non_executable_wtype() {
        let dir = fake_wtype_dir("noexec", 0o644);
        assert!(!has_wtype_in(std::iter::once(dir.clone())));
        let _ = std::fs::remove_dir_all(dir);
    }
    #[test]
    fn ignores_dir_without_wtype() {
        let dir = temp_dir("empty");
        assert!(!has_wtype_in(std::iter::once(dir.clone())));
        let _ = std::fs::remove_dir_all(dir);
    }
}
