# Development

Documentação para quem trabalha no código do whisper — humanos e agentes de
IA. Para um resumo operacional rápido (comandos e invariantes), veja
[AGENTS.md](../AGENTS.md).

## Setup

```sh
devenv shell                 # ambiente (mesmo devenv.nix do nix develop)
nix develop --no-pure-eval   # alternativa via flake
```

Dentro do shell:

```sh
cargo build --release
./target/release/whisper start
./target/release/whisper status   # idle
./target/release/whisper toggle   # sessão de ditado (precisa Wayland + mic)
```

Dependências de runtime (fora do shell): `pw-record` (PipeWire), `wtype`,
`wl-copy`, driver Vulkan. O pacote do flake (`nix build .#default`) já inclui
`wtype` e `wl-copy` no PATH via wrapper; instalando por outros meios,
instale-os no sistema — o app avisa (console, log e rodapé do OSD) se
`wtype` estiver ausente. O stderr do daemon em background vai para
`~/.local/state/whisper/daemon.log`; para ver logs ao vivo, rode `whisper
daemon` em primeiro plano.

## Project Structure

```
src/
  main.rs        # CLI (clap): start/stop/daemon/toggle/status/setup
  daemon.rs      # orquestrador: sessões, fases, hot reload, threads
  ipc.rs         # protocolo: socket Unix + JSON por linha
  config.rs      # config.toml, caminhos XDG, mtime (hot reload)
  model.rs       # catálogo de modelos + download paralelo (HTTP Range)
  setup.rs       # wizard interativo (dialoguer)
  audio.rs       # captura pw-record (f32 mono 16 kHz)
  transcribe.rs  # engine whisper.cpp (whisper-rs, Vulkan)
  postprocess.rs # trim_silence, remove_fillers, fix_punctuation (puros)
  osd.rs         # OSD wlr-layer-shell (tiny-skia + ab_glyph)
  insert.rs      # inserção: wtype (digita) + wl-copy (clipboard)
```

## Architecture

```
whisper toggle ──socket──► daemon (loop principal, recv_timeout 1 s)
                              ├─ thread OSD: wlr-layer-shell + teclas (Space/Enter/Esc)
                              ├─ thread captura: lê stdout do pw-record em blocos
                              └─ thread worker: transcrição + pós-processamento
```

O daemon é um loop de eventos sem async: tudo é `std::thread` + canais
`mpsc`. A CLI nunca fala com o OSD/áudio diretamente — só via IPC.

## IPC protocol

- Socket: `$XDG_RUNTIME_DIR/whisper.sock`, JSON por linha.
- Request: `{ "cmd": "toggle" | "status" | "stop" }`.
- Response: `{ "ok": bool, "state": str, "error": str|null, "exe": str|null }`
  — `exe` (só em `status`) é o caminho do binário do daemon; ausente em
  daemons de versões antigas.
- Socket órfão é removido na subida; um segundo daemon sai com erro (exit 1)
  em vez de ficar ocioso.

## State machine

Fases do daemon (`daemon.rs`): `Idle → Loading → Recording ⇄ Paused →
Transcribing → Idle`. O OSD tem a própria fase de UI (`osd.rs`: Loading,
Recording, Paused, Transcribing, Error).

Fluxo de uma sessão:

1. `toggle` com o daemon `Idle` → `start_session`: cria o OSD; na primeira
   sessão carrega o engine (async, `Loading`); depois `start_capture`
   (`pw-record`).
2. `Space` alterna `Recording ⇄ Paused`. Chunks de áudio viram amostras no
   buffer + nível RMS para a waveform.
3. `Enter` → `commit`: para a captura e manda o buffer para a thread worker
   (trim_silence → whisper → remove_fillers → fix_punctuation).
4. `handle_worker` agenda `pending_insert` e fecha o OSD imediatamente —
   sem preview do texto: ele aparece direto na app focada.
5. Evento `Closed` (emitido pela thread do OSD ao sair, após flush do
   destroy) → **só então** `insert::insert` digita na app focada → `Idle`.
6. `Esc` → `cancel_session`: mata captura, fecha OSD, descarta pendências.

**Invariante crítico:** a inserção acontece depois do `Closed`. Enquanto o
OSD está visível ele segura o foco de teclado — `wtype` digitando antes
escreveria no OSD, não na app. Não "otimize" isso.

## Hot reload

- O loop principal compara o mtime do `config.toml` a cada tick (~1 s) em
  `reload_config_if_changed`.
- Config válida → troca `self.cfg` na hora; inválida → mantém a anterior e
  loga.
- Campos de sessão valem na próxima sessão; `model`/`gpu_device`/`threads`
  mudados marcam `pending_engine_reload`, e `reload_engine_if_pending`
  recarrega o engine em background quando o daemon está ocioso (falha
  mantém o atual).

Ao adicionar uma opção nova em `config.rs` (com default), o hot reload a
pega automaticamente; se ela afetar o engine, inclua-a na comparação de
`reload_config_if_changed`.

## Download de modelos (model.rs)

- Probe com `Range: bytes=0-0`: `206` + `Content-Range` → download paralelo
  (até `MAX_CONNECTIONS=16` conexões, cada uma gravando seu intervalo com
  `write_at`); `200` → servidor sem Range, fallback para uma conexão.
- Falha remove o arquivo parcial (idempotência: `dest.exists()` pula).

## Testing

```sh
cargo test --release
```

Cobertura: pós-processamento (trim/fillers/pontuação), parse de config e
Content-Range, catálogo de modelos, smoke test do OSD, modo de inserção.

Integração real (ignorada por padrão — precisa modelo e WAV):

```sh
WHISPER_MODEL=~/.local/share/whisper/models/ggml-tiny.bin \
WHISPER_WAV=/tmp/jfk.wav \
cargo test --release transcribe_jfk -- --ignored
```

## Debugging

- `whisper status` responde `idle`/`recording`/`paused`/`transcribing`/
  `loading` — primeiro passo para saber onde o daemon está.
- `whisper daemon` em primeiro plano mostra stderr ao vivo (hot reload,
  erros de engine, falhas de inserção).
- Falha de inserção não é visível no OSD (já fechado): olhe o
  `daemon.log` ("inserção falhou (texto no clipboard)").
- `whisper status` mostra também o binário do daemon (`daemon: ...`) — se
  ele não for o esperado, `whisper start` já reinicia com o binário atual.
- Se `wtype` estiver ausente do PATH, `whisper start` avisa no console e o
  daemon registra o aviso no log na subida — instale o pacote e reinicie o
  daemon.

## Conventions

- Comentários, docs e mensagens de erro em **pt-BR**.
- Sem async runtime: threads + canais `mpsc`.
- Erros: `anyhow` com `context` em pt-BR.
- Caminhos XDG sempre via helpers de `config.rs` (nunca hardcode).
- UI do OSD: estado compartilhado `Arc<Mutex<UiState>>` — o daemon escreve,
  o OSD lê por frame; desenho com tiny-skia (fonte DejaVu embutida em
  `assets/`).
