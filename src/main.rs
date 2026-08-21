//! whisper — ditado por voz com whisper.cpp (Vulkan) + OSD Wayland.
mod audio;
mod config;
mod daemon;
mod insert;
mod ipc;
mod model;
mod osd;
mod osd_draw;
mod postprocess;
mod setup;
mod transcribe;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::Serialize;
use std::fs::OpenOptions;
use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Command as Proc, Stdio};
use std::time::Duration;

use crate::config::Config;

#[derive(Parser)]
#[command(
    name = "whisper",
    version,
    about = "Ditado por voz com whisper.cpp + Vulkan"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sobe o daemon em background (não trava o shell; log em daemon.log).
    Start,
    /// Derruba o daemon; é sucesso se ele já estiver parado.
    Stop,
    /// Reinicia o daemon reutilizando o lifecycle de `stop` e `start`.
    Restart,
    /// Sobe o daemon em primeiro plano (debug / systemd user service).
    Daemon,
    Toggle,
    Record,
    Commit,
    Cancel,
    Pause,
    /// Mostra o estado do daemon.
    Status {
        /// Emite somente JSON em stdout, adequado para scripts e status bars.
        #[arg(long)]
        json: bool,
    },
    /// Onboarding: escolhas, resumo e download automático dos modelos.
    Setup {
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long = "insert-mode")]
        insert_mode: Option<String>,
        /// Aceita o resumo sem confirmação; nunca inicia o daemon.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

#[derive(Debug, Serialize, PartialEq)]
struct StatusOutput {
    daemon: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exe: Option<String>,
}

impl StatusOutput {
    fn running(response: &ipc::Response) -> Self {
        StatusOutput {
            daemon: "running",
            state: Some(response.state.clone()),
            language: response.language.clone(),
            model: response.model.clone(),
            exe: response.exe.clone(),
        }
    }

    fn stopped() -> Self {
        StatusOutput {
            daemon: "stopped",
            state: None,
            language: None,
            model: None,
            exe: None,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Start => start(),
        Command::Stop => stop(),
        Command::Restart => restart(),
        Command::Daemon => daemon::run(),
        Command::Toggle => request_command(ipc::Cmd::Toggle),
        Command::Record => request_command(ipc::Cmd::Record),
        Command::Commit => request_command(ipc::Cmd::Commit),
        Command::Cancel => request_command(ipc::Cmd::Cancel),
        Command::Pause => request_command(ipc::Cmd::Pause),
        Command::Status { json } => status(json),
        Command::Setup {
            lang,
            model,
            insert_mode,
            yes,
        } => {
            let start_daemon = setup::run(setup::SetupOptions {
                lang,
                model,
                insert_mode,
                yes,
            })?;
            if start_daemon { start() } else { Ok(()) }
        }
    }
}

fn request_command(cmd: ipc::Cmd) -> Result<()> {
    let resp = ipc::request(cmd)?;
    print_response(&resp)
}

fn stop() -> Result<()> {
    if request_stop_and_wait()? {
        println!("whisper parado");
    } else {
        println!("whisper já está parado");
    }
    Ok(())
}

fn restart() -> Result<()> {
    request_stop_and_wait()?;
    start()
}

fn request_stop_and_wait() -> Result<bool> {
    let stopped = request_stop()?;
    wait_daemon_exit()?;
    Ok(stopped)
}

fn status(json: bool) -> Result<()> {
    let response = match ipc::request_status() {
        Ok(response) => response,
        Err(error) if daemon_unavailable(&error) => {
            let output = StatusOutput::stopped();
            if json {
                println!("{}", serde_json::to_string(&output)?);
            } else {
                println!("stopped");
            }
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    ensure_response_ok(&response)?;

    if json {
        println!(
            "{}",
            serde_json::to_string(&StatusOutput::running(&response))?
        );
    } else {
        print_response(&response)?;
    }
    Ok(())
}

/// Solicita o encerramento do daemon. Retorna `false` quando não há daemon
/// atendendo no socket, pois esse já é o estado desejado para `stop`.
fn request_stop() -> Result<bool> {
    match ipc::request(ipc::Cmd::Stop) {
        Ok(response) => {
            ensure_response_ok(&response)?;
            Ok(true)
        }
        Err(error) if daemon_unavailable(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Sobe o daemon destacado do terminal: sessão própria (sobrevive ao fechar o
/// shell), stdout/stderr para `~/.local/state/whisper/daemon.log`. Idempotente:
/// com o daemon já rodando (e sendo ESTE binário), apenas avisa.
fn start() -> Result<()> {
    let exe = std::env::current_exe().context("localizando o próprio executável")?;
    // Um daemon de outra origem (ex.: build de dev sem o wrapper do flake)
    // não tem wtype no PATH — a digitação quebraria. Diferente → reinicia.
    match ipc::request_status() {
        Ok(resp) => {
            ensure_response_ok(&resp)?;
            let same = resp.exe.as_deref() == exe.to_str();
            if same {
                println!("whisper já está rodando");
                return Ok(());
            }
            println!("daemon de outra origem — reiniciando com este binário");
            request_stop()?;
            wait_daemon_exit()?;
        }
        Err(error) if daemon_unavailable(&error) => {}
        Err(error) => return Err(error).context("consultando o status do daemon"),
    }
    let log = crate::config::log_path();
    if let Some(parent) = log.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .with_context(|| format!("abrindo log {}", log.display()))?;
    let mut cmd = Proc::new(exe);
    cmd.arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::from(file.try_clone()?))
        .stderr(Stdio::from(file))
        .process_group(0); // processo próprio: não morre com o terminal
    let mut child = cmd.spawn().context("subindo o daemon")?;
    // Confirma que o daemon respondeu no socket antes de devolver o shell.
    for _ in 0..10 {
        if ipc::request_status()
            .and_then(|resp| {
                ensure_response_ok(&resp)?;
                Ok(resp)
            })
            .is_ok()
        {
            println!("whisper rodando (log: {})", log.display());
            // Aviso imediato no console: o aviso do daemon só aparece no log.
            let cfg = Config::load().unwrap_or_default();
            if cfg.insert_mode.uses_wtype() && !insert::wtype_available() {
                eprintln!(
                    "aviso: wtype não encontrado no PATH — {}",
                    insert::wtype_hint()
                );
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let _ = child.kill();
    bail!("daemon não respondeu; veja o log: {}", log.display())
}

/// Aguarda o daemon antigo remover o socket após o `stop` (até ~5 s).
fn wait_daemon_exit() -> Result<()> {
    for _ in 0..50 {
        match ipc::request_status() {
            Ok(_) => std::thread::sleep(Duration::from_millis(100)),
            Err(error) if daemon_unavailable(&error) => return Ok(()),
            Err(error) => return Err(error).context("aguardando o daemon encerrar"),
        }
    }
    bail!(
        "daemon antigo não saiu após o stop; verifique o log: {}",
        crate::config::log_path().display()
    )
}

fn ensure_response_ok(resp: &ipc::Response) -> Result<()> {
    if let Some(err) = &resp.error {
        bail!("whisper: {err}");
    }
    Ok(())
}

fn print_response(resp: &ipc::Response) -> Result<()> {
    ensure_response_ok(resp)?;
    println!("{}", resp.state);
    if let Some(exe) = &resp.exe {
        // Origem do daemon — útil para detectar daemon de outra build.
        println!("daemon: {exe}");
    }
    Ok(())
}

fn daemon_unavailable(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        if let Some(io_error) = cause.downcast_ref::<io::Error>() {
            return matches!(
                io_error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::ConnectionRefused
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::BrokenPipe
            );
        }
        cause
            .downcast_ref::<serde_json::Error>()
            .is_some_and(serde_json::Error::is_eof)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_json_accepts_only_the_status_command() {
        let cli = Cli::try_parse_from(["whisper", "status", "--json"]).unwrap();

        assert!(matches!(cli.cmd, Command::Status { json: true }));
    }

    #[test]
    fn operational_commands_reject_status_json_flag() {
        assert!(Cli::try_parse_from(["whisper", "toggle", "--json"]).is_err());
    }

    #[test]
    fn setup_accepts_scriptable_choices() {
        let cli = Cli::try_parse_from([
            "whisper",
            "setup",
            "--lang",
            "pt",
            "--model",
            "small",
            "--insert-mode",
            "both",
            "--yes",
        ])
        .unwrap();

        assert!(matches!(cli.cmd, Command::Setup { yes: true, .. }));
    }

    #[test]
    fn stopped_status_serializes_without_session_fields() {
        let output = StatusOutput::stopped();

        assert_eq!(
            serde_json::to_string(&output).unwrap(),
            r#"{"daemon":"stopped"}"#
        );
    }

    #[test]
    fn running_status_serializes_daemon_metadata() {
        let response = ipc::Response {
            state: "recording".to_string(),
            error: None,
            exe: Some("/bin/whisper".to_string()),
            language: Some("pt".to_string()),
            model: Some("small".to_string()),
        };

        let output = StatusOutput::running(&response);

        assert_eq!(
            serde_json::to_string(&output).unwrap(),
            r#"{"daemon":"running","state":"recording","language":"pt","model":"small","exe":"/bin/whisper"}"#
        );
    }

    #[test]
    fn connection_refused_is_a_stopped_daemon() {
        let error = anyhow::Error::new(io::Error::from(io::ErrorKind::ConnectionRefused));

        assert!(daemon_unavailable(&error));
    }

    #[test]
    fn permission_errors_are_not_reported_as_stopped() {
        let error = anyhow::Error::new(io::Error::from(io::ErrorKind::PermissionDenied));

        assert!(!daemon_unavailable(&error));
    }

    #[test]
    fn end_of_response_is_treated_as_shutdown() {
        let json_error = serde_json::from_str::<serde_json::Value>("").unwrap_err();
        let error = anyhow::Error::new(json_error);

        assert!(daemon_unavailable(&error));
    }

    #[test]
    fn malformed_response_is_not_treated_as_shutdown() {
        let json_error = serde_json::from_str::<serde_json::Value>("not-json").unwrap_err();
        let error = anyhow::Error::new(json_error);

        assert!(!daemon_unavailable(&error));
    }
}
