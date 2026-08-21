//! Daemon: orquestra as sessões de ditado — estado, áudio (pw-record),
//! OSD (teclas), transcrição (whisper-rs) e inserção (wtype/wl-copy).
use crate::audio::{self, Capture};
use crate::config::{Config, config_mtime};
use crate::insert;
use crate::ipc::{self, Cmd, Response};
use crate::model;
use crate::osd::{Feedback, FeedbackKind, OsdCommand, OsdEvent, Phase as UiPhase, UiState};
use crate::postprocess;
use crate::transcribe::Engine;
use anyhow::Result;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Recording,
    Paused,
    Transcribing,
    Loading,
}

impl Phase {
    fn ui_phase(self) -> Option<UiPhase> {
        match self {
            Phase::Idle => None,
            Phase::Recording => Some(UiPhase::Recording),
            Phase::Paused => Some(UiPhase::Paused),
            Phase::Transcribing => Some(UiPhase::Transcribing),
            Phase::Loading => Some(UiPhase::Loading),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandAction {
    Start,
    Resume,
    Commit,
    Cancel,
    Pause,
    Noop,
}

fn command_action(phase: Phase, cmd: Cmd) -> CommandAction {
    match cmd {
        Cmd::Toggle => match phase {
            Phase::Idle => CommandAction::Start,
            Phase::Recording | Phase::Paused => CommandAction::Commit,
            Phase::Loading | Phase::Transcribing => CommandAction::Noop,
        },
        Cmd::Record => match phase {
            Phase::Idle => CommandAction::Start,
            Phase::Paused => CommandAction::Resume,
            Phase::Recording | Phase::Loading | Phase::Transcribing => CommandAction::Noop,
        },
        Cmd::Commit => match phase {
            Phase::Recording | Phase::Paused => CommandAction::Commit,
            Phase::Idle | Phase::Loading | Phase::Transcribing => CommandAction::Noop,
        },
        Cmd::Cancel => match phase {
            Phase::Recording | Phase::Paused | Phase::Loading | Phase::Transcribing => {
                CommandAction::Cancel
            }
            Phase::Idle => CommandAction::Noop,
        },
        Cmd::Pause => match phase {
            Phase::Recording => CommandAction::Pause,
            Phase::Idle | Phase::Paused | Phase::Loading | Phase::Transcribing => {
                CommandAction::Noop
            }
        },
        Cmd::Status | Cmd::Stop => CommandAction::Noop,
    }
}

const EVENT_POLL_INTERVAL: Duration = Duration::from_secs(1);
const AUDIO_FEEDBACK_DURATION: Duration = Duration::from_secs(2);
const PROCESSING_FEEDBACK_DURATION: Duration = Duration::from_millis(2500);
const EMPTY_FEEDBACK_DURATION: Duration = Duration::from_millis(1200);

#[derive(Debug, Clone, Copy)]
struct PendingFeedback {
    session: u64,
    deadline: Instant,
}

impl PendingFeedback {
    fn new(session: u64, duration: Duration) -> Self {
        Self {
            session,
            deadline: Instant::now() + duration,
        }
    }

    fn is_expired(self, now: Instant) -> bool {
        now >= self.deadline
    }

    fn remaining(self, now: Instant) -> Duration {
        self.deadline.saturating_duration_since(now)
    }
}

struct AudioChunk {
    samples: Vec<f32>,
    rms: f32,
}

enum WorkerOutcome {
    Transcribed { session: u64, text: String },
    Failed { session: u64, msg: String },
}

impl WorkerOutcome {
    fn session(&self) -> u64 {
        match self {
            WorkerOutcome::Transcribed { session, .. } | WorkerOutcome::Failed { session, .. } => {
                *session
            }
        }
    }
}

enum DaemonEvent {
    Ipc(Cmd, Sender<Response>),
    Osd {
        session: u64,
        event: OsdEvent,
    },
    Audio {
        session: u64,
        chunk: AudioChunk,
    },
    AudioEnded {
        session: u64,
        error: Option<String>,
    },
    Worker(WorkerOutcome),
    EngineLoaded {
        session: u64,
        result: Result<Arc<Engine>, String>,
    },
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
    /// Contador de sessões: descarta eventos assíncronos de sessões antigas.
    session: u64,
    feedback: Option<PendingFeedback>,
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
        session: 0,
        feedback: None,
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
    if cfg.insert_mode.uses_wtype() && !insert::wtype_available() {
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

fn feedback_wait_duration(feedback: Option<PendingFeedback>, now: Instant) -> Duration {
    feedback
        .map(|feedback| feedback.remaining(now).min(EVENT_POLL_INTERVAL))
        .unwrap_or(EVENT_POLL_INTERVAL)
}

fn feedback_phase(kind: FeedbackKind) -> UiPhase {
    match kind {
        FeedbackKind::Info => UiPhase::Info,
        FeedbackKind::Error => UiPhase::Error,
    }
}

fn should_retry_engine_reload(event_session: u64, current_session: u64, has_engine: bool) -> bool {
    event_session != current_session && has_engine
}

impl Daemon {
    fn loop_forever(&mut self) {
        loop {
            self.reload_config_if_changed();
            self.reload_engine_if_pending();
            self.expire_feedback_if_due();
            match self.events_rx.recv_timeout(self.event_wait_duration()) {
                Ok(ev) => match ev {
                    DaemonEvent::Ipc(cmd, reply) => {
                        if matches!(cmd, Cmd::Stop) {
                            self.cancel_session();
                            let _ = reply.send(Response {
                                state: "stopping".to_string(),
                                error: None,
                                exe: None,
                                language: None,
                                model: None,
                            });
                            break;
                        }
                        let resp = self.handle_cmd(cmd);
                        let _ = reply.send(resp);
                    }
                    DaemonEvent::Osd { session, event } => self.handle_osd(session, event),
                    DaemonEvent::Audio { session, chunk } => self.handle_audio(session, chunk),
                    DaemonEvent::AudioEnded { session, error } => {
                        self.handle_audio_ended(session, error)
                    }
                    DaemonEvent::Worker(out) => self.handle_worker(out),
                    DaemonEvent::EngineLoaded { session, result } => {
                        self.handle_engine_loaded(session, result)
                    }
                },
                Err(RecvTimeoutError::Timeout) => self.expire_feedback_if_due(),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    fn event_wait_duration(&self) -> Duration {
        feedback_wait_duration(self.feedback, Instant::now())
    }

    fn expire_feedback_if_due(&mut self) {
        let now = Instant::now();
        let Some(feedback) = self.feedback else {
            return;
        };
        if !feedback.is_expired(now) {
            return;
        }
        self.feedback = None;
        if feedback.session != self.session {
            return;
        }
        self.cancel_session();
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
        spawn_engine_load(self.session, &self.cfg, &self.events_tx);
    }

    fn handle_cmd(&mut self, cmd: Cmd) -> Response {
        if self.feedback.is_some() && matches!(cmd, Cmd::Cancel) {
            self.cancel_session();
        } else {
            match command_action(self.phase, cmd) {
                CommandAction::Start => self.start_session(),
                CommandAction::Resume => self.set_phase(Phase::Recording, None),
                CommandAction::Commit => self.commit(),
                CommandAction::Cancel => self.cancel_session(),
                CommandAction::Pause => self.set_phase(Phase::Paused, None),
                CommandAction::Noop => {}
            }
        }
        let mut resp = Response {
            state: self.state_str().to_string(),
            error: None,
            exe: None,
            language: None,
            model: None,
        };
        if matches!(cmd, Cmd::Status) {
            // O CLI compara com o próprio binário: daemon de outra origem
            // (ex.: build de dev sem o wrapper do flake) é reiniciado no
            // próximo `start`.
            resp.exe = std::env::current_exe()
                .ok()
                .map(|p| p.display().to_string());
            resp.language = Some(self.cfg.language.label().to_string());
            resp.model = Some(self.cfg.model.clone());
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

    fn handle_osd(&mut self, session: u64, ev: OsdEvent) {
        if session != self.session {
            eprintln!("whisper: evento do OSD descartado (sessão antiga)");
            return;
        }
        if self.feedback.is_some() && !matches!(ev, OsdEvent::Cancel | OsdEvent::Closed) {
            return;
        }
        match ev {
            OsdEvent::PauseToggle => {
                self.clear_temporary_feedback();
                match self.phase {
                    Phase::Recording => self.set_phase(Phase::Paused, None),
                    Phase::Paused => self.set_phase(Phase::Recording, None),
                    _ => {}
                }
            }
            OsdEvent::Commit => {
                self.clear_temporary_feedback();
                self.commit();
            }
            OsdEvent::Cancel => self.cancel_session(),
            OsdEvent::Closed => {
                if let Some(text) = self.pending_insert.take() {
                    // OSD fechado: o foco voltou à app, agora digita.
                    match insert::insert(&text, self.cfg.insert_mode) {
                        Ok(insert::InsertOutcome::Fallback { reason }) => eprintln!(
                            "whisper: não foi possível inserir o texto; ele foi copiado para a área de transferência. Motivo: {reason}"
                        ),
                        Ok(_) => {}
                        Err(error) => eprintln!("whisper: inserção falhou: {error}"),
                    }
                    self.finish_session(None);
                } else {
                    self.cancel_session();
                }
            }
        }
    }

    fn handle_audio(&mut self, session: u64, chunk: AudioChunk) {
        if session == self.session && self.phase == Phase::Recording {
            self.buffer.extend_from_slice(&chunk.samples);
            self.ui.lock().unwrap().push_level(chunk.rms);
        }
    }

    fn handle_audio_ended(&mut self, session: u64, err: Option<String>) {
        if session != self.session {
            eprintln!("whisper: fim de áudio descartado (sessão antiga)");
            return;
        }
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
            self.show_feedback(FeedbackKind::Error, msg, AUDIO_FEEDBACK_DURATION);
        }
    }

    fn handle_worker(&mut self, out: WorkerOutcome) {
        if self.phase != Phase::Transcribing || out.session() != self.session {
            eprintln!("whisper: resultado descartado (sessão cancelada)");
            return;
        }
        match out {
            WorkerOutcome::Transcribed { text, .. } => {
                if text.is_empty() {
                    self.finish_session(Some("nada detectado".to_string()));
                    return;
                }
                self.pending_insert = Some(text);
                // Sem preview do texto: fecha o OSD assim que a transcrição
                // termina e digita direto na app focada (via evento Closed).
                self.close_osd();
            }
            WorkerOutcome::Failed { msg, .. } => {
                self.show_feedback(FeedbackKind::Error, msg, PROCESSING_FEEDBACK_DURATION);
            }
        }
    }

    fn handle_engine_loaded(&mut self, session: u64, res: Result<Arc<Engine>, String>) {
        if session != self.session {
            if should_retry_engine_reload(session, self.session, self.engine.is_some()) {
                // Reagenda o hot reload; a configuração antiga não deve sobrescrever a atual.
                self.pending_engine_reload = true;
            }
            eprintln!("whisper: modelo descartado (sessão antiga)");
            return;
        }
        match res {
            Ok(engine) => {
                if !engine.vad_available() {
                    eprintln!(
                        "whisper: aviso: VAD indisponível — transcrevendo sem filtro de voz; \
                               rode 'whisper setup' e reinicie o daemon"
                    );
                }
                self.engine = Some(engine);
                match self.phase {
                    Phase::Loading => {
                        self.refresh_warning();
                        self.set_phase(Phase::Recording, None);
                        self.start_capture();
                    }
                    // Troca de modelo em background (hot reload da config).
                    Phase::Idle => eprintln!("whisper: modelo recarregado"),
                    _ => {}
                }
            }
            Err(err) => {
                if self.phase == Phase::Loading {
                    self.show_feedback(
                        FeedbackKind::Error,
                        format!("modelo indisponível: {err}"),
                        PROCESSING_FEEDBACK_DURATION,
                    );
                } else if self.phase == Phase::Idle {
                    eprintln!("whisper: falha ao recarregar modelo, mantendo o atual: {err}");
                }
            }
        }
    }

    fn start_session(&mut self) {
        self.close_osd();
        self.clear_temporary_feedback();
        self.pending_insert = None;
        self.session = self.session.wrapping_add(1);
        let ui = UiState::new(lang_model(&self.cfg));
        self.ui = Arc::new(Mutex::new(ui));
        // Avisos no rodapé do cartão (wtype/VAD ausentes): o usuário vê antes
        // de ditar, mesmo que o daemon tenha subido sem console.
        self.refresh_warning();
        let (osd_tx, osd_rx) = channel();
        let (osd_ev_tx, osd_ev_rx) = channel::<OsdEvent>();
        let ui = self.ui.clone();
        let daemon_tx = self.events_tx.clone();
        let session = self.session;
        std::thread::spawn(move || {
            while let Ok(event) = osd_ev_rx.recv() {
                if daemon_tx.send(DaemonEvent::Osd { session, event }).is_err() {
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
            self.set_phase(Phase::Loading, Some("carregando modelo…".to_string()));
            spawn_engine_load(self.session, &self.cfg, &self.events_tx);
        } else {
            self.set_phase(Phase::Recording, None);
            self.start_capture();
        }
    }

    fn start_capture(&mut self) {
        let session = self.session;
        match Capture::start(self.cfg.source.as_deref()) {
            Ok((capture, stdout)) => {
                self.capture = Some(capture);
                let tx = self.events_tx.clone();
                std::thread::spawn(move || {
                    let mut stdout = stdout;
                    loop {
                        match audio::read_chunk(&mut stdout) {
                            Ok((samples, rms)) if !samples.is_empty() => {
                                if tx
                                    .send(DaemonEvent::Audio {
                                        session,
                                        chunk: AudioChunk { samples, rms },
                                    })
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Ok(_) => {
                                let _ = tx.send(DaemonEvent::AudioEnded {
                                    session,
                                    error: None,
                                });
                                break;
                            }
                            Err(e) => {
                                let _ = tx.send(DaemonEvent::AudioEnded {
                                    session,
                                    error: Some(e.to_string()),
                                });
                                break;
                            }
                        }
                    }
                });
            }
            Err(e) => {
                self.show_feedback(
                    FeedbackKind::Error,
                    format!("áudio: {e:#}"),
                    AUDIO_FEEDBACK_DURATION,
                );
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
                self.show_feedback(
                    FeedbackKind::Error,
                    "modelo não carregado: rode 'whisper setup'".to_string(),
                    PROCESSING_FEEDBACK_DURATION,
                );
                return;
            }
        };
        self.set_phase(Phase::Transcribing, Some("Transcrevendo".to_string()));
        let samples = std::mem::take(&mut self.buffer);
        let cfg = self.cfg.clone();
        let session = self.session;
        let tx = self.events_tx.clone();
        std::thread::spawn(move || {
            let out = transcribe_worker(&engine, samples, &cfg, session);
            let _ = tx.send(DaemonEvent::Worker(out));
        });
    }

    fn stop_capture(&mut self) {
        if let Some(mut capture) = self.capture.take() {
            capture.stop();
        }
    }

    fn set_phase(&mut self, phase: Phase, status: Option<String>) {
        self.phase = phase;
        if let Some(ui_phase) = phase.ui_phase() {
            self.set_ui(ui_phase, status);
        }
    }

    fn set_ui(&mut self, phase: UiPhase, status: Option<String>) {
        if let Ok(mut ui) = self.ui.lock() {
            ui.phase = phase;
            ui.status = status;
        }
    }

    fn set_ui_feedback(&mut self, kind: FeedbackKind, message: String) {
        if let Ok(mut ui) = self.ui.lock() {
            ui.feedback = Some(Feedback { kind, message });
            ui.status = None;
        }
    }

    fn clear_ui_feedback(&mut self) {
        if let Ok(mut ui) = self.ui.lock() {
            ui.feedback = None;
        }
    }

    fn clear_temporary_feedback(&mut self) {
        self.feedback = None;
        self.clear_ui_feedback();
    }

    /// Atualiza o aviso do rodapé do OSD conforme as dependências disponíveis.
    fn refresh_warning(&mut self) {
        let wtype_missing = self.cfg.insert_mode.uses_wtype() && !insert::wtype_available();
        let vad_missing = self
            .engine
            .as_ref()
            .map(|e| !e.vad_available())
            .unwrap_or(false);
        if let Ok(mut ui) = self.ui.lock() {
            ui.warning = compose_warning(wtype_missing, vad_missing);
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
        self.pending_insert = None;
        self.clear_temporary_feedback();
        self.phase = Phase::Idle;
    }

    fn show_feedback(&mut self, kind: FeedbackKind, msg: String, duration: Duration) {
        self.stop_capture();
        self.pending_insert = None;
        self.phase = Phase::Idle;
        self.set_ui(feedback_phase(kind), None);
        self.set_ui_feedback(kind, msg);
        self.feedback = Some(PendingFeedback::new(self.session, duration));
    }

    fn finish_session(&mut self, msg: Option<String>) {
        if let Some(msg) = msg {
            self.show_feedback(FeedbackKind::Info, msg, EMPTY_FEEDBACK_DURATION);
        } else {
            self.cancel_session();
        }
    }
}

fn transcribe_worker(
    engine: &Engine,
    samples: Vec<f32>,
    cfg: &Config,
    session: u64,
) -> WorkerOutcome {
    let samples = match engine.filter_speech(samples) {
        Ok(s) => s,
        Err(e) => {
            return WorkerOutcome::Failed {
                session,
                msg: format!("detecção de voz falhou: {e:#}"),
            };
        }
    };
    if samples.is_empty() {
        return WorkerOutcome::Transcribed {
            session,
            text: String::new(),
        };
    }
    let raw = match engine.transcribe(&samples, cfg.language.whisper_code()) {
        Ok(text) => text,
        Err(e) => {
            return WorkerOutcome::Failed {
                session,
                msg: format!("transcrição falhou: {e:#}"),
            };
        }
    };
    let text = rust_cleanup(&raw, cfg);
    // A inserção (wtype) fica com o daemon, DEPOIS que o OSD fechar: com o
    // OSD visível o foco de teclado é dele e as teclas não chegariam à app.
    WorkerOutcome::Transcribed { session, text }
}

fn rust_cleanup(text: &str, cfg: &Config) -> String {
    let mut text = text.to_string();
    if cfg.remove_fillers {
        text = postprocess::remove_fillers(&text, cfg.language);
    }
    if cfg.punctuation {
        text = postprocess::fix_punctuation(&text, cfg.final_period);
    }
    text
}

/// Sobe o engine em background (primeira sessão ou hot reload da config).
fn spawn_engine_load(session: u64, cfg: &Config, tx: &Sender<DaemonEvent>) {
    let cfg = cfg.clone();
    let tx = tx.clone();
    std::thread::spawn(move || {
        let res = Engine::load(
            &cfg.model_path(),
            &model::vad_model_path(),
            cfg.gpu_device,
            cfg.threads,
        )
        .map(Arc::new)
        .map_err(|e| format!("{e:#}"));
        let _ = tx.send(DaemonEvent::EngineLoaded {
            session,
            result: res,
        });
    });
}

/// Compõe o aviso único do rodapé do OSD a partir das indisponibilidades de
/// wtype e VAD; `None` quando nada falta.
fn compose_warning(wtype_missing: bool, vad_missing: bool) -> Option<String> {
    match (wtype_missing, vad_missing) {
        (false, false) => None,
        (true, false) => {
            Some("wtype ausente — a digitação na app não vai funcionar (só clipboard)".to_string())
        }
        (false, true) => {
            Some("VAD ausente — transcrevendo sem filtro de voz; rode whisper setup".to_string())
        }
        (true, true) => {
            Some("wtype ausente (só clipboard) · VAD ausente (sem filtro de voz)".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_deadline_is_not_expired_before_duration() {
        let now = Instant::now();
        let feedback = PendingFeedback {
            session: 1,
            deadline: now + Duration::from_secs(1),
        };

        assert!(!feedback.is_expired(now));
    }

    #[test]
    fn feedback_deadline_expires_after_duration() {
        let now = Instant::now();
        let feedback = PendingFeedback {
            session: 1,
            deadline: now + Duration::from_secs(1),
        };

        assert!(feedback.is_expired(now + Duration::from_secs(1)));
    }

    #[test]
    fn cancelled_feedback_returns_to_regular_event_polling() {
        assert_eq!(
            feedback_wait_duration(None, Instant::now()),
            EVENT_POLL_INTERVAL
        );
    }

    #[test]
    fn feedback_kinds_map_to_distinct_ui_phases() {
        assert_eq!(feedback_phase(FeedbackKind::Info), UiPhase::Info);
        assert_eq!(feedback_phase(FeedbackKind::Error), UiPhase::Error);
    }

    fn test_daemon(session: u64, phase: Phase) -> Daemon {
        let (events_tx, events_rx) = channel();
        Daemon {
            cfg: Config::default(),
            cfg_mtime: None,
            pending_engine_reload: false,
            phase,
            engine: None,
            buffer: Vec::new(),
            capture: None,
            ui: Arc::new(Mutex::new(UiState::new("pt · turbo".to_string()))),
            osd: None,
            pending_insert: None,
            session,
            feedback: None,
            events_rx,
            events_tx,
        }
    }

    #[test]
    fn status_response_includes_current_session_metadata() {
        let mut daemon = test_daemon(1, Phase::Recording);
        daemon.cfg.language = crate::config::Language::En;
        daemon.cfg.model = "small".to_string();

        let response = daemon.handle_cmd(Cmd::Status);

        assert_eq!(response.language.as_deref(), Some("en"));
        assert_eq!(response.model.as_deref(), Some("small"));
    }

    #[test]
    fn non_status_response_does_not_include_session_metadata() {
        let mut daemon = test_daemon(1, Phase::Recording);

        let response = daemon.handle_cmd(Cmd::Toggle);

        assert_eq!(response.language, None);
        assert_eq!(response.model, None);
    }

    #[test]
    fn stale_hot_reload_is_retried_when_engine_exists() {
        assert!(should_retry_engine_reload(1, 2, true));
    }

    #[test]
    fn stale_initial_engine_load_is_not_retried_without_engine() {
        assert!(!should_retry_engine_reload(1, 2, false));
    }

    #[test]
    fn stale_audio_chunk_does_not_enter_current_session() {
        let mut daemon = test_daemon(2, Phase::Recording);
        daemon.handle_audio(
            1,
            AudioChunk {
                samples: vec![1.0, 2.0],
                rms: 0.5,
            },
        );

        assert!(daemon.buffer.is_empty());
    }

    #[test]
    fn stale_osd_close_does_not_cancel_current_session() {
        let mut daemon = test_daemon(2, Phase::Transcribing);
        daemon.pending_insert = Some("texto atual".to_string());
        daemon.handle_osd(1, OsdEvent::Closed);

        assert_eq!(daemon.pending_insert.as_deref(), Some("texto atual"));
    }

    #[test]
    fn cancel_clears_feedback_before_deadline() {
        let mut daemon = test_daemon(1, Phase::Idle);
        daemon.feedback = Some(PendingFeedback {
            session: 1,
            deadline: Instant::now() + Duration::from_secs(1),
        });

        daemon.handle_cmd(Cmd::Cancel);

        assert!(daemon.feedback.is_none());
    }

    #[test]
    fn expired_feedback_returns_to_idle() {
        let mut daemon = test_daemon(1, Phase::Recording);
        daemon.feedback = Some(PendingFeedback {
            session: 1,
            deadline: Instant::now() - Duration::from_secs(1),
        });

        daemon.expire_feedback_if_due();

        assert_eq!(daemon.phase, Phase::Idle);
    }

    #[test]
    fn terminal_feedback_keeps_ignoring_non_cancel_osd_controls() {
        let mut daemon = test_daemon(1, Phase::Idle);
        daemon.show_feedback(
            FeedbackKind::Info,
            "nada detectado".to_string(),
            Duration::from_secs(1),
        );

        daemon.handle_osd(1, OsdEvent::PauseToggle);
        daemon.handle_osd(1, OsdEvent::Commit);

        assert!(daemon.feedback.is_some());
    }

    #[test]
    fn empty_result_uses_informational_feedback() {
        let mut daemon = test_daemon(1, Phase::Transcribing);

        daemon.finish_session(Some("nada detectado".to_string()));

        assert_eq!(daemon.phase, Phase::Idle);
        let ui = daemon.ui.lock().unwrap();
        assert_eq!(ui.phase, UiPhase::Info);
        assert_eq!(ui.feedback.as_ref().unwrap().kind, FeedbackKind::Info);
    }

    #[test]
    fn real_failure_uses_error_feedback() {
        let mut daemon = test_daemon(1, Phase::Recording);

        daemon.show_feedback(
            FeedbackKind::Error,
            "transcrição falhou".to_string(),
            Duration::from_secs(1),
        );

        assert_eq!(daemon.ui.lock().unwrap().phase, UiPhase::Error);
    }

    #[test]
    fn idle_has_no_ui_phase() {
        assert_eq!(Phase::Idle.ui_phase(), None);
    }

    #[test]
    fn active_phases_map_to_their_ui_phase() {
        let cases = [
            (Phase::Recording, UiPhase::Recording),
            (Phase::Paused, UiPhase::Paused),
            (Phase::Transcribing, UiPhase::Transcribing),
            (Phase::Loading, UiPhase::Loading),
        ];

        for (phase, expected) in cases {
            assert_eq!(phase.ui_phase(), Some(expected));
        }
    }

    #[test]
    fn toggle_follows_the_global_shortcut_session_flow() {
        let cases = [
            (Phase::Idle, CommandAction::Start),
            (Phase::Recording, CommandAction::Commit),
            (Phase::Paused, CommandAction::Commit),
            (Phase::Loading, CommandAction::Noop),
            (Phase::Transcribing, CommandAction::Noop),
        ];

        for (phase, expected) in cases {
            assert_eq!(command_action(phase, Cmd::Toggle), expected);
        }
    }

    #[test]
    fn explicit_session_commands_only_allow_valid_transitions() {
        let cases = [
            (Phase::Idle, Cmd::Record, CommandAction::Start),
            (Phase::Paused, Cmd::Record, CommandAction::Resume),
            (Phase::Recording, Cmd::Record, CommandAction::Noop),
            (Phase::Loading, Cmd::Record, CommandAction::Noop),
            (Phase::Transcribing, Cmd::Record, CommandAction::Noop),
            (Phase::Recording, Cmd::Commit, CommandAction::Commit),
            (Phase::Paused, Cmd::Commit, CommandAction::Commit),
            (Phase::Idle, Cmd::Commit, CommandAction::Noop),
            (Phase::Loading, Cmd::Commit, CommandAction::Noop),
            (Phase::Transcribing, Cmd::Commit, CommandAction::Noop),
            (Phase::Idle, Cmd::Cancel, CommandAction::Noop),
            (Phase::Recording, Cmd::Cancel, CommandAction::Cancel),
            (Phase::Paused, Cmd::Cancel, CommandAction::Cancel),
            (Phase::Loading, Cmd::Cancel, CommandAction::Cancel),
            (Phase::Transcribing, Cmd::Cancel, CommandAction::Cancel),
            (Phase::Recording, Cmd::Pause, CommandAction::Pause),
            (Phase::Idle, Cmd::Pause, CommandAction::Noop),
            (Phase::Paused, Cmd::Pause, CommandAction::Noop),
            (Phase::Loading, Cmd::Pause, CommandAction::Noop),
            (Phase::Transcribing, Cmd::Pause, CommandAction::Noop),
        ];

        for (phase, cmd, expected) in cases {
            assert_eq!(command_action(phase, cmd), expected);
        }
    }

    #[test]
    fn warning_composes_all_dependency_combinations() {
        assert_eq!(compose_warning(false, false), None);
        assert!(
            compose_warning(true, false)
                .unwrap()
                .contains("wtype ausente")
        );
        assert!(
            compose_warning(false, true)
                .unwrap()
                .contains("VAD ausente")
        );
        let both = compose_warning(true, true).unwrap();
        assert!(both.contains("wtype ausente") && both.contains("VAD ausente"));
    }

    #[test]
    fn rust_cleanup_respects_flags() {
        let cfg = Config::default();
        assert_eq!(rust_cleanup("hmm então ahn vamos", &cfg), "Então vamos.");
        let mut disabled = cfg.clone();
        disabled.remove_fillers = false;
        disabled.punctuation = false;
        assert_eq!(
            rust_cleanup("hmm então ahn vamos", &disabled),
            "hmm então ahn vamos"
        );
    }
}
