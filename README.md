# whisper

Ditado por voz minimalista para Linux/Wayland, em Rust: **whisper.cpp com
aceleração Vulkan** (sem backend de nuvem), OSD com waveform e hotkeys, e
inserção do texto na app focada. Feito para uso pessoal.

Inspirado no [whisrs](https://github.com/y0sif/whisrs), com a diferença
central: o whisper.cpp é usado **upstream com GPU** (Vulkan → RADV em AMD,
também funciona em NVIDIA via driver Vulkan).

## Table of Contents

- [Quick Start](#quick-start)
- [Como funciona](#como-funciona)
- [Uso](#uso)
- [Configuração](#configuração)
- [Build](#build)
- [Documentação](#documentação)
- [Notas](#notas)

---

## Quick Start

Build com o devShell (cmake/clang/vulkan):

```bash
nix develop          # entra no shell com cmake/clang/vulkan/etc.
cargo build --release
```

Instale no PATH, configure e suba o daemon:

```bash
install -Dm755 target/release/whisper ~/.local/bin/whisper
whisper setup              # wizard: língua (pt/en/auto) + modelo + download
whisper start              # sobe o daemon em background (não trava o shell)
```

Dite: pressione a tecla do compositor (ou `whisper toggle`), fale e conclua
com `Enter` — o texto é inserido na app focada.

---

## Como funciona

```
[hotkey do compositor] ─► whisper toggle ─► daemon abre o OSD (borda inferior)
                                          Space = pausar/retomar gravação
                                          Enter = concluir: transcreve + insere + fecha
                                          Esc   = cancelar (descarta)
```

- Captura via `pw-record` (PipeWire): f32 mono 16 kHz, resample nativo.
- Transcrição batch no fim da gravação (Vulkan, ~1 s no turbo).
- Pós-processamento: corte de silêncio, remoção de fillers ("hmm", "ahn"…),
  capitalização/pontuação e período final.
- Inserção: `wtype` digita na app focada + `wl-copy` coloca no clipboard
  (modo configurável; se o `wtype` falhar, o texto está no clipboard).
- OSD em `wlr-layer-shell` (Niri, Sway, Hyprland, KDE ≥ 6.3, COSMIC) com
  fallback para janela xdg-toplevel (GNOME). Sem suporte a X11.

---

## Uso

```bash
whisper setup              # wizard: língua + modelo + download
whisper start              # sobe o daemon em background (idempotente)
whisper stop               # derruba o daemon
whisper status             # estado do daemon
whisper toggle             # inicia/cancela uma sessão (bind no compositor)
whisper daemon             # daemon em primeiro plano (debug / systemd)
```

`whisper start` roda o daemon destacado do terminal (grupo de processo
próprio, sobrevive ao fechar o shell) com log em
`~/.local/state/whisper/daemon.log`; com o daemon já rodando, apenas avisa.

Sessão de ditado:

| Tecla | Ação |
|-------|------|
| `Space` | pausar/retomar a gravação |
| `Enter` | concluir: transcreve, insere na app focada e fecha |
| `Esc` | cancelar e descartar |

Integrações (Niri, systemd) e problemas comuns: [docs/usage.md](docs/usage.md).

---

## Configuração

`~/.config/whisper/config.toml` (gerada pelo `whisper setup`). O daemon
observa o arquivo e recarrega na hora (hot reload, ~1 s): campos de sessão
valem já na próxima sessão; trocar `model`/`gpu_device`/`threads` recarrega o
modelo em background quando o daemon está ocioso (falha mantém o modelo
atual). Config inválida é ignorada (mantém a anterior).

Referência completa de opções, modelos e download:
[docs/configuration.md](docs/configuration.md).

---

## Build

```sh
nix develop          # entra no shell com cmake/clang/vulkan/etc.
cargo build --release
```

Ou instale direto: `nix build` → `result/bin/whisper`.

---

## Documentação

- [docs/index.md](docs/index.md) — visão geral da documentação.
- [docs/usage.md](docs/usage.md) — comandos, sessão de ditado, integrações e
  troubleshooting.
- [docs/configuration.md](docs/configuration.md) — `config.toml`, modelos e
  hot reload.
- [docs/development.md](docs/development.md) — arquitetura do código para
  devs e agentes de IA.

---

## Notas

- Modelo é carregado na primeira sessão e fica residente (VRAM).
- Sem microfone disponível, a sessão cancela com aviso no OSD.
- Teste de integração da transcrição (ignorado por padrão):

  ```sh
  WHISPER_MODEL=~/.local/share/whisper/models/ggml-tiny.bin \
  WHISPER_WAV=/tmp/jfk.wav \
  cargo test --release transcribe_jfk -- --ignored
  ```

- Fonte embutida: DejaVu Sans (licença Bitstream Vera/DejaVu, ver `assets/`).
