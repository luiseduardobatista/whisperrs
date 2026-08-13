# whisper

Ditado por voz minimalista para Linux/Wayland, em Rust: **whisper.cpp com
aceleração Vulkan** (sem backend de nuvem), OSD com waveform e hotkeys, e
inserção do texto na app focada. Feito para uso pessoal.

Inspirado no [whisrs](https://github.com/y0sif/whisrs), com a diferença
central: o whisper.cpp é usado **upstream com GPU** (Vulkan → RADV em AMD,
também funciona em NVIDIA via driver Vulkan).

## Fluxo

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
  (modo configurável; se o wtype falhar, o texto está no clipboard).
- OSD em `wlr-layer-shell` (Niri, Sway, Hyprland, KDE ≥ 6.3, COSMIC) com
  fallback para janela xdg-toplevel (GNOME). Sem suporte a X11.

## Build

```sh
nix develop          # entra no shell com cmake/clang/vulkan/etc.
cargo build --release
```

Ou instale direto: `nix build` → `result/bin/whisper`.

## Uso

```sh
whisper setup              # wizard: língua (pt/en/auto) + modelo + download
whisper daemon             # sobe o daemon (roda em background)
whisper toggle             # bind no compositor (tecla para iniciar)
whisper status             # estado do daemon
```

Modelos (multilíngues, HuggingFace): `tiny`, `base`, `small`, `medium`,
`large-v3`, `turbo` (default `turbo`). Ficam em
`~/.local/share/whisper/models/`.

### Niri

```kdl
binds {
    Mod+Shift+Space { spawn "whisper toggle"; }
}
spawn-at-startup "whisper daemon"
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

No NixOS, prefira um `systemd.user.services` no config (e garanta que o
binário encontre `libvulkan`/`libxkbcommon` em runtime).

## Configuração

`~/.config/whisper/config.toml` (gerada pelo `whisper setup`):

```toml
language = "pt"        # pt | en | auto
model = "turbo"        # tiny | base | small | medium | large-v3 | turbo
insert_mode = "both"   # type | clipboard | both
remove_fillers = true  # remove "hmm", "ahn"…
trim_silence = true    # corta silêncio das bordas e colapsa pausas longas
punctuation = true     # capitaliza frases e normaliza espaços
final_period = true    # garante ponto final
gpu_device = 0
threads = 4
# source = "nome.do.no.pipewire"   # fonte de áudio explícita (pw-record --target)
```

A língua é **forçada** no modelo (mais rápido e preciso que auto-detect);
`auto` deixa o whisper decidir. Acentos vêm do próprio modelo.

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
