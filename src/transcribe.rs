//! Motor de transcrição: whisper.cpp via whisper-rs, com backend Vulkan.
use anyhow::{Context, Result};
use std::path::Path;
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

pub struct Engine {
    ctx: WhisperContext,
    threads: u32,
}

impl Engine {
    /// Carrega o modelo (fica residente; reutilizado entre sessões).
    pub fn load(model_path: &Path, gpu_device: i32, threads: u32) -> Result<Engine> {
        let mut params = WhisperContextParameters::default();
        params.use_gpu = true;
        params.gpu_device = gpu_device;
        let path = model_path.to_str().context("caminho do modelo não é UTF-8")?;
        let ctx = WhisperContext::new_with_params(path, params).with_context(|| {
            format!(
                "falha ao carregar {} (rode 'whisper setup')",
                model_path.display()
            )
        })?;
        Ok(Engine { ctx, threads: threads.max(1) })
    }

    /// Transcreve amostras f32 mono 16 kHz. `language = None` → auto-detect.
    pub fn transcribe(&self, samples: &[f32], language: Option<&str>) -> Result<String> {
        let mut state = self.ctx.create_state().context("falha ao criar estado")?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(language);
        params.set_n_threads(self.threads as i32);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        state.full(params, samples).context("falha na transcrição")?;
        let text: Vec<String> = state.as_iter().map(|s| s.to_string()).collect();
        Ok(text.join(" ").trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Teste de integração ignorado: transcreve um WAV real no engine Vulkan.
    /// Uso:
    ///   WHISPER_MODEL=~/.local/share/whisper/models/ggml-tiny.bin \
    ///   WHISPER_WAV=/tmp/jfk.wav \
    ///   cargo test --release transcribe_jfk -- --ignored
    #[test]
    #[ignore = "requer modelo e arquivo de áudio reais"]
    fn transcribe_jfk() {
        let model = std::env::var("WHISPER_MODEL").expect("WHISPER_MODEL");
        let wav = std::env::var("WHISPER_WAV").expect("WHISPER_WAV");
        let samples = read_wav_f32(&wav);
        let engine = Engine::load(Path::new(&model), 0, 4).expect("engine");
        let text = engine.transcribe(&samples, Some("en")).expect("transcribe");
        println!("TRANSCRIÇÃO: {text}");
        assert!(!text.is_empty());
    }

    /// Leitor WAV mínimo (16 bits ou f32, mono/estéreo, com resample para 16 kHz).
    fn read_wav_f32(path: &str) -> Vec<f32> {
        let raw = std::fs::read(path).expect("wav");
        let mut off = 12;
        let (mut sample_rate, mut bits, mut channels) = (0u32, 0u16, 0u16);
        let mut data = Vec::new();
        while off + 8 <= raw.len() {
            let id = &raw[off..off + 4];
            let sz = u32::from_le_bytes(raw[off + 4..off + 8].try_into().unwrap()) as usize;
            if id == b"fmt " {
                let fmt = &raw[off + 8..off + 8 + sz];
                channels = u16::from_le_bytes(fmt[2..4].try_into().unwrap());
                sample_rate = u32::from_le_bytes(fmt[4..8].try_into().unwrap());
                bits = u16::from_le_bytes(fmt[14..16].try_into().unwrap());
            } else if id == b"data" {
                data = raw[off + 8..off + 8 + sz].to_vec();
                break;
            }
            off += 8 + sz + (sz % 2);
        }
        let mut out: Vec<f32> = match bits {
            16 => data
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                .collect(),
            32 => data
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
            b => panic!("bits por amostra não suportados: {b}"),
        };
        if channels > 1 {
            out = out
                .chunks(channels as usize)
                .map(|c| c.iter().sum::<f32>() / channels as f32)
                .collect();
        }
        if sample_rate != 16_000 {
            let ratio = 16_000.0 / sample_rate as f32;
            let n = (out.len() as f32 * ratio) as usize;
            out = (0..n)
                .map(|i| {
                    let src = i as f32 / ratio;
                    let a = src.floor() as usize;
                    let b = (a + 1).min(out.len() - 1);
                    let f = src - a as f32;
                    out[a] * (1.0 - f) + out[b] * f
                })
                .collect();
        }
        out
    }
}
