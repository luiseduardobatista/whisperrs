//! Daemon: orquestra as sessões de ditado — estado, áudio (pw-record),
//! OSD (teclas), transcrição (whisper-rs) e inserção (wtype/wl-copy).
use crate::audio::{self, Capture};
use crate::config::{config_mtime, Config, InsertMode};
use crate::insert;
use crate::ipc::{self, Cmd, Response};
use crate::osd::{OsdCommand, OsdEvent, Phase as UiPhase, UiState};
use crate::postprocess;
use crate::transcribe::Engine;
use anyhow::Result;
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Recording,
    Paused,
    Transcribing,
    Loading,
}

struct AudioChunk {
    samples: Vec<f32>,
    rms: f32,
}

enum WorkerOutcome {
    Transcribed { text: String },
    Failed(String),
}

enum DaemonEvent {
    Ipc(Cmd, Sender<Response>),
    Osd(OsdEvent),
    Audio(AudioChunk),
    AudioEnded(Option<String>),
    Worker(WorkerOutcome),
    EngineLoaded(Result<Arc<Engine>, String>),
}

struct Daemon {
    cfg: Config,
    /// mtime da config na última recarga (hot reload).
    cfg_mtime: Option<std::time::SystemTime>,
    /// A config pediu outro modelo/GPU/threads; recarrega quando ocioso.
    pending_engine_reload: bool,
    phase: Phase,
    engine: Option<Arc<Engine>>,
    buffer: Vec<f32>,
    capture: Option<Capture>,
    ui: Arc<Mutex<UiState>>,
    osd: Option<Sender<OsdCommand>>,
    /// Texto transcrito aguardando o OSD fechar para digitar na app
    /// (o OSD visível segura o foco de teclado; wtype digitaria nele).
    pending_insert: Option<String>,
    events_rx: Receiver<DaemonEvent>,
    events_tx: Sender<DaemonEvent>,
}

pub fn run() -> Result<()> {
    let cfg = Config::load()?;
    let (events_tx, events_rx) = channel::<DaemonEvent>();
    let ipc_tx = events_tx.clone();
    std::thread::spawn(move || {
        let handler = move |cmd: Cmd, reply: Sender<Response>| {
            let _ = ipc_tx.send(DaemonEvent::Ipc(cmd, reply));
        };
        if let Err(e) = ipc::serve(handler) {
            // Ex.: outro daemon já ativo — sair em vez de ficar ocioso à toa.
            eprintln!("whisper: ipc: {e:#}");
            std::process::exit(1);
        }
    });
    let mut daemon = Daemon {
        cfg,
        cfg_mtime: config_mtime(),
        pending_engine_reload: false,
        phase: Phase::Idle,
        engine: None,
        buffer: Vec::new(),
        capture: None,
        ui: Arc::new(Mutex::new(UiState::new(String::new()))),
        osd: None,
        pending_insert: None,
        events_rx,
        events_tx,
    };
    warn_if_wtype_missing(&daemon.cfg);
    daemon.loop_forever();
    let _ = std::fs::remove_file(crate::config::socket_path());
    Ok(())
}

/// Avisa (no stderr → daemon.log) se o wtype faltar no PATH quando o modo de
/// inserção depende dele: a digitação na app vai falhar e sobra só o clipboard.
fn warn_if_wtype_missing(cfg: &Config) {
    if matches!(cfg.insert_mode, InsertMode::Type | InsertMode::Both)
        && !insert::wtype_available()
    {
        eprintln!(
            "whisper: aviso: wtype não encontrado no PATH — a digitação na app focada vai \
             falhar (o texto irá só para o clipboard). {}",
            insert::wtype_hint()
        );
    }
}

fn lang_model(cfg: &Config) -> String {
    format!("{} · {}", cfg.language.label(), cfg.model)
}

impl Daemon {
    fn loop_forever(&mut self) {
        loop {
            self.reload_config_if_changed();
            self.reload_engine_if_pending();
            match self.events_rx.recv_timeout(Duration::from_secs(1)) {
                Ok(ev) => match ev {
                    DaemonEvent::Ipc(cmd, reply) => {
                        if matches!(cmd, Cmd::Stop) {
                            // Encerra sessão ativa (captura/OSD) e sai do loop.
                            self.cancel_session();
                            let _ = reply.send(Response {
                                ok: true,
                                state: "stopping".to_string(),
                                error: None,
                                exe: None,
                            });
                            break;
                        }
                        let resp = self.handle_cmd(cmd);
                        let _ = reply.send(resp);
                    }
                    DaemonEvent::Osd(ev) => self.handle_osd(ev),
                    DaemonEvent::Audio(chunk) => self.handle_audio(chunk),
                    DaemonEvent::AudioEnded(err) => self.handle_audio_ended(err),
                    DaemonEvent::Worker(out) => self.handle_worker(out),
                    DaemonEvent::EngineLoaded(res) => self.handle_engine_loaded(res),
                },
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    /// Hot reload: se o arquivo de config mudou, troca a config na hora.
    /// Campos usados por sessão (língua, pós-processamento, fonte, inserção)
    /// valem já na próxima sessão; se o modelo/GPU/threads mudaram, agenda a
    /// recarga do engine. Config inválida é ignorada (mantém a anterior).
    fn reload_config_if_changed(&mut self) {
        let mtime = config_mtime();
        if mtime == self.cfg_mtime {
            return;
        }
        self.cfg_mtime = mtime;
        match Config::load() {
            Ok(cfg) => {
                let engine_changed = cfg.model != self.cfg.model
                    || cfg.gpu_device != self.cfg.gpu_device
                    || cfg.threads != self.cfg.threads;
                self.cfg = cfg;
                if engine_changed {
                    self.pending_engine_reload = true;
                }
                eprintln!("whisper: config recarregada");
            }
            Err(e) => eprintln!("whisper: config inválida, mantendo a anterior: {e:#}"),
        }
    }

    /// Recarrega o modelo em background quando a config mudou e o daemon está
    /// ocioso; em caso de falha mantém o modelo atual (que continua valendo).
    fn reload_engine_if_pending(&mut self) {
        if !self.pending_engine_reload || self.phase != Phase::Idle || self.engine.is_none() {
            return;
        }
        self.pending_engine_reload = false;
        let cfg = self.cfg.clone();
        let tx = self.events_tx.clone();
        std::thread::spawn(move || {
            let res = Engine::load(&cfg.model_path(), cfg.gpu_device, cfg.threads)
                .map(Arc::new)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(DaemonEvent::EngineLoaded(res));
        });
    }

    fn handle_cmd(&mut self, cmd: Cmd) -> Response {
        match cmd {
            Cmd::Toggle => {
                if self.phase == Phase::Idle {
                    self.start_session();
                } else {
                    self.cancel_session();
                }
            }
            Cmd::Status => {}
            Cmd::Stop => {} // interceptado no loop_forever antes de chegar aqui
        }
        let mut resp = Response {
            ok: true,
            state: self.state_str().to_string(),
            error: None,
            exe: None,
        };
        if matches!(cmd, Cmd::Status) {
            // O CLI compara com o próprio binário: daemon de outra origem
            // (ex.: build de dev sem o wrapper do flake) é reiniciado no
            // próximo `start`.
            resp.exe = std::env::current_exe()
                .ok()
                .map(|p| p.display().to_string());
        }
        resp
    }

    fn state_str(&self) -> &'static str {
        match self.phase {
            Phase::Idle => "idle",
            Phase::Recording => "recording",
            Phase::Paused => "paused",
            Phase::Transcribing => "transcribing",
            Phase::Loading => "loading",
        }
    }

    fn handle_osd(&mut self, ev: OsdEvent) {
        match ev {
            OsdEvent::PauseToggle => match self.phase {
                Phase::Recording => {
                    self.phase = Phase::Paused;
                    self.set_ui(UiPhase::Paused, None);
                }
                Phase::Paused => {
                    self.phase = Phase::Recording;
                    self.set_ui(UiPhase::Recording, None);
                }
                _ => {}
            },
            OsdEvent::Commit => self.commit(),
            OsdEvent::Cancel => self.cancel_session(),
            OsdEvent::Closed => {
                if let Some(text) = self.pending_insert.take() {
                    // OSD fechado: o foco voltou à app, agora digita.
                    if let Err(e) = insert::insert(&text, self.cfg.insert_mode) {
                        // Em modo both o texto já está no clipboard.
                        eprintln!("whisper: inserção falhou (texto no clipboard): {e}");
                    }
                    self.finish_session(None);
                } else {
                    self.cancel_session();
                }
            }
        }
    }

    fn handle_audio(&mut self, chunk: AudioChunk) {
        if self.phase == Phase::Recording {
            self.buffer.extend_from_slice(&chunk.samples);
            self.ui.lock().unwrap().push_level(chunk.rms);
        }
    }

    fn handle_audio_ended(&mut self, err: Option<String>) {
        if matches!(self.phase, Phase::Recording | Phase::Paused) {
            // Sem detalhe do pw-record? Lê o stderr (ex.: "no target node available").
            let detail = match (&mut self.capture, err) {
                (Some(c), None) => c.stderr_text(),
                (_, Some(e)) => e,
                _ => String::new(),
            };
            let msg = if detail.is_empty() {
                "captura de áudio encerrou".to_string()
            } else {
                format!("áudio indisponível: {detail}")
            };
            self.set_ui(UiPhase::Error, Some(msg));
            std::thread::sleep(Duration::from_millis(2000));
            self.cancel_session();
        }
    }

    fn handle_worker(&mut self, out: WorkerOutcome) {
        match out {
            WorkerOutcome::Transcribed { text } => {
                if text.is_empty() {
                    self.finish_session(Some("nada detectado".to_string()));
                    return;
                }
                self.pending_insert = Some(text);
                // Sem preview do texto: fecha o OSD assim que a transcrição
                // termina e digita direto na app focada (via evento Closed).
                self.close_osd();
            }
            WorkerOutcome::Failed(msg) => {
                self.set_ui(UiPhase::Error, Some(msg));
                std::thread::sleep(Duration::from_millis(2500));
                self.finish_session(None);
            }
        }
    }

    fn handle_engine_loaded(&mut self, res: Result<Arc<Engine>, String>) {
        match res {
            Ok(engine) => {
                self.engine = Some(engine);
                if self.phase == Phase::Loading {
                    self.phase = Phase::Recording;
                    self.set_ui(UiPhase::Recording, None);
                    self.start_capture();
                } else if self.phase == Phase::Idle {
                    // Troca de modelo em background (hot reload da config).
                    eprintln!("whisper: modelo recarregado");
                }
            }
            Err(err) => {
                if self.phase == Phase::Loading {
                    self.set_ui(UiPhase::Error, Some(format!("modelo indisponível: {err}")));
                    std::thread::sleep(Duration::from_millis(2500));
                    self.cancel_session();
                } else if self.phase == Phase::Idle {
                    eprintln!("whisper: falha ao recarregar modelo, mantendo o atual: {err}");
                }
            }
        }
    }

    fn start_session(&mut self) {
        let mut ui = UiState::new(lang_model(&self.cfg));
        // Aviso visível no OSD (rodapé do cartão): sem wtype o texto não é
        // digitado na app focada e fica só no clipboard — o usuário vê antes
        // de ditar, mesmo que o daemon tenha subido sem console.
        if matches!(self.cfg.insert_mode, InsertMode::Type | InsertMode::Both)
            && !insert::wtype_available()
        {
            ui.warning = Some(
                "wtype ausente — a digitação na app não vai funcionar (só clipboard)".to_string(),
            );
        }
        self.ui = Arc::new(Mutex::new(ui));
        let (osd_tx, osd_rx) = channel();
        let (osd_ev_tx, osd_ev_rx) = channel::<OsdEvent>();
        let ui = self.ui.clone();
        let daemon_tx = self.events_tx.clone();
        std::thread::spawn(move || {
            while let Ok(ev) = osd_ev_rx.recv() {
                if daemon_tx.send(DaemonEvent::Osd(ev)).is_err() {
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            if let Err(e) = crate::osd::run(ui, osd_ev_tx.clone(), osd_rx) {
                eprintln!("whisper: osd: {e:#}");
            }
            // OSD saiu (Close recebido ou erro): superfície destruída e foco
            // de teclado já voltou à app — sinal seguro para digitar.
            let _ = osd_ev_tx.send(OsdEvent::Closed);
        });
        self.osd = Some(osd_tx);
        self.buffer.clear();

        if self.engine.is_none() {
            self.phase = Phase::Loading;
            self.set_ui(UiPhase::Loading, Some("carregando modelo…".to_string()));
            let cfg = self.cfg.clone();
            let tx = self.events_tx.clone();
            std::thread::spawn(move || {
                let res = Engine::load(&cfg.model_path(), cfg.gpu_device, cfg.threads)
                    .map(Arc::new)
                    .map_err(|e| format!("{e:#}"));
                let _ = tx.send(DaemonEvent::EngineLoaded(res));
            });
        } else {
            self.phase = Phase::Recording;
            self.set_ui(UiPhase::Recording, None);
            self.start_capture();
        }
    }

    fn start_capture(&mut self) {
        match Capture::start(self.cfg.source.as_deref()) {
            Ok((capture, stdout)) => {
                self.capture = Some(capture);
                let tx = self.events_tx.clone();
                std::thread::spawn(move || {
                    let mut stdout = stdout;
                    loop {
                        match audio::read_chunk(&mut stdout) {
                            Ok((samples, rms)) if !samples.is_empty() => {
                                if tx.send(DaemonEvent::Audio(AudioChunk { samples, rms })).is_err() {
                                    break;
                                }
                            }
                            Ok(_) => {
                                let _ = tx.send(DaemonEvent::AudioEnded(None));
                                break;
                            }
                            Err(e) => {
                                let _ = tx.send(DaemonEvent::AudioEnded(Some(e.to_string())));
                                break;
                            }
                        }
                    }
                });
            }
            Err(e) => {
                self.set_ui(UiPhase::Error, Some(format!("áudio: {e:#}")));
                std::thread::sleep(Duration::from_millis(2000));
                self.cancel_session();
            }
        }
    }

    fn commit(&mut self) {
        if !matches!(self.phase, Phase::Recording | Phase::Paused) {
            return;
        }
        self.stop_capture();
        if self.buffer.is_empty() {
            self.finish_session(Some("nada gravado".to_string()));
            return;
        }
        let engine = match &self.engine {
            Some(e) => Arc::clone(e),
            None => {
                self.finish_session(Some("modelo não carregado: rode 'whisper setup'".to_string()));
                return;
            }
        };
        self.phase = Phase::Transcribing;
        self.set_ui(UiPhase::Transcribing, Some("transcrevendo…".to_string()));
        let samples = std::mem::take(&mut self.buffer);
        let cfg = self.cfg.clone();
        let tx = self.events_tx.clone();
        std::thread::spawn(move || {
            let out = transcribe_worker(&engine, samples, &cfg);
            let _ = tx.send(DaemonEvent::Worker(out));
        });
    }

    fn stop_capture(&mut self) {
        if let Some(mut capture) = self.capture.take() {
            capture.stop();
        }
    }

    fn set_ui(&mut self, phase: UiPhase, status: Option<String>) {
        if let Ok(mut ui) = self.ui.lock() {
            ui.phase = phase;
            ui.status = status;
        }
    }

    fn close_osd(&mut self) {
        if let Some(tx) = self.osd.take() {
            let _ = tx.send(OsdCommand::Close);
        }
    }

    fn cancel_session(&mut self) {
        self.stop_capture();
        self.close_osd();
        self.pending_insert = None; // Esc/toggle descarta a inserção pendente
        self.phase = Phase::Idle;
    }

    fn finish_session(&mut self, msg: Option<String>) {
        self.stop_capture();
        if let Some(msg) = msg {
            self.set_ui(UiPhase::Error, Some(msg));
            std::thread::sleep(Duration::from_millis(1200));
        }
        self.close_osd();
        self.phase = Phase::Idle;
    }
}

/// Pipeline de transcrição (roda na thread de trabalho):
/// corta silêncio → whisper → fillers → pontuação → insere na app.
fn transcribe_worker(engine: &Engine, mut samples: Vec<f32>, cfg: &Config) -> WorkerOutcome {
    if cfg.trim_silence {
        samples = postprocess::trim_silence(&samples, audio::SAMPLE_RATE, 0.01, 500, 300);
    }
    if samples.is_empty() {
        return WorkerOutcome::Transcribed { text: String::new() };
    }
    let raw = match engine.transcribe(&samples, cfg.language.whisper_code()) {
        Ok(text) => text,
        Err(e) => return WorkerOutcome::Failed(format!("transcrição falhou: {e:#}")),
    };
    let mut text = raw;
    if cfg.remove_fillers {
        text = postprocess::remove_fillers(&text, cfg.language);
    }
    if cfg.punctuation {
        text = postprocess::fix_punctuation(&text, cfg.final_period);
    }
    // A inserção (wtype) fica com o daemon, DEPOIS que o OSD fechar: com o
    // OSD visível o foco de teclado é dele e as teclas não chegariam à app.
    WorkerOutcome::Transcribed { text }
}
