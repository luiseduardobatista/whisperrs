# AGENTS.md

Guia rápido para agentes de IA trabalhando neste repositório. Para a
referência completa (arquitetura, protocolo IPC, máquina de estados), veja
[docs/development.md](docs/development.md).

## O projeto

whisper é um ditado por voz em Rust para Linux/Wayland: captura via
`pw-record`, transcrição local com whisper.cpp (Vulkan, sem nuvem), OSD em
wlr-layer-shell e inserção do texto na app focada (`wtype` + `wl-copy`).
Arquitetura: CLI (`clap`) → IPC (socket Unix, JSON por linha) → daemon de
loop de eventos (threads `std` + canais `mpsc`, sem async).

## Comandos

```sh
devenv shell                 # ambiente de dev (ou: nix develop --no-pure-eval)
cargo build --release
cargo test --release
./target/release/whisper start | status | stop | toggle
# integração (ignorada): WHISPER_MODEL=... WHISPER_WAV=... cargo test --release transcribe_jfk -- --ignored
```

Log do daemon: `~/.local/state/whisper/daemon.log`. Config:
`~/.config/whisper/config.toml` (hot reload ~1 s, sem reiniciar o daemon).

## Invariantes — não quebrar

1. **Inserção do texto só após o OSD fechar** (evento `Closed`): o OSD
   visível segura o foco de teclado; digitar antes escreve no OSD, não na
   app. Ver `pending_insert` em `daemon.rs`.
2. **Hot reload**: qualquer opção nova de `config.rs` deve ter default e
   ser aplicada sem reiniciar; se afetar o engine (`model`/`gpu_device`/
   `threads`), entre na recarga em background (`reload_engine_if_pending`).
3. **Download idempotente**: falha remove o arquivo parcial (`model.rs`);
   nunca deixar um modelo corrompido que um `setup` seguinte pule.
4. **Daemon nunca fica como zumbi**: erro de socket na subida = exit 1.
5. **Sem async runtime** — threads + canais, como o resto do código.
6. **pt-BR** em comentários, mensagens de erro e docs.
7. **Caminhos XDG** sempre pelos helpers de `config.rs`.

## Onde está cada coisa

| Assunto | Arquivo |
|---|---|
| CLI, `start` (spawn destacado) | `src/main.rs` |
| Sessões, fases, hot reload, engine | `src/daemon.rs` |
| Protocolo IPC | `src/ipc.rs` |
| Config/XDG paths | `src/config.rs` |
| Modelos + download paralelo | `src/model.rs`, `src/setup.rs` |
| Áudio (pw-record) | `src/audio.rs` |
| Transcrição (whisper-rs/Vulkan) | `src/transcribe.rs` |
| Pós-processamento | `src/postprocess.rs` |
| OSD (desenho, teclas) | `src/osd.rs` |
| Inserção (wtype/wl-copy) | `src/insert.rs` |
