//! Captura de áudio via `pw-record` (PipeWire): amostras f32 mono a 16 kHz.
//! O PipeWire faz o resample para a taxa pedida; nenhuma dependência extra.
use anyhow::{Context, Result};
use std::io::Read;
use std::process::{Child, ChildStdout, Command, Stdio};

pub const SAMPLE_RATE: u32 = 16_000;
/// Bytes lidos por bloco (~50 ms de f32 mono a 16 kHz).
pub const CHUNK_BYTES: usize = 3_200;

/// Dona do processo `pw-record`. O stdout (amostras cruas) vai para a thread
/// leitora; `stop()` mata o processo, o que fecha o pipe e encerra o leitor.
pub struct Capture {
    child: Child,
    stderr: std::process::ChildStderr,
}

impl Capture {
    pub fn start(source: Option<&str>) -> Result<(Capture, ChildStdout)> {
        let mut cmd = Command::new("pw-record");
        cmd.args([
            "--raw",
            "--format",
            "f32",
            "--rate",
            "16000",
            "--channels",
            "1",
            "-",
        ]);
        if let Some(name) = source {
            cmd.args(["--target", name]);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .context("falha ao iniciar pw-record (PipeWire está instalado?)")?;
        let stdout = child.stdout.take().expect("stdout pipe");
        let stderr = child.stderr.take().expect("stderr pipe");
        Ok((Capture { child, stderr }, stdout))
    }

    pub fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Mensagem de erro do pw-record (ex.: "no target node available").
    pub fn stderr_text(&mut self) -> String {
        let mut buf = String::new();
        let _ = self.stderr.read_to_string(&mut buf);
        buf.trim().to_string()
    }
}

/// Lê um bloco de amostras do pipe. Retorna (amostras, rms do bloco);
/// amostras vazias = fim do fluxo (captura interrompida).
pub fn read_chunk(reader: &mut impl Read) -> std::io::Result<(Vec<f32>, f32)> {
    let mut buf = [0u8; CHUNK_BYTES];
    let mut filled = 0;
    while filled < CHUNK_BYTES {
        let n = reader.read(&mut buf[filled..])?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    let samples: Vec<f32> = buf[..filled - filled % 4]
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    let rms = crate::postprocess::rms(&samples);
    Ok((samples, rms))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn le_blocos_f32_do_pipe() {
        let bytes: Vec<u8> = [1.0f32, 0.5, -0.5, 0.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let mut cur = Cursor::new(bytes);
        let (samples, _) = read_chunk(&mut cur).unwrap();
        assert_eq!(samples, vec![1.0, 0.5, -0.5, 0.0]);
    }

    #[test]
    fn eof_retorna_vazio() {
        let mut cur = Cursor::new(Vec::<u8>::new());
        let (samples, _) = read_chunk(&mut cur).unwrap();
        assert!(samples.is_empty());
    }

    #[test]
    fn bytes_parciais_nao_quebram() {
        let mut bytes = 1.0f32.to_le_bytes().to_vec();
        bytes.push(0xAB); // byte solto
        let mut cur = Cursor::new(bytes);
        let (samples, _) = read_chunk(&mut cur).unwrap();
        assert_eq!(samples, vec![1.0]);
    }
}
