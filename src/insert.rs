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

#[derive(Debug, PartialEq, Eq)]
pub enum InsertOutcome {
    Inserted,
    Copied,
    Fallback { reason: String },
    InsertedAndCopied,
}

pub fn insert(text: &str, mode: InsertMode) -> Result<InsertOutcome, String> {
    insert_with(text, mode, attempt_insert, copy_to_clipboard)
}

fn insert_with<Insert, Copy>(
    text: &str,
    mode: InsertMode,
    mut attempt_insert: Insert,
    mut copy_to_clipboard: Copy,
) -> Result<InsertOutcome, String>
where
    Insert: FnMut(&str) -> Result<(), String>,
    Copy: FnMut(&str) -> Result<(), String>,
{
    match mode {
        InsertMode::InsertText => attempt_insert(text).map(|()| InsertOutcome::Inserted),
        InsertMode::CopyToClipboard => copy_to_clipboard(text).map(|()| InsertOutcome::Copied),
        InsertMode::Fallback => match attempt_insert(text) {
            Ok(()) => Ok(InsertOutcome::Inserted),
            Err(reason) => match copy_to_clipboard(text) {
                Ok(()) => Ok(InsertOutcome::Fallback { reason }),
                Err(copy_error) => Err(format!("{reason}; {copy_error}")),
            },
        },
        InsertMode::Both => {
            let insertion = attempt_insert(text);
            let clipboard = copy_to_clipboard(text);
            match (insertion, clipboard) {
                (Ok(()), Ok(())) => Ok(InsertOutcome::InsertedAndCopied),
                (Err(insert_error), Ok(())) => Err(format!(
                    "{insert_error}; texto no clipboard — dá para colar manualmente"
                )),
                (Ok(()), Err(copy_error)) => Err(copy_error),
                (Err(insert_error), Err(copy_error)) => {
                    Err(format!("{insert_error}; {copy_error}"))
                }
            }
        }
    }
}

fn attempt_insert(text: &str) -> Result<(), String> {
    let status = Command::new("wtype")
        .arg(text)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("wtype saiu com status {status}")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("wtype não encontrado no PATH — {}", wtype_hint()))
        }
        Err(error) => Err(format!("wtype: {error}")),
    }
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("wl-copy: {error}"))?;

    let write_result = child
        .stdin
        .take()
        .ok_or_else(|| "wl-copy não aceitou stdin".to_string())
        .and_then(|mut stdin| {
            stdin
                .write_all(text.as_bytes())
                .map_err(|error| error.to_string())
        });
    if let Err(error) = write_result {
        let _ = child.wait();
        return Err(format!("wl-copy: escrevendo texto: {error}"));
    }

    let status = child
        .wait()
        .map_err(|error| format!("wl-copy: aguardando processo: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("wl-copy saiu com status {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn insert_text_calls_only_wtype() {
        let calls = RefCell::new(Vec::new());
        let outcome = insert_with(
            "texto",
            InsertMode::InsertText,
            |text| {
                calls.borrow_mut().push(format!("wtype:{text}"));
                Ok(())
            },
            |text| {
                calls.borrow_mut().push(format!("wl-copy:{text}"));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(outcome, InsertOutcome::Inserted);
        assert_eq!(calls.into_inner(), ["wtype:texto"]);
    }

    #[test]
    fn insert_text_returns_wtype_failure_without_copying() {
        let calls = RefCell::new(Vec::new());
        let result = insert_with(
            "texto",
            InsertMode::InsertText,
            |_| {
                calls.borrow_mut().push("wtype");
                Err("wtype falhou".to_string())
            },
            |_| {
                calls.borrow_mut().push("wl-copy");
                Ok(())
            },
        );

        assert_eq!(result, Err("wtype falhou".to_string()));
        assert_eq!(calls.into_inner(), ["wtype"]);
    }

    #[test]
    fn copy_to_clipboard_calls_only_wl_copy() {
        let calls = RefCell::new(Vec::new());
        let outcome = insert_with(
            "texto",
            InsertMode::CopyToClipboard,
            |_| {
                calls.borrow_mut().push("wtype".to_string());
                Ok(())
            },
            |text| {
                calls.borrow_mut().push(format!("wl-copy:{text}"));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(outcome, InsertOutcome::Copied);
        assert_eq!(calls.into_inner(), ["wl-copy:texto"]);
    }

    #[test]
    fn fallback_does_not_copy_after_successful_insertion() {
        let calls = RefCell::new(Vec::new());
        let outcome = insert_with(
            "texto",
            InsertMode::Fallback,
            |_| {
                calls.borrow_mut().push("wtype");
                Ok(())
            },
            |_| {
                calls.borrow_mut().push("wl-copy");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(outcome, InsertOutcome::Inserted);
        assert_eq!(calls.into_inner(), ["wtype"]);
    }

    #[test]
    fn fallback_copies_after_insertion_failure() {
        let calls = RefCell::new(Vec::new());
        let outcome = insert_with(
            "texto",
            InsertMode::Fallback,
            |_| {
                calls.borrow_mut().push("wtype");
                Err("wtype falhou".to_string())
            },
            |_| {
                calls.borrow_mut().push("wl-copy");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            outcome,
            InsertOutcome::Fallback {
                reason: "wtype falhou".to_string()
            }
        );
        assert_eq!(calls.into_inner(), ["wtype", "wl-copy"]);
    }

    #[test]
    fn fallback_returns_real_error_when_both_operations_fail() {
        let result = insert_with(
            "texto",
            InsertMode::Fallback,
            |_| Err("wtype falhou".to_string()),
            |_| Err("wl-copy falhou".to_string()),
        );

        assert_eq!(result, Err("wtype falhou; wl-copy falhou".to_string()));
    }

    #[test]
    fn both_calls_both_operations_even_when_insertion_fails() {
        let calls = RefCell::new(Vec::new());
        let result = insert_with(
            "texto",
            InsertMode::Both,
            |_| {
                calls.borrow_mut().push("wtype");
                Err("wtype falhou".to_string())
            },
            |_| {
                calls.borrow_mut().push("wl-copy");
                Ok(())
            },
        );

        assert_eq!(calls.into_inner(), ["wtype", "wl-copy"]);
        assert_eq!(
            result,
            Err("wtype falhou; texto no clipboard — dá para colar manualmente".to_string())
        );
    }

    #[test]
    fn both_reports_success_when_both_operations_succeed() {
        let outcome = insert_with("texto", InsertMode::Both, |_| Ok(()), |_| Ok(())).unwrap();
        assert_eq!(outcome, InsertOutcome::InsertedAndCopied);
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
