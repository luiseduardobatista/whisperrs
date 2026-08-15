# Uso

Esta página cobre os comandos do whisper, a sessão de ditado, a integração
com compositor/systemd e os problemas comuns.

## Comandos

| Comando | Descrição |
|---------|-----------|
| `whisper setup [--lang pt\|en\|auto] [--model …] [--ai-model qwen3.5-0.8b]` | wizard: língua + modelos + download (gera o `config.toml`) |
| `whisper start` | sobe o daemon em background; idempotente |
| `whisper stop` | para o daemon; é sucesso se ele já estiver parado |
| `whisper restart` | compõe `stop` e `start`, aguardando o socket antigo sair |
| `whisper status` | mostra o estado humano do daemon (`idle`, `recording`, …) |
| `whisper status --json` | emite status estável para scripts e status bars |
| `whisper toggle` | inicia ou conclui a sessão; em `Loading`/`Transcribing`, não faz nada |
| `whisper record` | inicia uma sessão ou retoma uma sessão pausada |
| `whisper commit` | conclui a sessão em gravação/pausa |
| `whisper cancel` | cancela e descarta a sessão atual |
| `whisper pause` | pausa uma sessão em gravação |
| `whisper daemon` | daemon em primeiro plano (debug / systemd) |

### start

`whisper start` spawna o daemon **destacado do terminal**: grupo de processo
próprio (não morre quando o shell fecha), stdin nulo e stdout/stderr em
`~/.local/state/whisper/daemon.log` (respeita `XDG_STATE_HOME`). Antes de
devolver o shell, confirma que o daemon respondeu no socket; se não
responder, mata o processo e aponta o log. Com o daemon já rodando, apenas
avisa "já está rodando" e sai com sucesso.

### stop e restart

`whisper stop` envia o comando `Stop` pelo socket. O daemon encerra a sessão
ativa (captura e OSD), remove o socket e sai. Se o daemon já estiver parado,
o comando retorna sucesso sem tratar isso como erro técnico.

`whisper restart` reutiliza o mesmo lifecycle: solicita `stop`, aguarda o
socket antigo desaparecer e executa `start`. Falhas reais de IPC ou startup
retornam exit code `1`.

### status

O modo humano mostra o estado da sessão quando o daemon está disponível e
`stopped` quando não há listener no socket. O modo JSON escreve exatamente um
objeto em stdout, sem texto humano, ANSI ou avisos:

```json
{"daemon":"running","state":"recording","language":"pt","model":"turbo","smart":false,"exe":"/caminho/para/whisper"}
```

Campos `language`, `model`, `smart` e `exe` são opcionais para compatibilidade
com daemons antigos. `smart` representa somente o Smart Mode da sessão atual;
`language` é o idioma configurado (`pt`, `en` ou `auto`) e `model` é o modelo
configurado, não uma garantia de que o engine já foi carregado. Se o daemon
estiver parado, o JSON é `{"daemon":"stopped"}` e o exit code é `0`.

Falhas de permissão, transporte, leitura ou parsing não são convertidas em
`stopped`: stdout fica vazio, o diagnóstico vai para stderr e o exit code é
`1`.

### setup

`whisper setup` é um wizard interativo de língua e modelo; flags `--lang` e
`--model` pulam a interação. `--ai-model qwen3.5-0.8b` baixa o GGUF Qwen e o
ativa no catálogo, sem alterar `ai.enabled`; sem a flag, o wizard pergunta ao
final se deseja baixá-lo (default: não). O modelo é carregado sob demanda.
Baixa os modelos (se ainda não existirem) e salva
`~/.config/whisper/config.toml`. O download usa **conexões paralelas** (HTTP
Range, até 16, como o aria2c `-x 16`) para aproveitar a banda; falha remove o
arquivo parcial para não envenenar uma próxima tentativa.

## Sessão de ditado

O `toggle` inicia uma sessão em `Idle` ou conclui uma sessão em
`Recording`/`Paused`. Durante `Loading` ou `Transcribing`, é um no-op. O OSD
abre na borda inferior da tela com a waveform ao vivo:

| Tecla | Ação |
|-------|------|
| `Space` | pausar/retomar a gravação |
| `Enter` | concluir: transcreve, insere na app focada e fecha |
| `Esc` | cancelar e descartar |
| `S` | alternar o modo Smart quando Qwen estiver disponível |

Fluxo interno de uma sessão:

1. `toggle`/`record` em `Idle` → o modelo é carregado na primeira sessão (fica
   residente em VRAM) e a captura começa (`pw-record`, f32 mono 16 kHz).
2. `Space`/`pause` pausa a captura logicamente; `Space` ou `record` retoma.
3. `toggle`/`Enter`/`commit` em `Recording` ou `Paused` → para a captura e
   transcreve em batch (Vulkan, ~1 s no turbo).
4. Pós-processamento: corte de silêncio, remoção de fillers,
   capitalização/pontuação e período final (configurável).
5. O OSD fecha e **só então** o texto é digitado na app focada (`wtype`) e
   copiado para o clipboard (`wl-copy`). A ordem importa: enquanto o OSD está
   visível ele segura o foco de teclado, e o `wtype` digitaria nele — por isso
   a inserção espera o OSD fechar.
6. `Esc`/`cancel` descarta explicitamente a sessão, inclusive durante o
   carregamento ou a transcrição. Mensagens temporárias de erro ou de ausência
   de fala permanecem no OSD pelo tempo planejado sem bloquear `status`,
   `stop` ou novos comandos. Se o Smart Mode estiver indisponível, o aviso é
   temporário e a gravação continua ativa.

A língua é forçada no modelo (mais rápido e preciso que auto-detect); `auto`
deixa o whisper decidir. Acentos vêm do próprio modelo.

## Integrações

### Niri

```kdl
binds {
    Mod+Shift+Space { spawn "whisper toggle"; }
}
spawn-at-startup "whisper start"
```

### systemd (qualquer DE/WM Wayland)

```ini
# ~/.config/systemd/user/whisper.service
[Unit]
Description=whisper daemon
After=graphical-session.target pipewire.service
PartOf=graphical-session.target

[Service]
ExecStart=/caminho/para/whisper daemon
Restart=on-failure

[Install]
WantedBy=graphical-session.target
```

```sh
systemctl --user enable --now whisper.service
```

No systemd o daemon roda em primeiro plano — daí `daemon` e não `start`. No
NixOS, prefira um `systemd.user.services` no config (e garanta que o binário
encontre `libvulkan`/`libxkbcommon` em runtime).

## Troubleshooting

| Sintoma | Causa provável | Solução |
|---------|----------------|---------|
| `daemon não está rodando` | daemon não foi iniciado | `whisper start` |
| OSD mostra "modelo indisponível" | modelo não baixado | `whisper setup` |
| OSD mostra "áudio indisponível" | sem microfone / sem PipeWire | confira `pw-record` e a fonte (`source` na config) |
| "nada detectado" | silêncio na gravação | fale mais alto/próximo do microfone |
| Texto só no clipboard, não na app | `wtype` falhou (ex.: app sem foco de teclado) | o texto está no clipboard (modo `both`); veja `~/.local/state/whisper/daemon.log` |
| Nada acontece ao rodar `whisper daemon` | é um serviço de primeiro plano silencioso | normal: use `whisper start`/`status` |
| OSD não aparece | X11 | o whisper não suporta X11; use um compositor Wayland |

Dúvidas sobre o arquivo de config: [configuration.md](configuration.md).
