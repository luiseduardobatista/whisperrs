//! Wizard de configuração: língua, modelo e download automático.
use crate::config::{Config, InsertMode, Language};
use crate::insert;
use crate::model;
use anyhow::{Result, bail};
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;
use std::path::Path;

const LANG_ITEMS: [&str; 3] = [
    "pt — português",
    "en — inglês",
    "auto — detectar automaticamente",
];

pub fn run(
    lang_flag: Option<String>,
    model_flag: Option<String>,
    ai_model_flag: Option<String>,
) -> Result<()> {
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
    let mut downloaded_anything = ensure_downloaded(model, &dest, "modelo")?;
    let mut downloaded_engine_model = downloaded_anything;
    // Falha do VAD não aborta o setup: o daemon degrada com aviso no log/OSD
    // (mesmo fallback documentado) e o usuário pode tentar de novo depois.
    let vad_dest = model::vad_model_path();
    match ensure_downloaded(&model::VAD_MODEL, &vad_dest, "modelo VAD") {
        Ok(downloaded) => {
            downloaded_anything |= downloaded;
            downloaded_engine_model |= downloaded;
        }
        Err(e) => println!(
            "aviso: falha ao baixar o modelo VAD: {e:#}\n  o ditado funciona sem filtro de voz; \
             rode 'whisper setup' de novo para tentar."
        ),
    }

    let ai_info = if let Some(name) = ai_model_flag.as_deref() {
        let spec = model::find_llm(name)
            .ok_or_else(|| anyhow::anyhow!("modelo AI inválido: {name} (use qwen3.5-0.8b)"))?;
        let dest = model::models_dir().join(spec.file);
        let downloaded = ensure_downloaded(spec, &dest, "modelo Qwen")?;
        downloaded_anything |= downloaded;
        Some((name.to_string(), dest))
    } else {
        None
    };

    let mut cfg = Config::load()?;
    cfg.language = lang;
    cfg.model = model.name.to_string();
    if let Some((name, _)) = &ai_info {
        cfg.ai.model = name.clone();
    }
    cfg.save()?;

    println!();
    println!(
        "configuração salva em {}",
        crate::config::config_path().display()
    );
    println!("modelo em {}", dest.display());
    println!("modelo VAD em {}", vad_dest.display());
    if let Some((_, ai_dest)) = &ai_info {
        println!(
            "modelo Qwen em {} (carregado sob demanda; não é necessário reiniciar o daemon)",
            ai_dest.display()
        );
    }
    println!();
    println!("para usar:");
    println!("  1. suba o daemon:  whisper daemon   (ou systemd user service)");
    println!("  2. bind no compositor, ex. Niri:");
    println!("     Mod+Shift+Space = spawn \"whisper toggle\"");
    println!("  3. fale e use no popup: Space pausar · Enter concluir · Esc cancelar");

    if downloaded_anything && downloaded_engine_model {
        println!();
        println!("aviso: se o daemon já estiver ativo com o modelo carregado,");
        println!("  reinicie-o (whisper stop && whisper start) para o modelo novo");
        println!("  (e o VAD) serem usados.");
    }

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

/// Garante o arquivo baixado (idempotente); retorna `true` se baixou agora.
fn ensure_downloaded(spec: &model::ModelSpec, dest: &Path, label: &str) -> Result<bool> {
    if dest.exists() {
        println!("{label} já baixado: {}", dest.display());
        return Ok(false);
    }
    println!(
        "baixando {label} {} (~{} MB) do HuggingFace…",
        spec.file, spec.size_mb
    );
    model::download(spec, dest)?;
    Ok(true)
}
