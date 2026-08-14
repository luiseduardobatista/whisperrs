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

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::fs::OpenOptions;
use std::os::unix::process::CommandExt;
use std::process::{Command as Proc, Stdio};

use crate::config::{Config, InsertMode};

#[derive(Parser)]
#[command(name = "whisper", version, about = "Ditado por voz com whisper.cpp + Vulkan")]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sobe o daemon em background (não trava o shell; log em daemon.log).
    Start,
    /// Derruba o daemon.
    Stop,
    /// Sobe o daemon em primeiro plano (debug / systemd user service).
    Daemon,
    /// Inicia uma sessão de ditado; ativo = cancela (bind no compositor).
    Toggle,
    /// Mostra o estado do daemon.
    Status,
    /// Wizard de configuração: língua, modelo e download automático.
    Setup {
        #[arg(long)]
        lang: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Command::Start => start(),
        Command::Stop => {
            let resp = ipc::request(ipc::Cmd::Stop)?;
            print_response(&resp);
            Ok(())
        }
        Command::Daemon => daemon::run(),
        Command::Toggle => {
            let resp = ipc::request(ipc::Cmd::Toggle)?;
            print_response(&resp);
            Ok(())
        }
        Command::Status => {
            let resp = ipc::request(ipc::Cmd::Status)?;
            print_response(&resp);
            Ok(())
        }
        Command::Setup { lang, model } => setup::run(lang, model),
    }
}

/// Sobe o daemon destacado do terminal: sessão própria (sobrevive ao fechar o
/// shell), stdout/stderr para `~/.local/state/whisper/daemon.log`. Idempotente:
/// com o daemon já rodando (e sendo ESTE binário), apenas avisa.
fn start() -> Result<()> {
    let exe = std::env::current_exe().context("localizando o próprio executável")?;
    // Um daemon de outra origem (ex.: build de dev sem o wrapper do flake)
    // não tem wtype no PATH — a digitação quebraria. Diferente → reinicia.
    if let Ok(resp) = ipc::request(ipc::Cmd::Status) {
        let same = resp.exe.as_deref() == exe.to_str();
        if same {
            println!("whisper já está rodando");
            return Ok(());
        }
        println!("daemon de outra origem — reiniciando com este binário");
        let _ = ipc::request(ipc::Cmd::Stop);
        wait_daemon_exit()?;
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
        if ipc::request(ipc::Cmd::Status).is_ok() {
            println!("whisper rodando (log: {})", log.display());
            // Aviso imediato no console: o aviso do daemon só aparece no log.
            let cfg = Config::load().unwrap_or_default();
            if matches!(cfg.insert_mode, InsertMode::Type | InsertMode::Both)
                && !insert::wtype_available()
            {
                println!("aviso: wtype não encontrado no PATH — {}", insert::wtype_hint());
            }
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let _ = child.kill();
    bail!("daemon não respondeu; veja o log: {}", log.display())
}

/// Aguarda o daemon antigo remover o socket após o `stop` (até ~5 s).
fn wait_daemon_exit() -> Result<()> {
    for _ in 0..50 {
        if ipc::request(ipc::Cmd::Status).is_err() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    bail!("daemon antigo não saiu após o stop; verifique o log: {}", crate::config::log_path().display())
}

fn print_response(resp: &ipc::Response) {
    if let Some(err) = &resp.error {
        eprintln!("whisper: {err}");
        std::process::exit(1);
    }
    println!("{}", resp.state);
    if let Some(exe) = &resp.exe {
        // Origem do daemon — útil para detectar daemon de outra build.
        println!("daemon: {exe}");
    }
}
