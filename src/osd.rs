//! OSD: overlay wlr-layer-shell (Niri, Sway, Hyprland, KDE ≥ 6.3, COSMIC)
//! com fallback para janela xdg-toplevel (GNOME). Desenho por software com
//! tiny-skia + fonte embutida (DejaVu Sans); sem X11.
//!
//! A superfície layer-shell fica na borda inferior (largura total, altura
//! fixa) e o "cartão" do ditado é desenhado centralizado — assim a âncora
//! precisa só de TOP/BOTTOM/LEFT/RIGHT, que todo compositor tem.
//!
//! Com keyboard interactivity `Exclusive` (ou foco de janela no fallback),
//! o próprio app lê as teclas: Space pausa/retoma, Enter conclui, Esc cancela.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{
    Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_registry,
    delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
};
use tiny_skia::{PixmapMut, Transform};
use wayland_client::backend::WaylandError;
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface};
use wayland_client::{Connection, QueueHandle};

use crate::osd_draw::{CARD_H, CARD_W, draw_card, font};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Recording,
    Paused,
    Transcribing,
    Loading,
    Cleaning,
    Error,
}

/// Estado compartilhado da UI: o daemon escreve, o OSD lê a cada frame.
pub struct UiState {
    pub phase: Phase,
    pub status: Option<String>,
    /// Aviso persistente exibido no rodapé do cartão (ex.: wtype ausente).
    pub warning: Option<String>,
    /// Indica que o próximo ditado pode conter uma instrução de transformação.
    pub smart: bool,
    pub levels: VecDeque<f32>,
    pub lang_model: String,
}

impl UiState {
    pub fn new(lang_model: String) -> UiState {
        UiState {
            phase: Phase::Loading,
            status: None,
            warning: None,
            smart: false,
            levels: VecDeque::new(),
            lang_model,
        }
    }

    pub fn push_level(&mut self, rms: f32) {
        self.levels.push_back(rms);
        if self.levels.len() > 48 {
            self.levels.pop_front();
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum OsdEvent {
    PauseToggle,
    Commit,
    Cancel,
    SmartToggle,
    Closed,
}

#[derive(Debug, Clone, Copy)]
pub enum OsdCommand {
    Close,
}

struct App {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    kind: SurfaceKind,
    pool: SlotPool,
    scale: u32,
    /// Largura lógica confirmada pelo compositor para centralizar a cápsula.
    surface_width: u32,
    configured: bool,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    modifiers: Modifiers,
    ui: Arc<Mutex<UiState>>,
    events_tx: Sender<OsdEvent>,
    commands_rx: Receiver<OsdCommand>,
    animation_start: Instant,
}

enum SurfaceKind {
    Layer(LayerSurface),
    Window(Window),
}

impl SurfaceKind {
    fn surface(&self) -> &wl_surface::WlSurface {
        match self {
            SurfaceKind::Layer(l) => l.wl_surface(),
            SurfaceKind::Window(w) => w.wl_surface(),
        }
    }

    fn commit(&self) {
        match self {
            SurfaceKind::Layer(l) => l.commit(),
            SurfaceKind::Window(w) => w.commit(),
        }
    }
}

/// Roda o OSD até receber `Close` ou a janela ser fechada pelo compositor.
pub fn run(
    ui: Arc<Mutex<UiState>>,
    events_tx: Sender<OsdEvent>,
    commands_rx: Receiver<OsdCommand>,
) -> Result<()> {
    let conn = Connection::connect_to_env().context("conectando ao Wayland")?;
    let (globals, mut event_queue) = registry_queue_init(&conn).context("globais Wayland")?;
    let qh = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor ausente")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm ausente")?;
    let surface = compositor.create_surface(&qh);

    let kind = match LayerShell::bind(&globals, &qh) {
        Ok(layer_shell) => {
            let layer = layer_shell.create_layer_surface(
                &qh,
                surface,
                Layer::Overlay,
                Some("whisper"),
                None,
            );
            layer.set_anchor(Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
            layer.set_keyboard_interactivity(KeyboardInteractivity::Exclusive);
            layer.set_size(0, CARD_H as u32);
            layer.set_margin(0, 0, 16, 0);
            layer.commit();
            SurfaceKind::Layer(layer)
        }
        Err(_) => {
            // Fallback GNOME (mutter não implementa wlr-layer-shell).
            let xdg = XdgShell::bind(&globals, &qh).context("sem layer-shell nem xdg-shell")?;
            let window = xdg.create_window(surface, WindowDecorations::RequestServer, &qh);
            window.set_title("whisper");
            window.set_app_id("dev.local.whisper");
            window.set_min_size(Some((CARD_W as u32, CARD_H as u32)));
            window.commit();
            SurfaceKind::Window(window)
        }
    };

    // Reserva margem para telas 4K/8K e escalas HiDPI sem deixar o OSD
    // invisível quando houver mais de um buffer em voo.
    let pool = SlotPool::new(8 * 8192 * (CARD_H as usize) * 4, &shm).context("pool shm")?;
    let surface_width = match &kind {
        SurfaceKind::Layer(_) => 1920,
        SurfaceKind::Window(_) => CARD_W as u32,
    };

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        kind,
        pool,
        scale: 1,
        surface_width,
        configured: false,
        keyboard: None,
        modifiers: Modifiers::default(),
        ui,
        events_tx,
        commands_rx,
        animation_start: Instant::now(),
    };

    loop {
        // Lê eventos do socket (não bloqueante) para que configure, teclas e
        // frame callbacks cheguem; `dispatch_pending` só despacha o que já
        // foi lido da fila.
        if let Some(guard) = conn.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => {
                    eprintln!("whisper: leitura wayland: {e}");
                    break;
                }
            }
        }
        let _ = event_queue.dispatch_pending(&mut app);
        if matches!(app.commands_rx.try_recv(), Ok(OsdCommand::Close)) {
            break;
        }
        if app.configured {
            app.draw(&qh);
        }
        let _ = conn.flush();
        std::thread::sleep(Duration::from_millis(8));
    }
    // Flush final: garante que os destroys das superfícies (drop do `app`)
    // cheguem ao compositor antes de o daemon digitar na app focada.
    let _ = conn.flush();
    Ok(())
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        self.scale = new_factor.max(1) as u32;
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && self.keyboard.is_none()
            && let Ok(keyboard) = self.seat_state.get_keyboard(qh, &seat, None)
        {
            self.keyboard = Some(keyboard);
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        let ev = if event.keysym == Keysym::space {
            Some(OsdEvent::PauseToggle)
        } else if matches!(event.keysym, Keysym::Return | Keysym::KP_Enter) {
            Some(OsdEvent::Commit)
        } else if event.keysym == Keysym::Escape {
            Some(OsdEvent::Cancel)
        } else if event.keysym == Keysym::s
            && !self.modifiers.ctrl
            && !self.modifiers.alt
            && !self.modifiers.logo
        {
            Some(OsdEvent::SmartToggle)
        } else {
            None
        };
        if let Some(ev) = ev {
            let _ = self.events_tx.send(ev);
        }
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _event: KeyEvent,
    ) {
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _event: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: smithay_client_toolkit::seat::keyboard::RawModifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }
}

impl LayerShellHandler for App {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        let _ = self.events_tx.send(OsdEvent::Closed);
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if configure.new_size.0 > 0 {
            // O wlr-layer-shell entrega a largura em pixels do buffer; o
            // desenho trabalha em coordenadas lógicas antes do scale.
            self.surface_width = configure.new_size.0.div_ceil(self.scale.max(1));
        }
        if !self.configured {
            self.configured = true;
        }
        self.draw(qh);
    }
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        let _ = self.events_tx.send(OsdEvent::Closed);
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        if let Some(width) = configure.new_size.0 {
            self.surface_width = width.get();
        }
        if !self.configured {
            self.configured = true;
        }
        self.draw(qh);
    }
}

fn swap_red_blue(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
}

impl App {
    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        let s = self.scale as i32;
        let w = self.surface_width as i32 * s;
        let h = CARD_H as i32 * s;
        let stride = w * 4;
        let Ok((buffer, canvas)) = self
            .pool
            .create_buffer(w, h, stride, wl_shm::Format::Argb8888)
        else {
            eprintln!("whisper: pool SHM sem espaço para o frame do OSD ({w}x{h})");
            return;
        };
        let ui = match self.ui.lock() {
            Ok(ui) => ui,
            Err(_) => return,
        };
        // tiny-skia desenha bytes RGBA, enquanto ARGB8888 no Wayland little-endian
        // usa BGRA em memória. Converte antes e depois para preservar os canais.
        swap_red_blue(canvas);
        {
            let Some(mut pix) = PixmapMut::from_bytes(canvas, w as u32, h as u32) else {
                swap_red_blue(canvas);
                return;
            };
            draw_card(
                &mut pix,
                Transform::from_scale(s as f32, s as f32),
                &ui,
                font(),
                self.animation_start.elapsed().as_secs_f32(),
            );
        }
        swap_red_blue(canvas);
        drop(ui);
        self.kind.surface().damage_buffer(0, 0, w, h);
        if buffer.attach_to(self.kind.surface()).is_ok() {
            self.kind.commit();
        }
    }
}

delegate_compositor!(App);
delegate_output!(App);
delegate_shm!(App);
delegate_seat!(App);
delegate_keyboard!(App);
delegate_layer!(App);
delegate_xdg_shell!(App);
delegate_xdg_window!(App);
delegate_registry!(App);

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    smithay_client_toolkit::registry_handlers![OutputState, SeatState];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swap_red_blue_converts_rgba_to_wayland_argb8888_bytes() {
        let rgba = [52, 152, 219, 255];
        let mut pixels = rgba;

        swap_red_blue(&mut pixels);
        assert_eq!(pixels, [219, 152, 52, 255]);

        swap_red_blue(&mut pixels);
        assert_eq!(pixels, rgba);
    }
}
