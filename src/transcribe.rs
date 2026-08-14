//! Motor de transcrição: whisper.cpp via whisper-rs, com backend Vulkan e
//! VAD (Silero) residente para filtrar os segmentos de fala antes de transcrever.
use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Mutex;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperVadContext,
    WhisperVadContextParams, WhisperVadParams,
};

pub struct Engine {
    ctx: WhisperContext,
    threads: u32,
    /// VAD (Silero via ggml); Mutex porque a API exige `&mut`.
    vad: Option<Mutex<WhisperVadContext>>,
}

impl Engine {
    /// Carrega o modelo (fica residente; reutilizado entre sessões). O VAD é
    /// opcional: ausência/falha não derruba o engine (ver `vad_available`).
    pub fn load(
        model_path: &Path,
        vad_model_path: &Path,
        gpu_device: i32,
        threads: u32,
    ) -> Result<Engine> {
        let params = WhisperContextParameters {
            use_gpu: true,
            gpu_device,
            ..Default::default()
        };
        let path = model_path
            .to_str()
            .context("caminho do modelo não é UTF-8")?;
        let ctx = WhisperContext::new_with_params(path, params).with_context(|| {
            format!(
                "falha ao carregar {} (rode 'whisper setup')",
                model_path.display()
            )
        })?;
        let vad = vad_model_path
            .to_str()
            .and_then(|p| WhisperVadContext::new(p, WhisperVadContextParams::default()).ok())
            .map(Mutex::new);
        Ok(Engine {
            ctx,
            threads: threads.max(1),
            vad,
        })
    }

    /// VAD disponível?
    pub fn vad_available(&self) -> bool {
        self.vad.is_some()
    }

    /// Filtra o buffer para os segmentos de fala. Sem VAD, devolve o original
    /// sem cópia. `Ok(vazio)` = nenhuma fala; `Err` = falha de inferência.
    pub fn filter_speech(&self, samples: Vec<f32>) -> Result<Vec<f32>> {
        let Some(vad) = &self.vad else {
            return Ok(samples);
        };
        let mut vad = vad.lock().unwrap_or_else(|p| p.into_inner());
        let segments = vad
            .segments_from_samples(WhisperVadParams::default(), &samples)
            .context("detecção de voz falhou")?;
        let segments: Vec<(f32, f32)> = segments.map(|s| (s.start, s.end)).collect();
        Ok(concat_segments(&samples, &segments))
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
        state
            .full(params, samples)
            .context("falha na transcrição")?;
        let text: Vec<String> = state.as_iter().map(|s| s.to_string()).collect();
        Ok(text.join(" ").trim().to_string())
    }
}

/// Concatena os segmentos (`start`/`end` em centésimos; o pad de 30 ms já
/// está embutido nos timestamps). Início arredonda para baixo e fim para
/// cima (não cortar fonemas); clamp no buffer; intervalos vazios/invertidos
/// ignorados.
fn concat_segments(samples: &[f32], segments: &[(f32, f32)]) -> Vec<f32> {
    let rate = crate::audio::SAMPLE_RATE as f32;
    let mut out = Vec::new();
    for (start, end) in segments {
        let a = ((start / 100.0) * rate).max(0.0) as usize;
        let a = a.min(samples.len());
        let b = (((end / 100.0) * rate).ceil() as usize).min(samples.len());
        if b > a {
            out.extend_from_slice(&samples[a..b]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Amostras marcadas pela posição (0.0, 1.0, 2.0, …) para validar índices.
    fn ramp(n: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32).collect()
    }

    #[test]
    fn concat_converts_centiseconds_to_indices() {
        let samples = ramp(16_000); // 1 s a 16 kHz
        // 0,0 s–0,5 s → amostras 0..8000
        let out = concat_segments(&samples, &[(0.0, 50.0)]);
        assert_eq!(out.len(), 8_000);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[7_999], 7_999.0);
    }

    #[test]
    fn concat_preserves_segment_order() {
        let samples = ramp(32_000); // 2 s
        // 0,0–0,5 s e 1,0–1,5 s → duas metades, na ordem
        let out = concat_segments(&samples, &[(0.0, 50.0), (100.0, 150.0)]);
        assert_eq!(out.len(), 16_000);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[7_999], 7_999.0);
        assert_eq!(out[8_000], 16_000.0);
        assert_eq!(out[15_999], 23_999.0);
    }

    #[test]
    fn concat_clamps_to_buffer_bounds() {
        let samples = ramp(16_000);
        // Segmento além do fim do buffer: clamp, sem panic.
        let out = concat_segments(&samples, &[(0.0, 200.0)]);
        assert_eq!(out, samples);
        // Início negativo também satura em 0.
        let out = concat_segments(&samples, &[(-50.0, 50.0)]);
        assert_eq!(out.len(), 8_000);
    }

    #[test]
    fn concat_ignores_empty_and_inverted_segments() {
        let samples = ramp(16_000);
        // vazio (start == end) e invertido (start > end)
        let out = concat_segments(&samples, &[(50.0, 50.0), (150.0, 100.0), (0.0, 10.0)]);
        assert_eq!(out.len(), 1_600);
        assert_eq!(out[0], 0.0);
    }

    #[test]
    fn concat_empty_segments_returns_empty() {
        let samples = ramp(16_000);
        assert!(concat_segments(&samples, &[]).is_empty());
    }

    /// Teste de integração ignorado; execução documentada em docs/development.md.
    #[test]
    #[ignore = "requer modelo e arquivo de áudio reais"]
    fn transcribe_jfk() {
        let model = std::env::var("WHISPER_MODEL").expect("WHISPER_MODEL");
        let wav = std::env::var("WHISPER_WAV").expect("WHISPER_WAV");
        let samples = read_wav_f32(&wav);
        // Sem modelo VAD (path inexistente): transcreve o buffer inteiro.
        let engine = Engine::load(Path::new(&model), Path::new("/nonexistent-vad.bin"), 0, 4)
            .expect("engine");
        let text = engine.transcribe(&samples, Some("en")).expect("transcribe");
        println!("TRANSCRIÇÃO: {text}");
        assert!(!text.is_empty());
    }

    /// Teste de integração ignorado; execução documentada em docs/development.md.
    #[test]
    #[ignore = "requer modelo, VAD e arquivo de áudio reais"]
    fn vad_filters_real_audio() {
        let model = std::env::var("WHISPER_MODEL").expect("WHISPER_MODEL");
        let vad_model = std::env::var("WHISPER_VAD_MODEL").expect("WHISPER_VAD_MODEL");
        let wav = std::env::var("WHISPER_WAV").expect("WHISPER_WAV");
        let samples = read_wav_f32(&wav);
        let engine = Engine::load(Path::new(&model), Path::new(&vad_model), 0, 4).expect("engine");
        assert!(engine.vad_available(), "VAD não carregou");
        let original_len = samples.len();
        let speech = engine.filter_speech(samples).expect("vad");
        assert!(!speech.is_empty(), "VAD não detectou fala no WAV");
        assert!(speech.len() <= original_len, "VAD ampliou o áudio");
        let text = engine.transcribe(&speech, Some("en")).expect("transcribe");
        println!("TRANSCRIÇÃO (com VAD): {text}");
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
