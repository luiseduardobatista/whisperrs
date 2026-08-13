# Configuração

A configuração fica em `~/.config/whisper/config.toml` (respeita
`XDG_CONFIG_HOME`), gerada pelo `whisper setup`. O daemon observa o arquivo e
aplica mudanças em ~1 s, sem reiniciar (hot reload).

## Opções

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

| Opção | Valores | Default | Efeito |
|-------|---------|---------|--------|
| `language` | `pt` / `en` / `auto` | `pt` | língua forçada na transcrição; `auto` deixa o whisper detectar |
| `model` | `tiny`…`turbo` | `turbo` | modelo whisper.cpp (ver [Modelos](#modelos)) |
| `insert_mode` | `type` / `clipboard` / `both` | `both` | digita na app (`wtype`), copia (`wl-copy`) ou ambos |
| `remove_fillers` | `true` / `false` | `true` | remove fillers ("hmm", "ahn"…) |
| `trim_silence` | `true` / `false` | `true` | corta silêncio das bordas e colapsa pausas longas |
| `punctuation` | `true` / `false` | `true` | capitaliza frases e normaliza espaços |
| `final_period` | `true` / `false` | `true` | garante ponto final |
| `gpu_device` | inteiro | `0` | índice do dispositivo Vulkan |
| `threads` | inteiro | `4` | threads da transcrição |
| `source` | nome de nó PipeWire | `null` | fonte de áudio explícita (`pw-record --target`) |

## Modelos

Multilíngues, oficiais do whisper.cpp (HuggingFace), baixados para
`~/.local/share/whisper/models/` (respeita `XDG_DATA_HOME`):

| Modelo | Arquivo | Tamanho |
|--------|---------|---------|
| `tiny` | `ggml-tiny.bin` | 75 MB |
| `base` | `ggml-base.bin` | 142 MB |
| `small` | `ggml-small.bin` | 466 MB |
| `medium` | `ggml-medium.bin` | 1,5 GB |
| `large-v3` | `ggml-large-v3.bin` | 3,1 GB |
| `turbo` | `ggml-large-v3-turbo.bin` | 1,6 GB |

O modelo é carregado na primeira sessão de ditado e fica residente em VRAM
até o daemon parar. O download usa **conexões paralelas** (HTTP Range, até
16, como o aria2c `-x 16`) para aproveitar a banda da internet; se o servidor
não suportar Range, cai para uma única conexão. Uma falha remove o arquivo
parcial.

## Hot reload

O daemon compara o mtime do `config.toml` a cada ~1 s e, ao detectar mudança,
recarrega a config sem reiniciar:

- **Campos de sessão** (`language`, `insert_mode`, `remove_fillers`,
  `trim_silence`, `punctuation`, `final_period`, `source`) valem já na
  próxima sessão — e até numa gravação em andamento, no momento do `Enter`.
- **`model`/`gpu_device`/`threads`** recarregam o modelo em background quando
  o daemon está ocioso; em caso de falha, mantém o modelo atual.
- **Config inválida** (erro de TOML) é ignorada: o daemon mantém a anterior e
  loga o erro em `~/.local/state/whisper/daemon.log`.

Isso também vale para `whisper setup`: rodar o wizard com o daemon vivo já
aplica tudo sem reiniciar.

## Teste de integração

Transcrição com modelo e áudio reais (ignorado por padrão):

```sh
WHISPER_MODEL=~/.local/share/whisper/models/ggml-tiny.bin \
WHISPER_WAV=/tmp/jfk.wav \
cargo test --release transcribe_jfk -- --ignored
```
