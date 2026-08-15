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
cargo build
./target/debug/whisper start
./target/debug/whisper status   # idle
./target/debug/whisper toggle   # sessão de ditado (precisa Wayland + mic)
```

O `setup` primeiro coleta todas as escolhas, monta um resumo dos downloads
pendentes e só então executa os downloads e salva a configuração. O ciclo de
vida do daemon continua em `main.rs`: o setup apenas retorna ao CLI se o
usuário interativo pediu para iniciar o daemon.

Dependências de runtime (fora do shell): `pw-record` (PipeWire), `wtype`,
`wl-copy`, driver Vulkan e, opcionalmente, `llama-server` (llama.cpp ≥ b7973)
no PATH. O pacote do flake (`nix build .#default`) já inclui `wtype`,
`wl-copy` e `llama-server` (`llama-cpp-vulkan`) no PATH via wrapper — quem
instala por outros meios instala os programas no sistema —; o app avisa
(console, log e rodapé do OSD) se `wtype` ou Qwen estiverem indisponíveis.
O stderr do daemon em background vai para
`~/.local/state/whisper/daemon.log`; para ver logs ao vivo, rode `whisper
daemon` em primeiro plano.

## Project Structure

```
src/
  main.rs        # CLI (clap): start/stop/restart/daemon/toggle/record/commit/cancel/pause/status/setup
  daemon.rs      # orquestrador: sessões, fases, hot reload, threads
  ipc.rs         # protocolo: socket Unix + JSON por linha
  config.rs      # config.toml, caminhos XDG, mtime (hot reload)
  model.rs       # catálogo de modelos + download paralelo (HTTP Range)
  llm.rs         # cliente local do llama-server (Qwen, opcional)
  setup.rs       # onboarding, resumo e downloads (dialoguer)
  audio.rs       # captura pw-record (f32 mono 16 kHz)
  transcribe.rs  # engine whisper.cpp (whisper-rs, Vulkan) + VAD residente
  postprocess.rs # rms, remove_fillers, fix_punctuation (puros)
  osd.rs         # integração Wayland: superfícies, teclado e buffer SHM
  osd_draw.rs    # desenho puro do cartão (tiny-skia + ab_glyph)
  insert.rs      # inserção: wtype (digita) + wl-copy (clipboard)
```

## Architecture

```
whisper toggle ──socket──► daemon (loop principal, recv_timeout até 1 s)
                              ├─ thread OSD: wlr-layer-shell + teclas (Space/Enter/Esc/S)
                              ├─ thread captura: lê stdout do pw-record em blocos
                              └─ thread worker: VAD → whisper → Qwen/fallback → insert
```

O daemon é um loop de eventos sem async: tudo é `std::thread` + canais
`mpsc`. A CLI nunca fala com o OSD/áudio diretamente — só via IPC. O setup
não interativo usa flags (`--insert-mode`, `--ai-model`, `--no-ai` e `--yes`)
e não inicia o daemon. Feedbacks
temporários usam um deadline monotônico; o `recv_timeout` espera no máximo até
essa deadline, sem bloquear o processamento de eventos. Eventos vindos de
threads carregam o contador da sessão e são descartados quando pertencem a uma
sessão antiga.

## IPC protocol

- Socket: `$XDG_RUNTIME_DIR/whisper.sock`, JSON por linha.
- Request: `{ "cmd": "toggle" | "record" | "commit" | "cancel" | "pause" | "status" | "stop" }`.
- `toggle`: em `Idle` inicia; em `Recording`/`Paused` conclui; em
  `Loading`/`Transcribing` é no-op.
- `record`: inicia em `Idle` e retoma em `Paused`; `commit` conclui;
  `cancel` descarta; `pause` pausa somente uma gravação.
- Response operacional: `{ "state": str, "error": str|null }`.
- Response de `status` também pode conter `exe`, `language`, `model` e `smart`.
  Todos são opcionais para manter compatibilidade com daemons antigos. `exe`
  é o caminho do binário do daemon; `language` e `model` são os valores de
  configuração aceitos pelo daemon; `smart` é o Smart Mode da sessão atual.
- O CLI projeta essa resposta para `status --json`, adicionando
  `{"daemon":"running"}`. Sem listener alcançável, retorna
  `{"daemon":"stopped"}` com exit code `0`; falhas reais de permissão,
  transporte ou parsing retornam exit code `1` e diagnóstico em stderr.
- Socket órfão é removido na subida; um segundo daemon sai com erro (exit 1)
  em vez de ficar ocioso.

## State machine

Fases do daemon (`daemon.rs`): `Idle → Loading → Recording ⇄ Paused →
Transcribing → Idle`. Erros e feedbacks temporários mantêm o daemon em `Idle`
enquanto o OSD permanece visível até a deadline; isso deixa `status`,
`cancel` e `stop` responsivos. O OSD tem a própria fase de UI (`osd.rs`:
Loading, Recording, Paused, Transcribing, Info, Warning, Error). Feedbacks
informativos e avisos usam cores próprias; falhas reais usam `Error`.

Fluxo de uma sessão:

1. `toggle`/`record` com o daemon `Idle` → `start_session`: cria o OSD; na
   primeira sessão carrega o engine (async, `Loading`); depois `start_capture`
   (`pw-record`).
2. `Space`/`pause` coloca a sessão em `Paused`; `Space`/`record` retoma em
   `Recording`. Chunks de áudio viram amostras no buffer + nível RMS para a
   waveform somente enquanto a fase é `Recording`.
3. `toggle`/`Enter`/`commit` em `Recording` ou `Paused` → para a captura e manda
   o buffer para a thread worker (VAD → whisper → Qwen/fallback → inserção).
   O VAD (Silero, CPU) extrai os segmentos de fala do buffer e concatena só
   eles — silêncio das bordas e pausas longas não vão para o whisper. O Qwen só
   é tentado quando está habilitado e disponível; qualquer falha volta ao
   cleanup Rust.
4. `toggle` durante `Loading` ou `Transcribing` é no-op; o trabalho em
   andamento não é descartado.
5. `handle_worker` agenda `pending_insert` e fecha o OSD imediatamente — sem
   preview do texto: ele aparece direto na app focada.
6. Evento `Closed` (emitido pela thread do OSD ao sair, após flush do
   destroy) → **só então** `insert::insert` digita na app focada → `Idle`.
7. `Esc`/`cancel` → `cancel_session`: mata captura, fecha OSD, descarta
   pendências. Em erros ou feedbacks como `nada detectado`, o OSD é mantido
   por uma deadline sem bloquear o loop; ao expirar, a sessão é cancelada.
   O aviso de Smart indisponível é não fatal: mantém `Recording`/`Paused`,
   deixa as ações disponíveis e desaparece na própria deadline.

**Invariante crítico:** a inserção acontece depois do `Closed`. Enquanto o
OSD está visível ele segura o foco de teclado — `wtype` digitando antes
escreveria no OSD, não na app. Não "otimize" isso.

## VAD (transcribe.rs)

O VAD (Silero via ggml, sem onnxruntime) é parte do `Engine`: carregado na
mesma thread de inicialização e residente entre sessões. A API do whisper.cpp
é batch — `whisper_vad_detect_speech` zera o estado LSTM a cada chamada —,
então o uso é no commit, sobre o buffer completo (`segments_from_samples`),
não em streaming. Como a API exige `&mut` e o `Engine` é compartilhado por
`Arc`, o contexto fica em `Option<Mutex<WhisperVadContext>>` (`None` =
modelo ausente/falho; nunca é opção de config).

- Parâmetros: defaults do whisper.cpp (threshold 0.5, fala mín. 250 ms,
  silêncio mín. 100 ms, pad 30 ms) — timestamps dos segmentos vêm em
  **centésimos de segundo** e já incluem o pad; `concat_segments` (função
  pura testada) converte para índices a 16 kHz (início arredonda para baixo,
  fim para cima, clamp). O `samples_overlap` do whisper.cpp não é aplicado
  no caminho standalone usado aqui (só no fluxo integrado do
  `whisper_full_with_state`).
- O VAD roda em CPU (o whisper.cpp força CPU nele) e não disputa a Vulkan
  com o modelo de transcrição.
- Falha de inferência (contexto carregado) é `Err` → erro de sessão;
  ausência do modelo é degradação silenciosa com aviso no log/OSD; zero
  segmentos é `Vec::new()` → fluxo "nada detectado".
- O daemon **nunca baixa** o modelo VAD: `whisper setup` é quem instala
  (`model::download`, idempotente); daemon ativo precisa reiniciar para
  carregar um VAD recém-instalado.

## Hot reload

- O loop principal compara o mtime do `config.toml` a cada tick (~1 s) em
  `reload_config_if_changed`.
- Config válida → troca `self.cfg` na hora; inválida → mantém a anterior e
  loga.
- Campos de sessão valem na próxima sessão; `model`/`gpu_device`/`threads`
  mudados marcam `pending_engine_reload`, e `reload_engine_if_pending`
  recarrega o engine em background quando o daemon está ocioso (falha
  mantém o atual). Mudanças em `[ai]` valem na próxima sessão e matam o
  servidor LLM; não recarregam o engine do whisper. O VAD não tem opção de
  config; instalar o modelo VAD com o daemon ativo exige reiniciar o daemon
  para ser carregado.

Ao adicionar uma opção nova em `config.rs` (com default), o hot reload a
pega automaticamente; se ela afetar o engine, inclua-a na comparação de
`reload_config_if_changed`.

## Qwen (pós-processamento)

O pós-processamento opcional usa `llama-server` ≥ b7973 como subprocesso local,
com o modelo Qwen3.5-0.8B GGUF servido sob demanda em `127.0.0.1`. A
configuração fica em `[ai]`: `enabled`, `model`, `context_size`, `gpu` e
`cleanup`. O modelo pode ser instalado com `whisper setup --ai-model
qwen3.5-0.8b`; ele não é baixado pelo daemon. Se o binário, modelo ou resposta
não estiverem disponíveis, o resultado é exatamente o fallback Rust atual
(`remove_fillers` + `fix_punctuation`). A tecla `S` alterna o smart mode, que
permite instruções naturais no início do ditado (como traduzir ou resumir).

O teste de integração do servidor real é ignorado por padrão e requer:

```sh
WHISPER_AI_MODEL=/caminho/Qwen_Qwen3.5-0.8B-Q5_K_M.gguf \
  cargo test llm::tests::process_real_model -- --ignored
```

## Download de modelos (model.rs)

Dois repositórios HuggingFace, mesmo mecanismo: modelos whisper em
`ggerganov/whisper.cpp` e o modelo VAD fixo (`ggml-silero-v6.2.0.bin`, ~865 KB)
em `ggml-org/whisper-vad`; `ModelSpec.base_url` guarda a origem de cada um.
O VAD fica fora de `MODELS` (não é selecionável no setup).

- Probe com `Range: bytes=0-0`: `206` + `Content-Range` → download paralelo
  (até `MAX_CONNECTIONS=16` conexões, cada uma gravando seu intervalo com
  `write_at`); `200` → servidor sem Range, fallback para uma conexão.
- Falha remove o arquivo parcial (idempotência: `dest.exists()` pula).

## Releases e cachix

- `release: types: [published]` (ou `workflow_dispatch` para testar na mão)
  dispara `.github/workflows/release.yml`: builda `nix build .#default`
  dentro do shell do devenv; o cachix-action (com o secret
  `CACHIX_AUTH_TOKEN`) empurra os store paths para
  `luiseduardobatista.cachix.org`.
- Máquinas NixOS consomem o cache pelo `nixConfig` do flake
  (extra-substituters + public key) ou via `nix.settings` no
  `configuration.nix` — baixam o binário pronto, sem rebuild.
- Antes de lançar: bump da versão no `Cargo.toml` e no `flake.nix`, tag
  `vX.Y.Z`, e release com `gh release create vX.Y.Z --generate-notes`.

## Testing

```sh
cargo test
```

Cobertura: pós-processamento (fillers/pontuação/RMS), parse de config e
Content-Range, catálogo de modelos (inclui VAD e Qwen), concatenação de
segmentos do VAD, validação do pós-processamento Qwen, composição de avisos do
OSD, smoke test do OSD, modo de inserção.

Integração real (ignorada por padrão — precisa modelo, VAD e WAV):

```sh
WHISPER_MODEL=~/.local/share/whisper/models/ggml-tiny.bin \
WHISPER_VAD_MODEL=~/.local/share/whisper/models/ggml-silero-v6.2.0.bin \
WHISPER_WAV=/tmp/jfk.wav \
cargo test transcribe_jfk vad_filters_real_audio -- --ignored
```

`transcribe_jfk` usa um path de VAD inexistente (transcreve o buffer
inteiro); `vad_filters_real_audio` valida o filtro real (fala presente,
saída menor ou igual à entrada, transcrição não vazia). Senoide não serve
como prova de fala para o VAD — só áudio real.

## Debugging

- `whisper status` responde `idle`/`recording`/`paused`/`transcribing`/
  `loading` — primeiro passo para saber onde o daemon está. Durante um
  feedback terminal, a sessão já está em `idle`, embora o OSD ainda mostre
  a mensagem até a deadline. O aviso não fatal de Smart mantém o estado
  `recording`/`paused`.
- `whisper status --json` emite somente JSON: `daemon` é `running` ou
  `stopped`; quando está rodando, `state` é a fase pública e os metadados de
  configuração/sessão podem aparecer. O estado `stopped` retorna exit code
  `0`; falhas reais de transporte ou parsing retornam `1` em stderr.
- `whisper stop` é idempotente e `whisper restart` espera o socket antigo
  desaparecer antes de chamar `start`.
- `whisper daemon` em primeiro plano mostra stderr ao vivo (hot reload,
  erros de engine, falhas de inserção).
- Falha de inserção não é visível no OSD (já fechado): olhe o
  `daemon.log` (`inserção falhou: ...`); quando a cópia funcionar apesar da
  digitação, o erro avisa que o texto ficou no clipboard.
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
  o OSD lê por frame; `osd_draw.rs` desenha com tiny-skia direto no buffer
  SHM (fonte DejaVu embutida em `assets/`).
