//! Configuração persistida em TOML (~/.config/whisper/config.toml).
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
#[serde(rename_all = "lowercase")]
pub enum InsertMode {
    /// Digita o texto na app focada (wtype).
    Type,
    /// Copia o texto para o clipboard (wl-copy).
    Clipboard,
    /// Digita e também copia (fallback manual).
    Both,
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
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: Language::Pt,
            model: "turbo".to_string(),
            insert_mode: InsertMode::Both,
            remove_fillers: true,
            punctuation: true,
            final_period: true,
            gpu_device: 0,
            threads: 4,
            source: None,
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
        assert_eq!(cfg.model, "turbo");
        assert!(cfg.punctuation);
        assert_eq!(cfg.insert_mode, InsertMode::Both);
    }

    #[test]
    fn language_to_whisper_code() {
        assert_eq!(Language::Pt.whisper_code(), Some("pt"));
        assert_eq!(Language::En.whisper_code(), Some("en"));
        assert_eq!(Language::Auto.whisper_code(), None);
    }
}
