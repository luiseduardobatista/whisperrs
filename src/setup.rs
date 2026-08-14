//! Wizard de configuração: língua, modelo e download automático.
use crate::config::{Config, InsertMode, Language};
use crate::insert;
use crate::model;
use anyhow::{Result, bail};
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;

const LANG_ITEMS: [&str; 3] = [
    "pt — português",
    "en — inglês",
    "auto — detectar automaticamente",
];

pub fn run(lang_flag: Option<String>, model_flag: Option<String>) -> Result<()> {
    let lang = match lang_flag.as_deref() {
        Some("pt") => Language::Pt,
        Some("en") => Language::En,
        Some("auto") => Language::Auto,
        Some(other) => bail!("língua inválida: {other} (use pt, en ou auto)"),
        None => {
            let idx = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("Língua padrão (pode forçar pt/en ou deixar auto-detect)")
                .default(0)
                .items(&LANG_ITEMS)
                .interact()?;
            Language::ALL[idx]
        }
    };

    let model = match model_flag.as_deref() {
        Some(name) => match model::find(name) {
            Some(m) => m,
            None => {
                bail!("modelo inválido: {name} (use tiny, base, small, medium, large-v3 ou turbo)")
            }
        },
        None => {
            let items: Vec<String> = model::MODELS
                .iter()
                .map(|m| format!("{} — {} MB", m.name, m.size_mb))
                .collect();
            let idx = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("Modelo (multilíngue; o download ocorre em seguida)")
                .default(model::MODELS.len() - 1)
                .items(&items)
                .interact()?;
            &model::MODELS[idx]
        }
    };

    let dest = model::models_dir().join(model.file);
    if dest.exists() {
        println!("modelo já baixado: {}", dest.display());
    } else {
        println!(
            "baixando {} (~{} MB) do HuggingFace…",
            model.file, model.size_mb
        );
        model::download(model, &dest)?;
    }

    let mut cfg = Config::load()?;
    cfg.language = lang;
    cfg.model = model.name.to_string();
    cfg.save()?;

    println!();
    println!(
        "configuração salva em {}",
        crate::config::config_path().display()
    );
    println!("modelo em {}", dest.display());
    println!();
    println!("para usar:");
    println!("  1. suba o daemon:  whisper daemon   (ou systemd user service)");
    println!("  2. bind no compositor, ex. Niri:");
    println!("     Mod+Shift+Space = spawn \"whisper toggle\"");
    println!("  3. fale e use no popup: Space pausar · Enter concluir · Esc cancelar");

    if matches!(cfg.insert_mode, InsertMode::Type | InsertMode::Both) && !insert::wtype_available()
    {
        println!();
        println!("aviso: 'wtype' não está no PATH — a digitação na app focada não vai funcionar");
        println!(
            "  (o texto ficará só no clipboard). {}",
            insert::wtype_hint()
        );
        println!("  Depois de instalar, reinicie o daemon (whisper stop && whisper start).");
    }
    Ok(())
}
