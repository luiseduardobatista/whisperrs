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
cargo build
```

Instale no PATH, configure e suba o daemon:

```bash
install -Dm755 target/debug/whisper ~/.local/bin/whisper
whisper setup              # onboarding: idioma + small recomendado + resumo/download
whisper start              # sobe o daemon em background (não trava o shell)
```

Dite: pressione a tecla do compositor (ou `whisper toggle`), fale e conclua
com `Enter` ou acionando novamente o atalho global — o texto é inserido na app
focada.

---

## Como funciona

```
[hotkey do compositor] ─► whisper toggle ─► daemon abre o OSD (borda inferior)
                                          Toggle novamente = concluir
                                          Space = pausar/retomar gravação
                                          Enter = concluir: transcreve + insere + fecha
                                          Esc   = cancelar (descarta)
```

- Captura via `pw-record` (PipeWire): f32 mono 16 kHz, resample nativo.
- Transcrição batch no fim da gravação (Vulkan, ~1 s no turbo).
- Detecção de voz (VAD): só os segmentos de fala vão para o whisper
  (silêncio e pausas longas ficam de fora).
- Pós-processamento: remoção de fillers ("hmm", "ahn"…),
  capitalização/pontuação e período final; opcionalmente Qwen3.5-2B local
  via `llama-server` (com fallback automático para o cleanup Rust).
- Inserção: `wtype` digita na app focada e `wl-copy` pode copiar o texto
  (modo configurável; o padrão preserva o clipboard e só copia se `wtype` falhar).
- OSD em `wlr-layer-shell` (Niri, Sway, Hyprland, KDE ≥ 6.3, COSMIC) com
  fallback para janela xdg-toplevel (GNOME). Sem suporte a X11.

---

## Uso

```bash
whisper setup              # onboarding interativo; small é o recomendado
whisper start              # sobe o daemon em background (idempotente)
whisper stop               # para o daemon; sucesso se já estiver parado
whisper restart            # reinicia o daemon
whisper status             # estado humano do daemon
whisper status --json      # estado para scripts e status bars
whisper toggle             # inicia ou conclui a sessão (bind no compositor)
whisper record             # inicia ou retoma uma sessão
whisper commit             # conclui a sessão atual
whisper cancel             # cancela e descarta a sessão
whisper pause              # pausa a gravação
whisper daemon             # daemon em primeiro plano (debug / systemd)
```

`whisper start` roda o daemon destacado do terminal (grupo de processo
próprio, sobrevive ao fechar o shell) com log em
`~/.local/state/whisper/daemon.log`; com o daemon já rodando, apenas avisa.
`stop` é idempotente: parar um daemon que já não está rodando retorna sucesso.
`restart` compõe `stop` e `start`, aguardando o socket antigo desaparecer.

Para automação, `whisper status --json` escreve somente um objeto JSON em
stdout. Um daemon parado é um resultado válido e retorna exit code `0`:

```json
{"daemon":"running","state":"recording","language":"pt","model":"small"}
```

Falhas de transporte ou protocolo não escrevem JSON em stdout, informam o
problema em stderr e retornam exit code `1`.

Sessão de ditado:

| Tecla | Ação |
|-------|------|
| `Space` | pausar/retomar a gravação |
| `Enter` | concluir: transcreve, insere na app focada e fecha |
| `Esc` | cancelar e descartar |

O `toggle` durante `Loading` ou `Transcribing` não altera a sessão; use
`cancel` ou `Esc` para descartar explicitamente. Integrações (Niri, systemd) e
problemas comuns: [docs/usage.md](docs/usage.md).

---

## Configuração

`~/.config/whisper/config.toml` (gerada pelo `whisper setup`). O daemon
observa o arquivo e recarrega na hora (hot reload, ~1 s): campos de sessão
valem já na próxima sessão; trocar `model`/`gpu_device`/`threads` recarrega o
modelo em background quando o daemon está ocioso (falha mantém o modelo
atual). Config inválida é ignorada (mantém a anterior). O setup apresenta um
resumo dos downloads antes de começar; no modo interativo também pode iniciar
o daemon ao final.

Referência completa de opções, modelos e download:
[docs/configuration.md](docs/configuration.md).

Para usar o pós-processamento Qwen, instale e habilite o modelo com
`whisper setup --ai-model qwen3.5-2b --yes` (ou responda "sim" à pergunta do
setup) e mantenha `llama-server` no PATH. Para um setup totalmente scriptável
sem Qwen, use `--no-ai --yes`:

```toml
[ai]
enabled = true
model = "qwen3.5-2b"
context_size = 2048
gpu = true
cleanup = true
```

Exemplo não interativo completo:

```bash
whisper setup --lang pt --model small --insert-mode both --no-ai --yes
```

---

## Build

```sh
nix develop          # entra no shell com cmake/clang/vulkan/etc.
cargo build
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
  cargo test transcribe_jfk -- --ignored
  ```

- Fonte embutida: DejaVu Sans (licença Bitstream Vera/DejaVu, ver `assets/`).
