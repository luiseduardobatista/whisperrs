//! Daemon: orquestra as sessões de ditado — estado, áudio (pw-record),
//! OSD (teclas), transcrição (whisper-rs) e inserção (wtype/wl-copy).
use crate::audio::{self, Capture};
use crate::config::Config;
use crate::insert;
use crate::ipc::{self, Cmd, Response};
use crate::osd::{OsdCommand, OsdEvent, Phase as UiPhase, UiState};
use crate::postprocess;
use crate::transcribe::Engine;
use anyhow::Result;
use std::sync::mpsc::{channel, Receiver, Sender};
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
    Transcribed {
        text: String,
        inserted: Result<(), String>,
    },
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
    phase: Phase,
    engine: Option<Arc<Engine>>,
    buffer: Vec<f32>,
    capture: Option<Capture>,
    ui: Arc<Mutex<UiState>>,
    osd: Option<Sender<OsdCommand>>,
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
            eprintln!("whisper: ipc: {e:#}");
        }
    });
    let mut daemon = Daemon {
        cfg,
        phase: Phase::Idle,
        engine: None,
        buffer: Vec::new(),
        capture: None,
        ui: Arc::new(Mutex::new(UiState::new(String::new()))),
        osd: None,
        events_rx,
        events_tx,
    };
    daemon.loop_forever();
    Ok(())
}

fn lang_model(cfg: &Config) -> String {
    format!("{} · {}", cfg.language.label(), cfg.model)
}

impl Daemon {
    fn loop_forever(&mut self) {
        while let Ok(ev) = self.events_rx.recv() {
            match ev {
                DaemonEvent::Ipc(cmd, reply) => {
                    let resp = self.handle_cmd(cmd);
                    let _ = reply.send(resp);
                }
                DaemonEvent::Osd(ev) => self.handle_osd(ev),
                DaemonEvent::Audio(chunk) => self.handle_audio(chunk),
                DaemonEvent::AudioEnded(err) => self.handle_audio_ended(err),
                DaemonEvent::Worker(out) => self.handle_worker(out),
                DaemonEvent::EngineLoaded(res) => self.handle_engine_loaded(res),
            }
        }
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
        }
        Response { ok: true, state: self.state_str().to_string(), error: None }
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
            OsdEvent::Cancel | OsdEvent::Closed => self.cancel_session(),
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
            WorkerOutcome::Transcribed { text, inserted } => {
                if text.is_empty() {
                    self.finish_session(Some("nada detectado".to_string()));
                    return;
                }
                let status = match inserted {
                    Ok(()) => format!("✓  {text}"),
                    Err(e) => format!("{text}   ⚠ {e}"),
                };
                self.set_ui(UiPhase::Transcribing, Some(status));
                std::thread::sleep(Duration::from_millis(1600));
                self.finish_session(None);
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
                }
            }
            Err(err) => {
                if self.phase == Phase::Loading {
                    self.set_ui(UiPhase::Error, Some(format!("modelo indisponível: {err}")));
                    std::thread::sleep(Duration::from_millis(2500));
                    self.cancel_session();
                }
            }
        }
    }

    fn start_session(&mut self) {
        self.ui = Arc::new(Mutex::new(UiState::new(lang_model(&self.cfg))));
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
            if let Err(e) = crate::osd::run(ui, osd_ev_tx, osd_rx) {
                eprintln!("whisper: osd: {e:#}");
            }
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
        return WorkerOutcome::Transcribed { text: String::new(), inserted: Ok(()) };
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
    let inserted = insert::insert(&text, cfg.insert_mode).map_err(|e| format!("{e}"));
    WorkerOutcome::Transcribed { text, inserted }
}
