//! Daemon: orquestra as sessões de ditado — estado, áudio (pw-record),
//! OSD (teclas), transcrição (whisper-rs) e inserção (wtype/wl-copy).
use crate::audio::{self, Capture};
use crate::config::{Config, InsertMode, config_mtime};
use crate::insert;
use crate::ipc::{self, Cmd, Response};
use crate::llm;
use crate::model;
use crate::osd::{OsdCommand, OsdEvent, Phase as UiPhase, UiState};
use crate::postprocess;
use crate::transcribe::Engine;
use anyhow::Result;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
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
    Osd(OsdEvent),
    Audio(AudioChunk),
    AudioEnded(Option<String>),
    Worker(WorkerOutcome),
    WorkerProgress {
        session: u64,
        phase: UiPhase,
        status: String,
    },
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
    llm: Arc<llm::Llm>,
    smart_mode: bool,
    /// Contador de sessões: descarta resultados de workers de sessões antigas.
    session: u64,
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
        llm: Arc::new(llm::Llm::default()),
        smart_mode: false,
        session: 0,
        events_rx,
        events_tx,
    };
    warn_if_wtype_missing(&daemon.cfg);
    warn_if_ai_missing(&daemon.cfg);
    daemon.loop_forever();
    let _ = std::fs::remove_file(crate::config::socket_path());
    Ok(())
}

/// Avisa (no stderr → daemon.log) se o wtype faltar no PATH quando o modo de
/// inserção depende dele: a digitação na app vai falhar e sobra só o clipboard.
fn warn_if_wtype_missing(cfg: &Config) {
    if matches!(cfg.insert_mode, InsertMode::Type | InsertMode::Both) && !insert::wtype_available()
    {
        eprintln!(
            "whisper: aviso: wtype não encontrado no PATH — a digitação na app focada vai \
             falhar (o texto irá só para o clipboard). {}",
            insert::wtype_hint()
        );
    }
}

fn warn_if_ai_missing(cfg: &Config) {
    if cfg.ai.enabled && (cfg.ai_model_path().is_none() || !llm::Llm::server_available()) {
        eprintln!(
            "whisper: aviso: Qwen indisponível — cleanup em Rust. {}",
            llm::Llm::hint()
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
                            self.cancel_session();
                            self.kill_llm();
                            let _ = reply.send(Response {
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
                    DaemonEvent::WorkerProgress {
                        session,
                        phase,
                        status,
                    } => self.handle_worker_progress(session, phase, status),
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
                let ai_changed = cfg.ai != self.cfg.ai;
                self.cfg = cfg;
                if engine_changed {
                    self.pending_engine_reload = true;
                }
                if ai_changed {
                    self.kill_llm();
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
        spawn_engine_load(&self.cfg, &self.events_tx);
    }

    fn handle_cmd(&mut self, cmd: Cmd) -> Response {
        match command_action(self.phase, cmd) {
            CommandAction::Start => self.start_session(),
            CommandAction::Resume => self.set_phase(Phase::Recording, None),
            CommandAction::Commit => self.commit(),
            CommandAction::Cancel => self.cancel_session(),
            CommandAction::Pause => self.set_phase(Phase::Paused, None),
            CommandAction::Noop => {}
        }
        let mut resp = Response {
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
                Phase::Recording => self.set_phase(Phase::Paused, None),
                Phase::Paused => self.set_phase(Phase::Recording, None),
                _ => {}
            },
            OsdEvent::Commit => self.commit(),
            OsdEvent::Cancel => self.cancel_session(),
            OsdEvent::SmartToggle => {
                if self.cfg.ai.enabled {
                    self.smart_mode = !self.smart_mode;
                    if let Ok(mut ui) = self.ui.lock() {
                        ui.smart = self.smart_mode;
                    }
                } else {
                    eprintln!("whisper: modo smart ignorado: [ai] enabled = false");
                }
            }
            OsdEvent::Closed => {
                if let Some(text) = self.pending_insert.take() {
                    // OSD fechado: o foco voltou à app, agora digita.
                    if let Err(e) = insert::insert(&text, self.cfg.insert_mode) {
                        // O insert detalha a falha e avisa quando o texto
                        // sobrou no clipboard (modo both); a sessão encerra
                        // do mesmo jeito.
                        eprintln!("whisper: inserção falhou: {e}");
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

    fn handle_worker_progress(&mut self, session: u64, phase: UiPhase, status: String) {
        if self.phase == Phase::Transcribing && session == self.session {
            self.set_ui(phase, Some(status));
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
                self.set_ui(UiPhase::Error, Some(msg));
                std::thread::sleep(Duration::from_millis(2500));
                self.finish_session(None);
            }
        }
    }

    fn handle_engine_loaded(&mut self, res: Result<Arc<Engine>, String>) {
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
        self.session = self.session.wrapping_add(1);
        self.smart_mode = false;
        let ui = UiState::new(lang_model(&self.cfg));
        self.ui = Arc::new(Mutex::new(ui));
        // Avisos no rodapé do cartão (wtype/VAD ausentes): o usuário vê antes
        // de ditar, mesmo que o daemon tenha subido sem console.
        self.refresh_warning();
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
            self.set_phase(Phase::Loading, Some("carregando modelo…".to_string()));
            spawn_engine_load(&self.cfg, &self.events_tx);
        } else {
            self.set_phase(Phase::Recording, None);
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
                                if tx
                                    .send(DaemonEvent::Audio(AudioChunk { samples, rms }))
                                    .is_err()
                                {
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
                self.finish_session(Some(
                    "modelo não carregado: rode 'whisper setup'".to_string(),
                ));
                return;
            }
        };
        self.set_phase(Phase::Transcribing, Some("Transcrevendo".to_string()));
        let samples = std::mem::take(&mut self.buffer);
        let cfg = self.cfg.clone();
        let smart = self.smart_mode;
        let session = self.session;
        let llm = Arc::clone(&self.llm);
        let tx = self.events_tx.clone();
        std::thread::spawn(move || {
            let out = transcribe_worker(&engine, samples, &cfg, smart, session, llm, tx.clone());
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

    /// Atualiza o aviso do rodapé do OSD conforme as dependências disponíveis.
    fn refresh_warning(&mut self) {
        let wtype_missing = matches!(self.cfg.insert_mode, InsertMode::Type | InsertMode::Both)
            && !insert::wtype_available();
        let vad_missing = self
            .engine
            .as_ref()
            .map(|e| !e.vad_available())
            .unwrap_or(false);
        let ai_missing = self.cfg.ai.enabled
            && (self.cfg.ai_model_path().is_none() || !llm::Llm::server_available());
        if let Ok(mut ui) = self.ui.lock() {
            ui.warning = compose_warning(wtype_missing, vad_missing, ai_missing);
        }
    }

    fn kill_llm(&mut self) {
        // `kill` nunca espera request em andamento (a trava do child do
        // servidor é curta), então a chamada é síncrona: sem thread
        // destacada que poderia morrer junto com o daemon (servidor órfão)
        // ou matar um servidor recém-subido com config nova.
        self.llm.kill();
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

fn transcribe_worker(
    engine: &Engine,
    samples: Vec<f32>,
    cfg: &Config,
    smart: bool,
    session: u64,
    llm: Arc<llm::Llm>,
    progress_tx: Sender<DaemonEvent>,
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
    let fallback = rust_cleanup(&raw, cfg);
    let text = if use_ai(cfg, smart) {
        let _ = progress_tx.send(DaemonEvent::WorkerProgress {
            session,
            phase: UiPhase::Cleaning,
            status: "Transcrevendo".to_string(),
        });
        match llm_process(cfg, smart, &raw, llm) {
            Ok(text) if plausible_no_think_ok(&raw, &text, smart) => text,
            Ok(_) => {
                eprintln!("whisper: resposta do Qwen rejeitada — usando fallback");
                fallback
            }
            Err(e) => {
                eprintln!("whisper: qwen falhou ({e:#}) — usando fallback");
                fallback
            }
        }
    } else {
        fallback
    };
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

fn use_ai(cfg: &Config, smart: bool) -> bool {
    cfg.ai.enabled && (smart || cfg.ai.cleanup) && cfg.ai_model_path().is_some()
}

fn llm_process(cfg: &Config, smart: bool, raw: &str, llm: Arc<llm::Llm>) -> anyhow::Result<String> {
    llm.process(&cfg.ai, cfg.threads, raw, smart)
}

fn plausible_no_think_ok(raw: &str, out: &str, smart: bool) -> bool {
    if smart {
        llm::basic_ok(out, raw)
    } else {
        llm::plausible(raw, out)
    }
}

/// Sobe o engine em background (primeira sessão ou hot reload da config).
fn spawn_engine_load(cfg: &Config, tx: &Sender<DaemonEvent>) {
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
        let _ = tx.send(DaemonEvent::EngineLoaded(res));
    });
}

/// Compõe o aviso único do rodapé do OSD a partir das indisponibilidades de
/// wtype, VAD e Qwen; `None` quando nada falta.
fn compose_warning(wtype_missing: bool, vad_missing: bool, ai_missing: bool) -> Option<String> {
    match (wtype_missing, vad_missing, ai_missing) {
        (false, false, false) => None,
        (false, false, true) => Some("Qwen ausente — cleanup Rust; rode whisper setup".to_string()),
        (true, false, false) => {
            Some("wtype ausente — a digitação na app não vai funcionar (só clipboard)".to_string())
        }
        (false, true, false) => {
            Some("VAD ausente — transcrevendo sem filtro de voz; rode whisper setup".to_string())
        }
        (true, true, false) => {
            Some("wtype ausente (só clipboard) · VAD ausente (sem filtro de voz)".to_string())
        }
        (true, false, true) => {
            Some("wtype e Qwen ausentes — digitação e cleanup limitados".to_string())
        }
        (false, true, true) => {
            Some("VAD e Qwen ausentes — cleanup limitado; rode whisper setup".to_string())
        }
        (true, true, true) => {
            Some("wtype, VAD e Qwen ausentes — digitação e cleanup limitados".to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(compose_warning(false, false, false), None);
        assert_eq!(
            compose_warning(false, false, true).as_deref(),
            Some("Qwen ausente — cleanup Rust; rode whisper setup")
        );
        assert_eq!(
            compose_warning(true, false, true).as_deref(),
            Some("wtype e Qwen ausentes — digitação e cleanup limitados")
        );
        assert_eq!(
            compose_warning(false, true, true).as_deref(),
            Some("VAD e Qwen ausentes — cleanup limitado; rode whisper setup")
        );
        assert_eq!(
            compose_warning(true, true, true).as_deref(),
            Some("wtype, VAD e Qwen ausentes — digitação e cleanup limitados")
        );
        assert!(
            compose_warning(true, false, false)
                .unwrap()
                .contains("wtype ausente")
        );
        assert!(
            compose_warning(false, true, false)
                .unwrap()
                .contains("VAD ausente")
        );
        let both = compose_warning(true, true, false).unwrap();
        assert!(both.contains("wtype ausente") && both.contains("VAD ausente"));
    }

    #[test]
    fn use_ai_obeys_enabled_cleanup_smart_and_model_presence() {
        let path = std::env::temp_dir().join(format!(
            "whisper-ai-use-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"modelo").unwrap();
        let mut cfg = Config::default();
        cfg.ai.model = path.display().to_string();
        cfg.ai.cleanup = false;
        assert!(!use_ai(&cfg, false));
        assert!(use_ai(&cfg, true));
        cfg.ai.enabled = false;
        assert!(!use_ai(&cfg, true));
        cfg.ai.enabled = true;
        cfg.ai.cleanup = true;
        assert!(use_ai(&cfg, false));
        std::fs::remove_file(&path).unwrap();
        assert!(!use_ai(&cfg, true));
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
