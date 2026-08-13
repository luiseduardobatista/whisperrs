//! Pós-processamento puro: corte de silêncio (antes de transcrever) e
//! limpeza de texto (fillers, pontuação) depois da transcrição.
use crate::config::Language;

/// RMS de um bloco de amostras (para a waveform e detecção de silêncio).
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

/// Corta silêncio nas bordas e colapsa pausas internas mais longas que
/// `max_gap_ms` para `collapse_to_ms`. Janela de análise: 30 ms.
pub fn trim_silence(
    samples: &[f32],
    sample_rate: u32,
    threshold: f32,
    max_gap_ms: u64,
    collapse_to_ms: u64,
) -> Vec<f32> {
    const WIN_MS: u64 = 30;
    let win = ((sample_rate as u64 * WIN_MS / 1000) as usize).max(1);
    let levels: Vec<f32> = samples.chunks(win).map(rms).collect();

    let first = levels.iter().position(|l| *l >= threshold);
    let last = levels.iter().rposition(|l| *l >= threshold);
    let (Some(first), Some(last)) = (first, last) else {
        return Vec::new();
    };

    let max_gap_wins = (max_gap_ms / WIN_MS).max(1) as usize;
    let collapse_wins = (collapse_to_ms / WIN_MS).max(1) as usize;
    let mut out = Vec::with_capacity(samples.len());

    let mut i = first;
    while i <= last {
        if levels[i] >= threshold {
            let start = i * win;
            let end = ((i + 1) * win).min(samples.len());
            out.extend_from_slice(&samples[start..end]);
            i += 1;
        } else {
            let mut j = i;
            while j <= last && levels[j] < threshold {
                j += 1;
            }
            let gap = j - i;
            if gap > max_gap_wins {
                let keep = collapse_wins.min(gap);
                let start = (i * win).min(samples.len());
                let end = (start + keep * win).min(samples.len());
                out.extend_from_slice(&samples[start..end]);
            }
            i = j;
        }
    }
    out
}

/// Marcadores de fala (filler words) por língua, checados com caixa
/// insensível e sem afetar palavras reais ("tipo", "é", "um" não entram).
const FILLERS: &[(Language, &[&str])] = &[
    (Language::Pt, &["hmm", "ahn", "ãhn", "eh", "hm", "mmm", "ahã"]),
    (Language::En, &["um", "uh", "hmm", "ah", "er", "mm"]),
];

pub fn remove_fillers(text: &str, lang: Language) -> String {
    let fillers = FILLERS
        .iter()
        .find(|(l, _)| *l == lang)
        .map(|(_, f)| *f)
        .unwrap_or(&[]);
    if fillers.is_empty() {
        return text.to_string();
    }
    text.split_whitespace()
        .filter(|tok| {
            let core = tok.trim_matches(|c: char| !c.is_alphabetic());
            !fillers.iter().any(|f| core.eq_ignore_ascii_case(f))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Normaliza espaços, corrige espaço antes de pontuação, capitaliza início
/// de frases e garante pontuação final (se `final_period`).
pub fn fix_punctuation(text: &str, final_period: bool) -> String {
    let mut collapsed = String::with_capacity(text.len());
    let mut prev_ws = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            prev_ws = true;
            continue;
        }
        let punct = matches!(ch, ',' | '.' | '!' | '?' | ';' | ':' | '…');
        if prev_ws && !punct {
            collapsed.push(' ');
        }
        collapsed.push(ch);
        prev_ws = false;
    }

    let mut out = String::with_capacity(collapsed.len());
    let mut start_of_sentence = true;
    for ch in collapsed.chars() {
        if start_of_sentence && ch.is_alphabetic() {
            out.extend(ch.to_uppercase());
            start_of_sentence = false;
        } else {
            out.push(ch);
        }
        if matches!(ch, '…' | '.' | '!' | '?') {
            start_of_sentence = true;
        }
    }

    if final_period && !out.is_empty() && !matches!(out.chars().last(), Some('.' | '!' | '?' | '…')) {
        out.push('.');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, sample_rate: u32, seconds: f32, amp: f32) -> Vec<f32> {
        let n = (sample_rate as f32 * seconds) as usize;
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / sample_rate as f32).sin() * amp)
            .collect()
    }

    #[test]
    fn rms_de_silencio_e_zero() {
        assert_eq!(rms(&[0.0; 100]), 0.0);
    }

    #[test]
    fn rms_de_onda_senoidal() {
        let s = sine(440.0, 16_000, 0.1, 0.5);
        let r = rms(&s);
        assert!((r - 0.5 / 2f32.sqrt()).abs() < 0.02, "rms = {r}");
    }

    #[test]
    fn trim_remove_silencio_das_bordas() {
        let mut s = vec![0.0; 16_000]; // 1s de silêncio
        s.extend(sine(440.0, 16_000, 0.5, 0.3));
        s.extend(vec![0.0; 8_000]); // 0.5s de silêncio
        let out = trim_silence(&s, 16_000, 0.01, 500, 300);
        let first = out.iter().position(|v| v.abs() > 0.01);
        let last = out.iter().rposition(|v| v.abs() > 0.01);
        assert!(first.is_some());
        assert!(last.is_some());
        // bordas cortadas
        assert!(out.len() < s.len());
        // nenhuma amostra forte perdida
        let strong = s.iter().filter(|v| v.abs() > 0.01).count();
        let strong_out = out.iter().filter(|v| v.abs() > 0.01).count();
        assert_eq!(strong, strong_out);
    }

    #[test]
    fn trim_colapsa_pausa_interna_longa() {
        let mut s = sine(440.0, 16_000, 0.3, 0.3);
        s.extend(vec![0.0; 16_000]); // 1s de pausa
        s.extend(sine(440.0, 16_000, 0.3, 0.3));
        let out = trim_silence(&s, 16_000, 0.01, 500, 300);
        // pausa colapsada: saída bem menor que a entrada
        assert!(out.len() < 16_000, "saída {} >= 1s", out.len());
        // ainda contém as duas falas
        assert!(out.iter().filter(|v| v.abs() > 0.01).count() >= 9_000);
    }

    #[test]
    fn trim_tudo_silencio_retorna_vazio() {
        assert!(trim_silence(&vec![0.0; 16_000], 16_000, 0.01, 500, 300).is_empty());
    }

    #[test]
    fn remove_fillers_pt() {
        assert_eq!(remove_fillers("hmm então ahn vamos eh testar", Language::Pt), "então vamos testar");
    }

    #[test]
    fn remove_fillers_nao_remove_palavras_reais() {
        // "tipo" e "é" são palavras reais em pt e não devem sumir
        assert_eq!(remove_fillers("tipo é um teste", Language::Pt), "tipo é um teste");
        // "um" não é filler em inglês? é artigo alemão — em en, "um" é filler legítimo; aqui teste de en:
        assert_eq!(remove_fillers("um, uh, let me think", Language::En), "let me think");
    }

    #[test]
    fn remove_fillers_insensivel_a_caixa_e_pontuacao() {
        assert_eq!(remove_fillers("Hmm, ahn!", Language::Pt), "");
    }

    #[test]
    fn pontuacao_colapsa_espacos_e_capitaliza() {
        assert_eq!(
            fix_punctuation("olá   mundo .  como vai ?", true),
            "Olá mundo. Como vai?"
        );
    }

    #[test]
    fn pontuacao_periodo_final() {
        assert_eq!(fix_punctuation("isto é um teste", true), "Isto é um teste.");
        assert_eq!(fix_punctuation("isto é um teste", false), "Isto é um teste");
        assert_eq!(fix_punctuation("já tem ponto.", true), "Já tem ponto.");
        assert_eq!(fix_punctuation("pergunta?", true), "Pergunta?");
        assert_eq!(fix_punctuation("", true), "");
    }

    #[test]
    fn pontuacao_nao_capitaliza_mid_sentence() {
        assert_eq!(fix_punctuation("a b c", true), "A b c.");
    }
}
