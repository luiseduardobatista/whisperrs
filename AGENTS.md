# AGENTS.md

Guia rápido para agentes de IA neste repositório. Referência completa
(arquitetura, protocolo IPC, máquina de estados, testing, debugging):
[docs/development.md](docs/development.md).

## O projeto

Ditado por voz em Rust para Linux/Wayland: `pw-record` captura, whisper.cpp
(Vulkan, sem nuvem) transcreve, OSD em wlr-layer-shell mostra o estado e
`wtype` + `wl-copy` inserem o texto na app focada.

## Comandos

```sh
devenv shell                 # ambiente de dev (build com cargo, não com nix)
cargo build --release
cargo test
./target/release/whisper start | status | stop | toggle
```

Log do daemon: `~/.local/state/whisper/daemon.log`. Config:
`~/.config/whisper/config.toml` (hot reload ~1 s, sem reiniciar o daemon).

### Ambiente (build/teste)

- **Tudo dentro do `devenv shell`** (build, teste, clippy): fora dele faltam
  `pkg-config`, `libxkbcommon`, `glslang` (smithay-client-toolkit não
  compila), e o `target/` do nix é incompatível com o rustc do rustup
  (`E0514`; o `clippy` de fora é um shim do rustup e quebra igual). Alternar
  toolchain sem `cargo clean` custa um rebuild do zero.
- Primeiro build é lento (whisper-rs com bindgen/C++, smithay, tiny-skia,
  reqwest…): não rode `cargo clean` à toa (descarta 2–4 GB; `sccache`
  cacheia entre cleans). Depois disso a iteração é rápida: `cargo check`
  valida sem gerar binário.

## Invariantes — não quebrar

1. **Inserção do texto só após o OSD fechar** (evento `Closed`): o OSD
   visível segura o foco de teclado; digitar antes escreve no OSD, não na
   app (`pending_insert` em `daemon.rs`). Não "otimize" isso.
2. **Hot reload**: opção nova de `config.rs` deve ter default e ser aplicada
   sem reiniciar; se afetar o engine (`model`/`gpu_device`/`threads`),
   recarrega em background (`reload_engine_if_pending`).
3. **Download idempotente**: falha remove o arquivo parcial (`model.rs`);
   nunca deixar um modelo corrompido que um `setup` seguinte pule.
4. **Daemon nunca fica como zumbi**: erro de socket na subida = exit 1.
5. **Daemon sempre do mesmo binário que o CLI**: `start` compara o `exe` do
   daemon (via `status`) com o próprio `current_exe`; diferente (ou
   desconhecido = versão antiga) → derruba e sobe o atual. Não "otimize":
   um daemon de outra origem não tem o wrapper do flake (wtype no PATH).
6. **Sem async runtime** — threads + canais, como o resto do código.
7. **pt-BR** em comentários, mensagens de erro e docs.
8. **Caminhos XDG** sempre pelos helpers de `config.rs`.
