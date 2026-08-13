# Uso

Esta página cobre os comandos do whisper, a sessão de ditado, a integração
com compositor/systemd e os problemas comuns.

## Comandos

| Comando | Descrição |
|---------|-----------|
| `whisper setup [--lang pt\|en\|auto] [--model …]` | wizard: língua + modelo + download (gera o `config.toml`) |
| `whisper start` | sobe o daemon em background; idempotente |
| `whisper stop` | derruba o daemon (encerra sessão ativa e remove o socket) |
| `whisper status` | mostra o estado do daemon (`idle`, `recording`, …) |
| `whisper toggle` | inicia uma sessão de ditado; ativo = cancela |
| `whisper daemon` | daemon em primeiro plano (debug / systemd) |

### start

`whisper start` spawna o daemon **destacado do terminal**: grupo de processo
próprio (não morre quando o shell fecha), stdin nulo e stdout/stderr em
`~/.local/state/whisper/daemon.log` (respeita `XDG_STATE_HOME`). Antes de
devolver o shell, confirma que o daemon respondeu no socket; se não
responder, mata o processo e aponta o log. Com o daemon já rodando, apenas
avisa "já está rodando" e sai com sucesso.

### stop

`whisper stop` envia o comando `Stop` pelo socket. O daemon encerra a sessão
ativa (captura e OSD), responde `stopping`, remove o socket e sai.

### setup

`whisper setup` é um wizard interativo de língua e modelo; flags `--lang` e
`--model` pulam a interação. Baixa o modelo (se ainda não existir) e salva
`~/.config/whisper/config.toml`. O download usa **conexões paralelas** (HTTP
Range, até 16, como o aria2c `-x 16`) para aproveitar a banda; falha remove o
arquivo parcial para não envenenar uma próxima tentativa.

## Sessão de ditado

O `toggle` abre o OSD na borda inferior da tela com a waveform ao vivo:

| Tecla | Ação |
|-------|------|
| `Space` | pausar/retomar a gravação |
| `Enter` | concluir: transcreve, insere na app focada e fecha |
| `Esc` | cancelar e descartar |

Fluxo interno de uma sessão:

1. `toggle` → o modelo é carregado na primeira sessão (fica residente em
   VRAM) e a captura começa (`pw-record`, f32 mono 16 kHz).
2. `Enter` → para a captura e transcreve em batch (Vulkan, ~1 s no turbo).
3. Pós-processamento: corte de silêncio, remoção de fillers,
   capitalização/pontuação e período final (configurável).
4. O OSD mostra o texto transcrito, fecha e **só então** o texto é digitado
   na app focada (`wtype`) e copiado para o clipboard (`wl-copy`). A ordem
   importa: enquanto o OSD está visível ele segura o foco de teclado, e o
   `wtype` digitaria nele — por isso a inserção espera o OSD fechar.

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
