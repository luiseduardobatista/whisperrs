//! whisper — ditado por voz com whisper.cpp (Vulkan) + OSD Wayland.
mod audio;
mod config;
mod daemon;
mod insert;
mod ipc;
mod model;
mod osd;
mod postprocess;
mod setup;
mod transcribe;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "whisper", version, about = "Ditado por voz com whisper.cpp + Vulkan")]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Sobe o daemon (systemd user service / autostart do compositor).
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

fn print_response(resp: &ipc::Response) {
    if let Some(err) = &resp.error {
        eprintln!("whisper: {err}");
        std::process::exit(1);
    }
    println!("{}", resp.state);
}
