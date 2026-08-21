# Configuração

A configuração fica em `~/.config/whisper/config.toml` (respeita
`XDG_CONFIG_HOME`), gerada pelo `whisper setup`. O daemon observa o arquivo e
aplica mudanças em ~1 s, sem reiniciar (hot reload).

## Opções

```toml
language = "pt"        # pt | en | auto
model = "small"        # recomendado; tiny | base | small | medium | large-v3 | turbo
insert_mode = "fallback" # insert | clipboard | fallback | both
remove_fillers = true  # remove "hmm", "ahn"…
punctuation = true     # capitaliza frases e normaliza espaços
final_period = true    # garante ponto final
gpu_device = 0
threads = 4
# source = "nome.do.no.pipewire"   # fonte de áudio explícita (pw-record --target)
```

| Opção | Valores | Default | Efeito |
|-------|---------|---------|--------|
| `language` | `pt` / `en` / `auto` | `pt` | língua forçada na transcrição; `auto` deixa o whisper detectar |
| `model` | `tiny`…`turbo` | `small` | modelo whisper.cpp; `small` é o recomendado pelo setup (ver [Modelos](#modelos)) |
| `insert_mode` | `insert` / `clipboard` / `fallback` / `both` | `fallback` | digita, copia, usa clipboard só se necessário ou faz ambos |
| `remove_fillers` | `true` / `false` | `true` | remove fillers ("hmm", "ahn"…) |
| `punctuation` | `true` / `false` | `true` | capitaliza frases e normaliza espaços |
| `final_period` | `true` / `false` | `true` | garante ponto final |
| `gpu_device` | inteiro | `0` | índice do dispositivo Vulkan |
| `threads` | inteiro | `4` | threads da transcrição |
| `source` | nome de nó PipeWire | `null` | fonte de áudio explícita (`pw-record --target`) |

`insert_mode = "type"` continua aceito como alias legado de `"insert"`. Configurações
existentes com `"both"` preservam o comportamento anterior; o novo padrão só se
aplica quando o campo está ausente.

## Detecção de voz (VAD)

O VAD (Silero, via ggml) está **sempre ativo** — não há opção de configuração.
No `Enter` (concluir), o buffer gravado passa pelo VAD, que extrai só os
segmentos de fala; apenas esse áudio vai para o whisper. Silêncio das bordas e
pausas longas no meio do ditado somem da transcrição.

Parâmetros fixos do VAD:

| Parâmetro | Valor |
|-----------|-------|
| limiar de fala (`threshold`) | 0,5 |
| fala mínima (`min_speech_duration_ms`) | 250 ms |
| silêncio mínimo para encerrar fala (`min_silence_duration_ms`) | 100 ms |
| padding nas bordas (`speech_pad_ms`) | 30 ms |

O overlap de segmentos (`samples_overlap`) está no default do whisper.cpp,
mas **não é aplicado** no caminho standalone usado aqui (só no fluxo
integrado do `whisper_full_with_state`) — os segmentos são concatenados sem
sobreposição extra.

O VAD roda em **CPU** (o whisper.cpp força CPU nele) e não disputa a Vulkan
com o modelo de transcrição. O modelo é o `ggml-silero-v6.2.0.bin` (~865 KB),
baixado pelo `whisper setup` do repositório `ggml-org/whisper-vad` para
`~/.local/share/whisper/models/`.

**Fallback**: se o modelo VAD estiver ausente ou corrompido, o ditado continua
funcionando — o buffer inteiro é transcrito, sem filtro de voz — com aviso no
log (`~/.local/state/whisper/daemon.log`) e no rodapé do OSD. Rode
`whisper setup` para instalar e reinicie o daemon (`whisper stop && whisper
start`) se ele já estiver ativo: a presença do arquivo não dispara o hot
reload.

## Modelos

Multilíngues, oficiais do whisper.cpp (HuggingFace), baixados para
`~/.local/share/whisper/models/` (respeita `XDG_DATA_HOME`):

| Modelo | Perfil | Arquivo | Tamanho |
|--------|--------|---------|---------|
| `tiny` | muito leve · menor precisão | `ggml-tiny.bin` | 75 MB |
| `base` | leve · para máquinas modestas | `ggml-base.bin` | 142 MB |
| `small` | **recomendado · equilíbrio entre precisão e recursos** | `ggml-small.bin` | 466 MB |
| `medium` | mais preciso · mais pesado | `ggml-medium.bin` | 1,5 GB |
| `large-v3` | máxima precisão · muito pesado | `ggml-large-v3.bin` | 3,1 GB |
| `turbo` | rápido em hardware forte · download maior | `ggml-large-v3-turbo.bin` | 1,6 GB |

O `whisper setup` mostra um resumo e o tamanho aproximado dos arquivos que
faltam antes de baixar o modelo escolhido **e** o modelo VAD (fixo).
O daemon nunca baixa modelos. O modelo de transcrição é carregado na
primeira sessão de ditado e
fica residente em VRAM até o daemon parar. Os downloads usam **conexões
paralelas** (HTTP Range,
até 16, como o aria2c `-x 16`) para aproveitar a banda da internet; se o
servidor não suportar Range, cai para uma única conexão. Uma falha remove o
arquivo parcial.

## Hot reload

O daemon compara o mtime do `config.toml` a cada ~1 s e, ao detectar mudança,
recarrega a config sem reiniciar:

- **Campos de sessão** (`language`, `insert_mode`, `remove_fillers`,
  `punctuation`, `final_period`, `source`) valem já na próxima sessão — e até
  numa gravação em andamento, no momento do `Enter`.
- **`model`/`gpu_device`/`threads`** recarregam o modelo em background quando
  o daemon está ocioso; em caso de falha, mantém o modelo atual.
- **Config inválida** (erro de TOML) é ignorada: o daemon mantém a anterior e
  loga o erro em `~/.local/state/whisper/daemon.log`.

Isso também vale para `whisper setup`: rodar o wizard com o daemon vivo já
aplica a configuração sem reiniciar. Um VAD recém-instalado exige reiniciar o
daemon (`whisper restart`) para ser carregado.

## Teste de integração

Transcrição com modelo e áudio reais (ignorado por padrão):

```sh
WHISPER_MODEL=~/.local/share/whisper/models/ggml-tiny.bin \
WHISPER_WAV=/tmp/jfk.wav \
cargo test transcribe_jfk -- --ignored
```

Com VAD (filtro de fala + transcrição):

```sh
WHISPER_MODEL=~/.local/share/whisper/models/ggml-tiny.bin \
WHISPER_VAD_MODEL=~/.local/share/whisper/models/ggml-silero-v6.2.0.bin \
WHISPER_WAV=/tmp/jfk.wav \
cargo test vad_filters_real_audio -- --ignored
```
