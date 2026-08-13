//! Catálogo de modelos whisper.cpp (multilíngues) e download do HuggingFace.
use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub struct ModelSpec {
    pub name: &'static str,
    pub file: &'static str,
    pub size_mb: u64,
}

/// Modelos multilíngues oficiais de ggml-org/whisper.cpp.
pub const MODELS: &[ModelSpec] = &[
    ModelSpec { name: "tiny", file: "ggml-tiny.bin", size_mb: 75 },
    ModelSpec { name: "base", file: "ggml-base.bin", size_mb: 142 },
    ModelSpec { name: "small", file: "ggml-small.bin", size_mb: 466 },
    ModelSpec { name: "medium", file: "ggml-medium.bin", size_mb: 1_535 },
    ModelSpec { name: "large-v3", file: "ggml-large-v3.bin", size_mb: 3_093 },
    ModelSpec { name: "turbo", file: "ggml-large-v3-turbo.bin", size_mb: 1_620 },
];

const BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

pub fn find(name: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.name == name)
}

pub fn file_name(name: &str) -> Option<&'static str> {
    find(name).map(|m| m.file)
}

pub fn models_dir() -> PathBuf {
    crate::config::data_dir().join("models")
}

/// Baixa o modelo para `dest` (idempotente: modelo já existente é pulado).
pub fn download(spec: &ModelSpec, dest: &Path) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    let url = format!("{BASE_URL}/{}", spec.file);
    let resp = reqwest::blocking::Client::new()
        .get(&url)
        .send()
        .with_context(|| format!("baixando {url}"))?
        .error_for_status()
        .with_context(|| format!("erro HTTP ao baixar {url}"))?;
    let total = resp.content_length().unwrap_or(0);
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template("{msg}  {bar:32}  {percent}%  {bytes}/{total_bytes}  {eta}")
            .unwrap()
            .progress_chars("█▉▊▋▌▍▎▏  "),
    );
    pb.set_message(spec.file.to_string());
    std::fs::create_dir_all(dest.parent().expect("modelo dentro de models_dir"))?;
    let mut file = File::create(dest)?;
    let mut reader = resp;
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        pb.inc(n as u64);
    }
    pb.finish_and_clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogo_contem_modelos_esperados() {
        for name in ["tiny", "base", "small", "medium", "large-v3", "turbo"] {
            assert!(find(name).is_some(), "faltando {name}");
        }
        assert_eq!(find("turbo").unwrap().file, "ggml-large-v3-turbo.bin");
        assert!(find("inexistente").is_none());
    }
}
