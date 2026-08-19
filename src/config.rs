//! Configuração persistida em TOML (~/.config/whisper/config.toml).
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_MODEL: &str = "small";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Pt,
    En,
    Auto,
}

impl Language {
    pub const ALL: [Language; 3] = [Language::Pt, Language::En, Language::Auto];

    pub fn label(self) -> &'static str {
        match self {
            Language::Pt => "pt",
            Language::En => "en",
            Language::Auto => "auto",
        }
    }

    /// Código ISO para o whisper; `None` = auto-detect.
    pub fn whisper_code(self) -> Option<&'static str> {
        match self {
            Language::Pt => Some("pt"),
            Language::En => Some("en"),
            Language::Auto => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InsertMode {
    #[serde(rename = "insert", alias = "type")]
    InsertText,
    #[serde(rename = "clipboard")]
    CopyToClipboard,
    #[serde(rename = "fallback")]
    Fallback,
    #[serde(rename = "both")]
    Both,
}

impl InsertMode {
    pub fn label(self) -> &'static str {
        match self {
            InsertMode::InsertText => "Digitar automaticamente",
            InsertMode::CopyToClipboard => "Somente copiar para o clipboard",
            InsertMode::Fallback => "Digitar automaticamente; clipboard só se falhar (recomendado)",
            InsertMode::Both => "Digitar e copiar para o clipboard",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "insert" | "type" => Some(InsertMode::InsertText),
            "clipboard" => Some(InsertMode::CopyToClipboard),
            "fallback" => Some(InsertMode::Fallback),
            "both" => Some(InsertMode::Both),
            _ => None,
        }
    }

    pub fn uses_wtype(self) -> bool {
        matches!(
            self,
            InsertMode::InsertText | InsertMode::Fallback | InsertMode::Both
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiConfig {
    pub enabled: bool,
    pub model: String,
    pub context_size: u32,
    pub gpu: bool,
    pub cleanup: bool,
}

impl Default for AiConfig {
    fn default() -> Self {
        AiConfig {
            enabled: false,
            model: "qwen3.5-2b".to_string(),
            context_size: 2048,
            gpu: true,
            cleanup: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub language: Language,
    pub model: String,
    pub insert_mode: InsertMode,
    pub remove_fillers: bool,
    pub punctuation: bool,
    pub final_period: bool,
    pub gpu_device: i32,
    pub threads: u32,
    /// Nome do nó de áudio do PipeWire (pw-record --target); `None` = fonte padrão.
    pub source: Option<String>,
    #[serde(default)]
    pub ai: AiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: Language::Pt,
            model: DEFAULT_MODEL.to_string(),
            insert_mode: InsertMode::Fallback,
            remove_fillers: true,
            punctuation: true,
            final_period: true,
            gpu_device: 0,
            threads: 4,
            source: None,
            ai: AiConfig::default(),
        }
    }
}

impl Config {
    pub fn load() -> Result<Config> {
        let path = config_path();
        if !path.exists() {
            return Ok(Config::default());
        }
        let raw =
            std::fs::read_to_string(&path).with_context(|| format!("lendo {}", path.display()))?;
        let cfg: Config =
            toml::from_str(&raw).with_context(|| format!("parseando {}", path.display()))?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        std::fs::create_dir_all(path.parent().expect("config dir"))?;
        std::fs::write(&path, toml::to_string_pretty(self)?)
            .with_context(|| format!("escrevendo {}", path.display()))?;
        Ok(())
    }

    pub fn model_path(&self) -> PathBuf {
        crate::model::models_dir().join(crate::model::file_name(&self.model).unwrap_or(""))
    }

    /// Caminho do modelo LLM: nome do catálogo ou caminho absoluto; `None` se
    /// o arquivo não existe (o daemon degrada para o fallback Rust).
    pub fn ai_model_path(&self) -> Option<PathBuf> {
        let path = if self.ai.model.contains('/') {
            PathBuf::from(&self.ai.model)
        } else {
            let spec = crate::model::find_llm(&self.ai.model)?;
            crate::model::models_dir().join(spec.file)
        };
        path.is_file().then_some(path)
    }
}

pub fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("whisper")
}

pub fn data_dir() -> PathBuf {
    xdg("XDG_DATA_HOME", ".local/share").join("whisper")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Última modificação do arquivo de config; `None` se o arquivo não existe
/// (usado pelo hot reload do daemon para detectar mudanças).
pub fn config_mtime() -> Option<std::time::SystemTime> {
    std::fs::metadata(config_path())
        .and_then(|m| m.modified())
        .ok()
}

pub fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("whisper")
}

pub fn log_path() -> PathBuf {
    state_dir().join("daemon.log")
}

pub fn socket_path() -> PathBuf {
    xdg("XDG_RUNTIME_DIR", "/tmp").join("whisper.sock")
}

fn xdg(env: &str, default: &str) -> PathBuf {
    match std::env::var(env) {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(home).join(default)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_file_with_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.language, Language::Pt);
        assert_eq!(cfg.model, DEFAULT_MODEL);
        assert!(cfg.punctuation);
        assert_eq!(cfg.insert_mode, InsertMode::Fallback);
        assert!(!cfg.ai.enabled);
        assert_eq!(cfg.ai, AiConfig::default());
    }

    #[test]
    fn insert_mode_reports_when_wtype_is_needed() {
        assert!(InsertMode::InsertText.uses_wtype());
        assert!(InsertMode::Fallback.uses_wtype());
        assert!(InsertMode::Both.uses_wtype());
        assert!(!InsertMode::CopyToClipboard.uses_wtype());
    }

    #[test]
    fn insert_mode_accepts_legacy_alias_and_serializes_canonical_value() {
        let cfg: Config = toml::from_str("insert_mode = \"type\"").unwrap();
        assert_eq!(cfg.insert_mode, InsertMode::InsertText);

        let serialized = toml::to_string(&cfg).unwrap();
        assert!(serialized.contains("insert_mode = \"insert\""));
    }

    #[test]
    fn insert_mode_parses_all_canonical_values() {
        for (value, expected) in [
            ("insert", InsertMode::InsertText),
            ("clipboard", InsertMode::CopyToClipboard),
            ("fallback", InsertMode::Fallback),
            ("both", InsertMode::Both),
        ] {
            let cfg: Config = toml::from_str(&format!("insert_mode = \"{value}\"")).unwrap();
            assert_eq!(cfg.insert_mode, expected);
        }
    }

    #[test]
    fn parse_complete_ai_config() {
        let cfg: Config = toml::from_str(
            r#"
            [ai]
            enabled = false
            model = "qwen3.5-2b"
            context_size = 4096
            gpu = false
            cleanup = false
            "#,
        )
        .unwrap();
        assert!(!cfg.ai.enabled);
        assert_eq!(cfg.ai.model, "qwen3.5-2b");
        assert_eq!(cfg.ai.context_size, 4096);
        assert!(!cfg.ai.gpu);
        assert!(!cfg.ai.cleanup);
    }

    /// Aponta `XDG_DATA_HOME` para um diretório temporário enquanto vive: os
    /// testes não podem tocar o estado real do usuário (nem o `$HOME`
    /// read-only do sandbox do Nix). Env var é global do processo e os testes
    /// rodam em paralelo, então serializa com um mutex estático.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct TempDataHome {
        dir: std::path::PathBuf,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl TempDataHome {
        fn new() -> Self {
            let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = std::env::temp_dir().join(format!(
                "whisper-teste-data-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            // Safety: ENV_LOCK garante que nenhum outro teste mexe no env
            // enquanto este guard vive.
            unsafe { std::env::set_var("XDG_DATA_HOME", &dir) };
            Self { dir, _guard }
        }
    }

    impl Drop for TempDataHome {
        fn drop(&mut self) {
            // Safety: mesmo mutex do new().
            unsafe { std::env::remove_var("XDG_DATA_HOME") };
            std::fs::remove_dir_all(&self.dir).ok();
        }
    }

    #[test]
    fn ai_model_path_resolves_catalog_and_direct_paths() {
        let _data_home = TempDataHome::new();
        let catalog_path = crate::model::models_dir().join("Qwen_Qwen3.5-2B-Q5_K_M.gguf");
        std::fs::create_dir_all(catalog_path.parent().unwrap()).unwrap();
        std::fs::write(&catalog_path, b"teste").unwrap();
        let cfg = Config::default();
        assert_eq!(cfg.ai_model_path(), Some(catalog_path.clone()));
        std::fs::remove_file(catalog_path).unwrap();

        let mut invalid = cfg.clone();
        invalid.ai.model = "modelo-inexistente".to_string();
        assert_eq!(invalid.ai_model_path(), None);

        let direct = std::env::temp_dir().join(format!(
            "whisper-ai-model-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&direct, b"teste").unwrap();
        let mut direct_cfg = cfg;
        direct_cfg.ai.model = direct.display().to_string();
        assert_eq!(direct_cfg.ai_model_path(), Some(direct.clone()));
        std::fs::remove_file(&direct).unwrap();
        assert_eq!(direct_cfg.ai_model_path(), None);
    }

    #[test]
    fn language_to_whisper_code() {
        assert_eq!(Language::Pt.whisper_code(), Some("pt"));
        assert_eq!(Language::En.whisper_code(), Some("en"));
        assert_eq!(Language::Auto.whisper_code(), None);
    }
}
