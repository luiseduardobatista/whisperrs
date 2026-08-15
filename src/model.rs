//! Catálogo de modelos whisper.cpp (multilíngues) e download do HuggingFace.
use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

pub struct ModelSpec {
    pub name: &'static str,
    pub file: &'static str,
    pub size_mb: u64,
    pub description: &'static str,
    /// Repositório HuggingFace do arquivo (resolve/main).
    pub base_url: &'static str,
}

/// Modelos multilíngues oficiais de ggml-org/whisper.cpp.
pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        name: "tiny",
        file: "ggml-tiny.bin",
        size_mb: 75,
        description: "muito leve · menor precisão",
        base_url: WHISPER_BASE_URL,
    },
    ModelSpec {
        name: "base",
        file: "ggml-base.bin",
        size_mb: 142,
        description: "leve · para máquinas modestas",
        base_url: WHISPER_BASE_URL,
    },
    ModelSpec {
        name: "small",
        file: "ggml-small.bin",
        size_mb: 466,
        description: "recomendado · equilíbrio entre precisão e recursos",
        base_url: WHISPER_BASE_URL,
    },
    ModelSpec {
        name: "medium",
        file: "ggml-medium.bin",
        size_mb: 1_535,
        description: "mais preciso · mais pesado",
        base_url: WHISPER_BASE_URL,
    },
    ModelSpec {
        name: "large-v3",
        file: "ggml-large-v3.bin",
        size_mb: 3_093,
        description: "máxima precisão · muito pesado",
        base_url: WHISPER_BASE_URL,
    },
    ModelSpec {
        name: "turbo",
        file: "ggml-large-v3-turbo.bin",
        size_mb: 1_620,
        description: "rápido em hardware forte · download maior",
        base_url: WHISPER_BASE_URL,
    },
];

/// Modelos LLM (pós-processamento com Qwen); fora de `MODELS` (não
/// selecionáveis no setup normal de transcrição).
pub const LLM_MODELS: &[ModelSpec] = &[ModelSpec {
    name: "qwen3.5-0.8b",
    file: "Qwen_Qwen3.5-0.8B-Q5_K_M.gguf",
    size_mb: 650,
    description: "pós-processamento inteligente local",
    base_url: "https://huggingface.co/bartowski/Qwen_Qwen3.5-0.8B-GGUF/resolve/main",
}];

/// Modelo VAD (Silero) fixo, baixado pelo `whisper setup` junto com o modelo
/// de transcrição. Fica fora de `MODELS`: não é modelo selecionável.
pub const VAD_MODEL: ModelSpec = ModelSpec {
    name: "silero-v6.2.0",
    file: "ggml-silero-v6.2.0.bin",
    size_mb: 1,
    description: "filtro de voz e silêncio",
    base_url: VAD_BASE_URL,
};

const WHISPER_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
const VAD_BASE_URL: &str = "https://huggingface.co/ggml-org/whisper-vad/resolve/main";

/// Conexões paralelas por download (mesmo default do aria2c `-x 16`).
const MAX_CONNECTIONS: u64 = 16;
const CHUNK_SIZE: u64 = 32 * 1024 * 1024;
const BUF_SIZE: usize = 256 * 1024;

pub fn find(name: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.name == name)
}

pub fn find_llm(name: &str) -> Option<&'static ModelSpec> {
    LLM_MODELS.iter().find(|m| m.name == name)
}

pub fn file_name(name: &str) -> Option<&'static str> {
    find(name).map(|m| m.file)
}

pub fn models_dir() -> PathBuf {
    crate::config::data_dir().join("models")
}

pub fn vad_model_path() -> PathBuf {
    models_dir().join(VAD_MODEL.file)
}

/// Baixa o modelo para `dest` (idempotente: modelo já existente é pulado).
/// Usa HTTP Range em conexões paralelas quando o servidor suporta (206);
/// uma falha remove o arquivo parcial para não envenenar a próxima tentativa.
pub fn download(spec: &ModelSpec, dest: &Path) -> Result<()> {
    if dest.exists() {
        return Ok(());
    }
    let url = format!("{}/{}", spec.base_url, spec.file);
    let result = download_impl(spec, &url, dest);
    if result.is_err() {
        let _ = std::fs::remove_file(dest);
    }
    result
}

fn download_impl(spec: &ModelSpec, url: &str, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest.parent().expect("modelo dentro de models_dir"))?;
    let client = reqwest::blocking::Client::new();

    // Sonda com Range de 1 byte: 206 = servidor suporta paralelismo e informa
    // o total; 200 = ignora Range e o corpo da sonda já é o arquivo inteiro.
    let probe = client
        .get(url)
        .header(reqwest::header::RANGE, "bytes=0-0")
        .send()
        .with_context(|| format!("baixando {url}"))?
        .error_for_status()
        .with_context(|| format!("erro HTTP ao baixar {url}"))?;

    if probe.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        if let Some(total) = content_range_total(probe.headers()) {
            drop(probe);
            return download_parallel(&client, url, dest, total, spec.file);
        }
        // 206 sem Content-Range: cai para GET simples abaixo.
    } else {
        return download_single(probe, dest, spec.file);
    }
    drop(probe);

    let resp = client
        .get(url)
        .send()
        .with_context(|| format!("baixando {url}"))?
        .error_for_status()
        .with_context(|| format!("erro HTTP ao baixar {url}"))?;
    download_single(resp, dest, spec.file)
}

/// Lê o total de `Content-Range: bytes 0-0/TOTAL`; `None` se ausente ou "*".
fn content_range_total(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let hdr = headers.get(reqwest::header::CONTENT_RANGE)?.to_str().ok()?;
    let (_, total) = hdr.rsplit_once('/')?;
    total.parse().ok()
}

fn progress_bar(total: u64, label: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template("{msg}  {bar:32}  {percent}%  {bytes}/{total_bytes}  {eta}")
            .unwrap()
            .progress_chars("█▉▊▋▌▍▎▏  "),
    );
    pb.set_message(label.to_string());
    pb
}

/// Baixa o arquivo inteiro com uma única conexão (servidor sem suporte a Range).
fn download_single(resp: reqwest::blocking::Response, dest: &Path, label: &str) -> Result<()> {
    let total = resp.content_length().unwrap_or(0);
    let pb = progress_bar(total, label);
    let mut file = File::create(dest)?;
    let mut reader = resp;
    let mut buf = vec![0u8; BUF_SIZE];
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

/// Baixa em até `MAX_CONNECTIONS` conexões paralelas, cada uma gravando seu
/// pedaço na posição certa (write_at). Uma conexão TCP sozinha não satura
/// links rápidos — o limite é a janela de fluxo, não a banda —, então várias
/// conexões paralelas resolvem isso (técnica do aria2c `-x N`).
fn download_parallel(
    client: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
    total: u64,
    label: &str,
) -> Result<()> {
    let file = File::create(dest)?;
    file.set_len(total)?;

    let chunks = total.div_ceil(CHUNK_SIZE).clamp(1, MAX_CONNECTIONS);
    let pb = progress_bar(total, label);
    let done = Arc::new(AtomicU64::new(0));
    let (tx, rx) = std::sync::mpsc::channel();

    for i in 0..chunks {
        let start = total * i / chunks;
        let end = total * (i + 1) / chunks - 1;
        let client = client.clone();
        let url = url.to_string();
        let dest = dest.to_path_buf();
        let done = done.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let _ = tx.send(download_chunk(&client, &url, &dest, start, end, &done));
        });
    }

    let mut remaining = chunks as usize;
    let mut errors = Vec::new();
    while remaining > 0 {
        pb.set_position(done.load(Ordering::Relaxed));
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(Ok(())) => remaining -= 1,
            Ok(Err(e)) => {
                errors.push(e);
                remaining -= 1;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    pb.finish_and_clear();

    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("download incompleto: {}", errors.join("; "))
    }
}

fn download_chunk(
    client: &reqwest::blocking::Client,
    url: &str,
    dest: &Path,
    start: u64,
    end: u64,
    done: &AtomicU64,
) -> Result<(), String> {
    let resp = client
        .get(url)
        .header(reqwest::header::RANGE, format!("bytes={start}-{end}"))
        .send()
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?;
    if resp.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        return Err("servidor não honrou o Range (resposta não-206)".into());
    }
    let file = File::options()
        .write(true)
        .open(dest)
        .map_err(|e| e.to_string())?;
    let mut reader = resp;
    let mut buf = vec![0u8; BUF_SIZE];
    let mut written = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_at(&buf[..n], start + written)
            .map_err(|e| e.to_string())?;
        written += n as u64;
        done.fetch_add(n as u64, Ordering::Relaxed);
    }
    let expected = end - start + 1;
    if written != expected {
        return Err(format!("resposta curta: {written} de {expected} bytes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_expected_models() {
        for name in ["tiny", "base", "small", "medium", "large-v3", "turbo"] {
            assert!(find(name).is_some(), "faltando {name}");
        }
        assert_eq!(find("turbo").unwrap().file, "ggml-large-v3-turbo.bin");
        assert!(find("inexistente").is_none());
    }

    #[test]
    fn llm_catalog_is_separate_and_exact() {
        let spec = find_llm("qwen3.5-0.8b").unwrap();
        assert!(find("qwen3.5-0.8b").is_none());
        assert_eq!(spec.file, "Qwen_Qwen3.5-0.8B-Q5_K_M.gguf");
        assert_eq!(spec.size_mb, 650);
        assert_eq!(
            spec.base_url,
            "https://huggingface.co/bartowski/Qwen_Qwen3.5-0.8B-GGUF/resolve/main"
        );
        assert!(find_llm("inexistente").is_none());
    }

    #[test]
    fn vad_model_is_not_selectable_but_has_expected_origin() {
        assert!(find(VAD_MODEL.name).is_none());
        // Campos exatos protegem contra typo no arquivo/repositório.
        assert_eq!(VAD_MODEL.file, "ggml-silero-v6.2.0.bin");
        assert_eq!(
            VAD_MODEL.base_url,
            "https://huggingface.co/ggml-org/whisper-vad/resolve/main"
        );
        assert_eq!(VAD_MODEL.size_mb, 1);
        assert_eq!(vad_model_path(), models_dir().join(VAD_MODEL.file));
    }

    #[test]
    fn parse_content_range_total() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert(
            reqwest::header::CONTENT_RANGE,
            "bytes 0-0/123456789".parse().unwrap(),
        );
        assert_eq!(content_range_total(&h), Some(123456789));
        h.insert(
            reqwest::header::CONTENT_RANGE,
            "bytes 0-0/*".parse().unwrap(),
        );
        assert_eq!(content_range_total(&h), None);
        h.insert(reqwest::header::CONTENT_RANGE, "lixo".parse().unwrap());
        assert_eq!(content_range_total(&h), None);
    }
}
