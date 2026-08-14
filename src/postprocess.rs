//! Pós-processamento puro: medição de nível (waveform) e limpeza de texto
//! (fillers, pontuação) depois da transcrição.
use crate::config::Language;

/// RMS de um bloco de amostras (para a waveform).
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f32 = samples.iter().map(|s| s * s).sum();
    (sum / samples.len() as f32).sqrt()
}

/// Marcadores de fala (filler words) por língua, checados com caixa
/// insensível e sem afetar palavras reais ("tipo", "é", "um" não entram).
const FILLERS: &[(Language, &[&str])] = &[
    (
        Language::Pt,
        &["hmm", "ahn", "ãhn", "eh", "hm", "mmm", "ahã"],
    ),
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

    if final_period && !out.is_empty() && !matches!(out.chars().last(), Some('.' | '!' | '?' | '…'))
    {
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
            .map(|i| {
                (2.0 * std::f32::consts::PI * freq * i as f32 / sample_rate as f32).sin() * amp
            })
            .collect()
    }

    #[test]
    fn rms_of_silence_is_zero() {
        assert_eq!(rms(&[0.0; 100]), 0.0);
    }

    #[test]
    fn rms_of_sine_wave() {
        let s = sine(440.0, 16_000, 0.1, 0.5);
        let r = rms(&s);
        assert!((r - 0.5 / 2f32.sqrt()).abs() < 0.02, "rms = {r}");
    }

    #[test]
    fn remove_fillers_pt() {
        assert_eq!(
            remove_fillers("hmm então ahn vamos eh testar", Language::Pt),
            "então vamos testar"
        );
    }

    #[test]
    fn remove_fillers_keeps_real_words() {
        // "tipo" e "é" são palavras reais em pt e não devem sumir
        assert_eq!(
            remove_fillers("tipo é um teste", Language::Pt),
            "tipo é um teste"
        );
        // "um" não é filler em inglês? é artigo alemão — em en, "um" é filler legítimo; aqui teste de en:
        assert_eq!(
            remove_fillers("um, uh, let me think", Language::En),
            "let me think"
        );
    }

    #[test]
    fn remove_fillers_case_and_punctuation_insensitive() {
        assert_eq!(remove_fillers("Hmm, ahn!", Language::Pt), "");
    }

    #[test]
    fn punctuation_collapses_spaces_and_capitalizes() {
        assert_eq!(
            fix_punctuation("olá   mundo .  como vai ?", true),
            "Olá mundo. Como vai?"
        );
    }

    #[test]
    fn punctuation_final_period() {
        assert_eq!(fix_punctuation("isto é um teste", true), "Isto é um teste.");
        assert_eq!(fix_punctuation("isto é um teste", false), "Isto é um teste");
        assert_eq!(fix_punctuation("já tem ponto.", true), "Já tem ponto.");
        assert_eq!(fix_punctuation("pergunta?", true), "Pergunta?");
        assert_eq!(fix_punctuation("", true), "");
    }

    #[test]
    fn punctuation_does_not_capitalize_mid_sentence() {
        assert_eq!(fix_punctuation("a b c", true), "A b c.");
    }
}
