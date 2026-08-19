use crate::config::{self, Config, InsertMode, Language};
use crate::insert;
use crate::model;
use anyhow::{Result, bail};
use dialoguer::Confirm;
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};

const LANG_ITEMS: [&str; 3] = ["Português", "Inglês", "Detectar automaticamente"];

const INSERT_ITEMS: [&str; 4] = [
    "Digitar automaticamente; clipboard só se falhar (recomendado)",
    "Digitar automaticamente",
    "Somente copiar para o clipboard",
    "Digitar e copiar para o clipboard",
];

#[derive(Debug, Default)]
pub struct SetupOptions {
    pub lang: Option<String>,
    pub model: Option<String>,
    pub ai_model: Option<String>,
    pub insert_mode: Option<String>,
    pub no_ai: bool,
    pub yes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownloadKind {
    Whisper,
    Vad,
    Qwen,
}

struct PendingDownload {
    kind: DownloadKind,
    spec: &'static model::ModelSpec,
    dest: PathBuf,
    label: &'static str,
}

struct SetupPlan {
    cfg: Config,
    model: &'static model::ModelSpec,
    model_dest: PathBuf,
    vad_dest: PathBuf,
    pending: Vec<PendingDownload>,
}

#[derive(Debug, Default)]
struct DownloadReport {
    vad_downloaded: bool,
}

pub fn run(options: SetupOptions) -> Result<bool> {
    let interactive = is_interactive();
    let config_exists = config::config_path().is_file();
    let mut cfg = Config::load()?;

    let lang = select_language(options.lang.as_deref(), cfg.language, interactive)?;
    let model = select_model(options.model.as_deref(), &cfg.model, interactive)?;
    let insert_mode =
        select_insert_mode(options.insert_mode.as_deref(), cfg.insert_mode, interactive)?;
    let (ai_enabled, download_qwen) = select_ai(&options, &mut cfg, config_exists, interactive)?;

    cfg.language = lang;
    cfg.model = model.name.to_string();
    cfg.insert_mode = insert_mode;
    cfg.ai.enabled = ai_enabled;

    let plan = SetupPlan::new(cfg, model, download_qwen)?;
    print_summary(&plan);

    if !confirm_downloads(&plan, &options, interactive)? {
        println!("setup cancelado.");
        return Ok(false);
    }

    let report = execute_downloads(&plan)?;
    plan.cfg.save()?;
    print_completion(&plan, &report);

    if interactive && !options.yes && !all_choices_provided(&options) {
        return Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("Iniciar o daemon agora?")
            .default(false)
            .interact()
            .map_err(Into::into);
    }

    Ok(false)
}

fn is_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

fn select_language(flag: Option<&str>, current: Language, interactive: bool) -> Result<Language> {
    if let Some(value) = flag {
        return parse_language(value);
    }
    if !interactive {
        return Ok(current);
    }

    let default = Language::ALL
        .iter()
        .position(|language| *language == current)
        .unwrap_or(0);
    let index = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Idioma do ditado")
        .default(default)
        .items(&LANG_ITEMS)
        .interact()?;
    Ok(Language::ALL[index])
}

fn parse_language(value: &str) -> Result<Language> {
    match value {
        "pt" => Ok(Language::Pt),
        "en" => Ok(Language::En),
        "auto" => Ok(Language::Auto),
        other => bail!("língua inválida: {other} (use pt, en ou auto)"),
    }
}

fn select_model(
    flag: Option<&str>,
    current: &str,
    interactive: bool,
) -> Result<&'static model::ModelSpec> {
    let current = model::find(current)
        .or_else(|| model::find(config::DEFAULT_MODEL))
        .ok_or_else(|| anyhow::anyhow!("catálogo sem o modelo recomendado"))?;

    if let Some(value) = flag {
        return model::find(value).ok_or_else(|| {
            anyhow::anyhow!(
                "modelo inválido: {value} (use tiny, base, small, medium, large-v3 ou turbo)"
            )
        });
    }
    if !interactive {
        return Ok(current);
    }

    let items: Vec<String> = model::MODELS.iter().map(model_item_label).collect();
    let default = model::MODELS
        .iter()
        .position(|candidate| candidate.name == current.name)
        .unwrap_or(0);
    let index = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Modelo de transcrição")
        .default(default)
        .items(&items)
        .interact()?;
    Ok(&model::MODELS[index])
}

fn model_item_label(spec: &model::ModelSpec) -> String {
    format!(
        "{} — {} (~{} MB)",
        spec.name, spec.description, spec.size_mb
    )
}

fn select_insert_mode(
    flag: Option<&str>,
    current: InsertMode,
    interactive: bool,
) -> Result<InsertMode> {
    if let Some(value) = flag {
        return parse_insert_mode(value);
    }
    if !interactive {
        return Ok(current);
    }

    let default = insert_mode_index(current);
    let index = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("Como inserir o texto?")
        .default(default)
        .items(&INSERT_ITEMS)
        .interact()?;
    Ok(insert_mode_at(index))
}

fn parse_insert_mode(value: &str) -> Result<InsertMode> {
    InsertMode::parse(value).ok_or_else(|| {
        anyhow::anyhow!(
            "modo de inserção inválido: {value} (use insert, clipboard, fallback ou both; type é alias legado)"
        )
    })
}

fn insert_mode_index(mode: InsertMode) -> usize {
    match mode {
        InsertMode::Fallback => 0,
        InsertMode::InsertText => 1,
        InsertMode::CopyToClipboard => 2,
        InsertMode::Both => 3,
    }
}

fn insert_mode_at(index: usize) -> InsertMode {
    match index {
        0 => InsertMode::Fallback,
        1 => InsertMode::InsertText,
        2 => InsertMode::CopyToClipboard,
        _ => InsertMode::Both,
    }
}

fn select_ai(
    options: &SetupOptions,
    cfg: &mut Config,
    config_exists: bool,
    interactive: bool,
) -> Result<(bool, bool)> {
    if options.no_ai {
        return Ok((false, false));
    }

    if let Some(name) = options.ai_model.as_deref() {
        let spec = model::find_llm(name)
            .ok_or_else(|| anyhow::anyhow!("modelo AI inválido: {name} (use qwen3.5-0.8b)"))?;
        cfg.ai.model = spec.name.to_string();
        return Ok((true, true));
    }

    if interactive {
        let current_enabled = cfg.ai.enabled && cfg.ai_model_path().is_some();
        let want_ai = Confirm::with_theme(&ColorfulTheme::default())
            .with_prompt("Ativar pós-processamento inteligente local com Qwen (~650 MB)?")
            .default(current_enabled)
            .interact()?;
        if want_ai {
            ensure_catalog_ai_model(cfg)?;
            return Ok((true, true));
        }
        return Ok((false, false));
    }

    if config_exists {
        return Ok((cfg.ai.enabled, false));
    }

    Ok((false, false))
}

fn ensure_catalog_ai_model(cfg: &mut Config) -> Result<()> {
    if model::find_llm(&cfg.ai.model).is_none() && cfg.ai_model_path().is_none() {
        let spec = model::LLM_MODELS
            .first()
            .ok_or_else(|| anyhow::anyhow!("catálogo AI vazio"))?;
        cfg.ai.model = spec.name.to_string();
    }
    Ok(())
}

impl SetupPlan {
    fn new(cfg: Config, model: &'static model::ModelSpec, download_qwen: bool) -> Result<Self> {
        let model_dest = model::models_dir().join(model.file);
        let vad_dest = model::vad_model_path();
        let mut pending = Vec::new();

        add_pending(
            &mut pending,
            DownloadKind::Whisper,
            model,
            model_dest.clone(),
            "modelo Whisper",
        );
        add_pending(
            &mut pending,
            DownloadKind::Vad,
            &model::VAD_MODEL,
            vad_dest.clone(),
            "modelo VAD",
        );

        if cfg.ai.enabled && download_qwen {
            if let Some(spec) = model::find_llm(&cfg.ai.model) {
                add_pending(
                    &mut pending,
                    DownloadKind::Qwen,
                    spec,
                    model::models_dir().join(spec.file),
                    "modelo Qwen",
                );
            } else if cfg.ai_model_path().is_none() {
                bail!(
                    "modelo Qwen não encontrado: {}; desative AI ou escolha qwen3.5-0.8b",
                    cfg.ai.model
                );
            }
        }

        Ok(Self {
            cfg,
            model,
            model_dest,
            vad_dest,
            pending,
        })
    }
}

fn add_pending(
    pending: &mut Vec<PendingDownload>,
    kind: DownloadKind,
    spec: &'static model::ModelSpec,
    dest: PathBuf,
    label: &'static str,
) {
    if !dest.exists() {
        pending.push(PendingDownload {
            kind,
            spec,
            dest,
            label,
        });
    }
}

fn print_summary(plan: &SetupPlan) {
    println!();
    println!("Configuração");
    println!();
    println!("  Idioma       {}", plan.cfg.language.label());
    println!(
        "  Modelo       {} — {}",
        plan.model.name, plan.model.description
    );
    println!("  Inserção     {}", plan.cfg.insert_mode.label());
    if plan.cfg.ai.enabled {
        println!("  Qwen cleanup Ativado ({})", plan.cfg.ai.model);
    } else {
        println!("  Qwen cleanup Desativado");
    }

    if plan.pending.is_empty() {
        println!();
        println!("  Downloads pendentes: nenhum");
    } else {
        println!();
        println!("Downloads pendentes:");
        for download in &plan.pending {
            println!("  {} — ~{} MB", download.label, download.spec.size_mb);
        }
        println!(
            "  Total aproximado: ~{} MB",
            pending_download_total(&plan.pending)
        );
    }
    println!();
}

fn pending_download_total(pending: &[PendingDownload]) -> u64 {
    pending.iter().map(|download| download.spec.size_mb).sum()
}

fn confirm_downloads(plan: &SetupPlan, options: &SetupOptions, interactive: bool) -> Result<bool> {
    if plan.pending.is_empty() || options.yes || all_choices_provided(options) {
        return Ok(true);
    }
    if !interactive {
        bail!("setup não interativo precisa de --yes ou de todas as escolhas em flags");
    }
    Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt("Continuar com esta configuração?")
        .default(true)
        .interact()
        .map_err(Into::into)
}

fn all_choices_provided(options: &SetupOptions) -> bool {
    options.lang.is_some()
        && options.model.is_some()
        && options.insert_mode.is_some()
        && (options.ai_model.is_some() || options.no_ai)
}

fn execute_downloads(plan: &SetupPlan) -> Result<DownloadReport> {
    let mut report = DownloadReport::default();
    for download in &plan.pending {
        match download.kind {
            DownloadKind::Whisper | DownloadKind::Qwen => {
                ensure_downloaded(download.spec, &download.dest, download.label)?;
            }
            DownloadKind::Vad => {
                match ensure_downloaded(download.spec, &download.dest, download.label) {
                    Ok(_) => report.vad_downloaded = true,
                    Err(error) => println!(
                        "aviso: falha ao baixar o modelo VAD: {error:#}\n  o ditado funciona sem filtro de voz; rode 'whisper setup' de novo para tentar."
                    ),
                }
            }
        }
    }
    Ok(report)
}

fn print_completion(plan: &SetupPlan, report: &DownloadReport) {
    println!("✓ Configuração concluída");
    println!();
    println!("Configuração salva em {}", config::config_path().display());
    println!("Modelo       {}", plan.model.name);
    println!("Modelo em    {}", plan.model_dest.display());
    println!("VAD em       {}", plan.vad_dest.display());
    println!("Inserção     {}", plan.cfg.insert_mode.label());
    println!(
        "Qwen cleanup {}",
        if plan.cfg.ai.enabled {
            format!("ativado ({})", plan.cfg.ai.model)
        } else {
            "desativado".to_string()
        }
    );
    println!();
    println!("Próximo passo:");
    println!("  whisper start");

    if report.vad_downloaded {
        println!();
        println!(
            "aviso: se o daemon já estiver ativo, reinicie-o para carregar o novo VAD (whisper restart)."
        );
    }

    if plan.cfg.insert_mode.uses_wtype() && !insert::wtype_available() {
        println!();
        println!("aviso: 'wtype' não está no PATH — a digitação na app focada não vai funcionar");
        println!("  o texto ficará só no clipboard. {}", insert::wtype_hint());
        println!("  Depois de instalar, reinicie o daemon (whisper restart).");
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommended_model_is_small() {
        assert_eq!(config::DEFAULT_MODEL, "small");
        assert_eq!(model::find(config::DEFAULT_MODEL).unwrap().name, "small");
    }

    #[test]
    fn insert_mode_values_have_stable_meaning() {
        assert_eq!(parse_insert_mode("insert").unwrap(), InsertMode::InsertText);
        assert_eq!(parse_insert_mode("type").unwrap(), InsertMode::InsertText);
        assert_eq!(
            parse_insert_mode("clipboard").unwrap(),
            InsertMode::CopyToClipboard
        );
        assert_eq!(parse_insert_mode("fallback").unwrap(), InsertMode::Fallback);
        assert_eq!(parse_insert_mode("both").unwrap(), InsertMode::Both);

        for mode in [
            InsertMode::Fallback,
            InsertMode::InsertText,
            InsertMode::CopyToClipboard,
            InsertMode::Both,
        ] {
            assert_eq!(insert_mode_at(insert_mode_index(mode)), mode);
        }
    }

    #[test]
    fn invalid_insert_mode_is_rejected() {
        assert!(parse_insert_mode("invalid").is_err());
    }

    #[test]
    fn explicit_ai_model_enables_download_without_interaction() {
        let mut cfg = Config::default();
        let options = SetupOptions {
            ai_model: Some("qwen3.5-0.8b".to_string()),
            ..SetupOptions::default()
        };

        assert_eq!(
            select_ai(&options, &mut cfg, false, false).unwrap(),
            (true, true)
        );
    }

    #[test]
    fn noninteractive_setup_preserves_existing_ai_choice() {
        let mut cfg = Config::default();
        cfg.ai.enabled = true;
        let options = SetupOptions::default();

        assert_eq!(
            select_ai(&options, &mut cfg, true, false).unwrap(),
            (true, false)
        );
    }

    #[test]
    fn direct_ai_model_path_is_not_replaced_by_catalog_model() {
        let path = std::env::temp_dir().join(format!(
            "whisper-setup-ai-{}-{}.gguf",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"modelo").unwrap();

        let mut cfg = Config::default();
        cfg.ai.model = path.display().to_string();
        ensure_catalog_ai_model(&mut cfg).unwrap();

        assert_eq!(cfg.ai.model, path.display().to_string());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn pending_download_total_sums_selected_components() {
        let pending = vec![
            PendingDownload {
                kind: DownloadKind::Whisper,
                spec: model::find("small").unwrap(),
                dest: PathBuf::from("/tmp/small"),
                label: "modelo Whisper",
            },
            PendingDownload {
                kind: DownloadKind::Vad,
                spec: &model::VAD_MODEL,
                dest: PathBuf::from("/tmp/vad"),
                label: "modelo VAD",
            },
        ];

        assert_eq!(pending_download_total(&pending), 467);
    }
}
