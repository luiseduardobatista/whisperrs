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
use std::time::Duration;

use ab_glyph::{Font as _, FontArc, PxScale, ScaleFont as _};
use anyhow::{Context, Result};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, KeyboardHandler, Keysym};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::xdg::window::{
    Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_registry,
    delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Transform};
use wayland_client::globals::registry_queue_init;
use wayland_client::protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface};
use wayland_client::backend::WaylandError;
use wayland_client::{Connection, QueueHandle};

/// Dimensões lógicas do cartão (escaladas pela saída na renderização).
pub const CARD_W: f32 = 460.0;
pub const CARD_H: f32 = 180.0;

// Fonte embutida via include_bytes (DejaVu Sans, licença livre).
static FONT: std::sync::OnceLock<FontArc> = std::sync::OnceLock::new();

fn font() -> &'static FontArc {
    FONT.get_or_init(|| {
        FontArc::try_from_slice(include_bytes!("../assets/DejaVuSans.ttf"))
            .expect("fonte embutida inválida")
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Recording,
    Paused,
    Transcribing,
    Loading,
    Error,
}

/// Estado compartilhado da UI: o daemon escreve, o OSD lê a cada frame.
pub struct UiState {
    pub phase: Phase,
    pub status: Option<String>,
    /// Aviso persistente exibido no rodapé do cartão (ex.: wtype ausente).
    pub warning: Option<String>,
    pub levels: VecDeque<f32>,
    pub lang_model: String,
}

impl UiState {
    pub fn new(lang_model: String) -> UiState {
        UiState {
            phase: Phase::Loading,
            status: None,
            warning: None,
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
    configured: bool,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    ui: Arc<Mutex<UiState>>,
    events_tx: Sender<OsdEvent>,
    commands_rx: Receiver<OsdCommand>,
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
            layer.set_margin(0, 0, 24, 0);
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

    let pool = SlotPool::new(8 * 1920 * (CARD_H as usize) * 4, &shm).context("pool shm")?;

    let mut app = App {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        kind,
        pool,
        scale: 1,
        configured: false,
        keyboard: None,
        ui,
        events_tx,
        commands_rx,
    };

    loop {
        // Lê eventos do socket (não bloqueante) para que configure, teclas e
        // frame callbacks cheguem; `dispatch_pending` só despacha o que já
        // foi lido da fila.
        if let Some(guard) = conn.prepare_read() {
            match guard.read() {
                Ok(_) => {}
                Err(WaylandError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock => {}
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
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            if let Ok(keyboard) = self.seat_state.get_keyboard(qh, &seat, None) {
                self.keyboard = Some(keyboard);
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            if let Some(keyboard) = self.keyboard.take() {
                keyboard.release();
            }
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
        _: smithay_client_toolkit::seat::keyboard::Modifiers,
        _: smithay_client_toolkit::seat::keyboard::RawModifiers,
        _: u32,
    ) {
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
        _configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if !self.configured {
            self.configured = true;
            self.draw(qh);
        }
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
        _configure: WindowConfigure,
        _serial: u32,
    ) {
        if !self.configured {
            self.configured = true;
            self.draw(qh);
        }
    }
}

impl App {
    fn draw(&mut self, _qh: &QueueHandle<Self>) {
        let s = self.scale as i32;
        let w = 1920 * s;
        let h = CARD_H as i32 * s;
        let stride = w * 4;
        let Ok((buffer, canvas)) = self.pool.create_buffer(w, h, stride, wl_shm::Format::Argb8888)
        else {
            return;
        };
        let mut pix = match Pixmap::from_vec(
            canvas.to_vec(),
            tiny_skia::IntSize::from_wh(w as u32, h as u32).unwrap(),
        ) {
            Some(p) => p,
            None => return,
        };
        let ui = match self.ui.lock() {
            Ok(ui) => ui,
            Err(_) => return,
        };
        draw_card(&mut pix, Transform::from_scale(s as f32, s as f32), &ui, font());
        drop(ui);
        canvas.copy_from_slice(pix.data());
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

// ---------------------------------------------------------------------------
// Desenho (funções puras, independentes do Wayland)

fn rounded_rect_path(r: Rect, radius: f32) -> tiny_skia::Path {
    let mut pb = PathBuilder::new();
    pb.move_to(r.x() + radius, r.y());
    pb.line_to(r.right() - radius, r.y());
    pb.quad_to(r.right(), r.y(), r.right(), r.y() + radius);
    pb.line_to(r.right(), r.bottom() - radius);
    pb.quad_to(r.right(), r.bottom(), r.right() - radius, r.bottom());
    pb.line_to(r.x() + radius, r.bottom());
    pb.quad_to(r.x(), r.bottom(), r.x(), r.bottom() - radius);
    pb.line_to(r.x(), r.y() + radius);
    pb.quad_to(r.x(), r.y(), r.x() + radius, r.y());
    pb.close();
    pb.finish().expect("path de retângulo arredondado")
}

fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Recording => "gravando",
        Phase::Paused => "pausado",
        Phase::Transcribing => "transcrevendo",
        Phase::Loading => "carregando",
        Phase::Error => "erro",
    }
}

fn phase_color(phase: Phase) -> (u8, u8, u8) {
    match phase {
        Phase::Recording => (46, 204, 113),
        Phase::Paused => (241, 196, 15),
        Phase::Transcribing | Phase::Loading => (52, 152, 219),
        Phase::Error => (231, 76, 60),
    }
}

fn draw_card(pix: &mut Pixmap, t: Transform, ui: &UiState, font: &FontArc) {
    let (r, g, b) = phase_color(ui.phase);
    let w = CARD_W;
    let h = CARD_H;

    // Superfície pode ser mais larga que o cartão (layer full-width).
    let cx = (pix.width() as f32 / t.sx - w) / 2.0;
    let cy = 0.0;

    let mut paint = Paint::default();

    // Sombra/fundo do cartão.
    paint.set_color_rgba8(24, 24, 28, 235);
    let rect = Rect::from_xywh(cx, cy, w, h).unwrap();
    let path = rounded_rect_path(rect, 14.0);
    pix.fill_path(&path, &paint, FillRule::Winding, t, None);

    // Barra de status à esquerda (cor da fase).
    paint.set_color_rgba8(r, g, b, 255);
    let bar = Rect::from_xywh(cx + 14.0, cy + 16.0, 4.0, 20.0).unwrap();
    pix.fill_rect(bar, &paint, t, None);

    // Título: língua · modelo.
    draw_text(
        pix,
        font,
        &ui.lang_model,
        cx + 28.0,
        cy + 30.0,
        13.0,
        (160, 160, 160, 255),
        w - 40.0,
        t,
    );

    // Fase à direita.
    let label = phase_label(ui.phase);
    let lw = measure(font, label, 13.0);
    draw_text(
        pix,
        font,
        label,
        cx + w - 14.0 - lw,
        cy + 30.0,
        13.0,
        (r, g, b, 255),
        w - 40.0,
        t,
    );

    // Waveform central.
    let wave_top = 52.0;
    let wave_h = 62.0;
    let n = ui.levels.len();
    let bar_w = 6.0;
    let gap = 3.0;
    let total = n as f32 * (bar_w + gap) - gap;
    let x0 = cx + (w - total) / 2.0;
    paint.set_color_rgba8(r, g, b, 210);
    for (i, lvl) in ui.levels.iter().enumerate() {
        let bh = (lvl.sqrt() * wave_h * 3.0).clamp(3.0, wave_h);
        let x = x0 + i as f32 * (bar_w + gap);
        let y = cy + wave_top + (wave_h - bh) / 2.0;
        let bar = Rect::from_xywh(x, y, bar_w, bh).unwrap();
        pix.fill_rect(bar, &paint, t, None);
    }
    if n == 0 {
        // Nível de repouso quando ainda não há áudio.
        paint.set_color_rgba8(r, g, b, 90);
        let base = Rect::from_xywh(cx + w / 2.0 - 24.0, cy + wave_top + wave_h / 2.0 - 1.0, 48.0, 2.0)
            .unwrap();
        pix.fill_rect(base, &paint, t, None);
    }

    // Status principal (texto transcrito, erros etc.).
    let status = ui.status.as_deref().unwrap_or(label);
    let status_color = if ui.phase == Phase::Error {
        (231, 76, 60, 255)
    } else {
        (240, 240, 240, 255)
    };
    let sw = measure(font, status, 15.0);
    draw_text(
        pix,
        font,
        status,
        cx + (w - sw) / 2.0,
        cy + 138.0,
        15.0,
        status_color,
        w - 40.0,
        t,
    );

    // Rodapé: dicas de hotkeys, ou o aviso (ex.: wtype ausente) quando houver.
    let hints = "Space pausar   ·   Enter concluir   ·   Esc cancelar";
    let (footer, color) = match &ui.warning {
        Some(w) => (w.as_str(), (241, 196, 15, 255)), // âmbar: atenção
        None => (hints, (120, 120, 130, 255)),
    };
    let fw = measure(font, footer, 12.0);
    draw_text(
        pix,
        font,
        footer,
        cx + (w - fw) / 2.0,
        cy + 164.0,
        12.0,
        color,
        w - 40.0,
        t,
    );
}

fn measure(font: &FontArc, text: &str, size: f32) -> f32 {
    let sf = font.as_scaled(PxScale::from(size));
    text.chars().map(|c| sf.h_advance(font.glyph_id(c))).sum()
}

/// Desenha texto simples, truncando com "…" se exceder `max_w`.
fn draw_text(
    pix: &mut Pixmap,
    font: &FontArc,
    text: &str,
    x: f32,
    y: f32,
    size: f32,
    color: (u8, u8, u8, u8),
    max_w: f32,
    t: Transform,
) {
    let scale = PxScale::from(size);
    let sf = font.as_scaled(scale);

    let mut pen = x;
    let mut truncated = false;
    for (i, c) in text.chars().enumerate() {
        let gid = font.glyph_id(c);
        let adv = sf.h_advance(gid);
        if i > 0 && pen + adv > x + max_w {
            truncated = true;
            break;
        }
        let glyph = gid.with_scale_and_position(scale, ab_glyph::point(pen, y));
        if let Some(outline) = font.outline_glyph(glyph) {
            paint_glyph(pix, &outline, color, t);
        }
        pen += adv;
    }
    if truncated {
        let gid = font.glyph_id('…');
        let glyph = gid.with_scale_and_position(scale, ab_glyph::point(pen, y));
        if let Some(outline) = font.outline_glyph(glyph) {
            paint_glyph(pix, &outline, color, t);
        }
    }
}

/// Rasteriza um glifo via callback de cobertura (AA) e compõe pixel a pixel
/// (source-over em espaço premultiplicado — o `fill_rect` com máscara do
/// tiny-skia não pintou nada neste setup, então o blend é manual).
fn paint_glyph(
    pix: &mut Pixmap,
    outline: &ab_glyph::OutlinedGlyph,
    color: (u8, u8, u8, u8),
    t: Transform,
) {
    let b = outline.px_bounds();
    let (mw, mh) = (b.width() as u32, b.height() as u32);
    if mw == 0 || mh == 0 {
        return;
    }
    let s = t.sx;
    let (r, g, bl, a) = (
        color.0 as f32 / 255.0,
        color.1 as f32 / 255.0,
        color.2 as f32 / 255.0,
        color.3 as f32 / 255.0,
    );
    let w = pix.width() as i32;
    let h = pix.height() as i32;
    outline.draw(|x, y, cov| {
        let dx = ((b.min.x + x as f32) * s) as i32;
        let dy = ((b.min.y + y as f32) * s) as i32;
        if dx < 0 || dy < 0 || dx >= w || dy >= h {
            return;
        }
        let idx = (dy as usize * w as usize + dx as usize) * 4;
        let sa = a * cov;
        let inv = 1.0 - sa;
        let px = pix.data_mut();
        px[idx] = ((r * cov + px[idx] as f32 / 255.0 * inv) * 255.0) as u8;
        px[idx + 1] = ((g * cov + px[idx + 1] as f32 / 255.0 * inv) * 255.0) as u8;
        px[idx + 2] = ((bl * cov + px[idx + 2] as f32 / 255.0 * inv) * 255.0) as u8;
        px[idx + 3] = ((sa + px[idx + 3] as f32 / 255.0 * inv) * 255.0) as u8;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_font_loads() {
        let f = font();
        assert!(measure(f, "abc", 13.0) > 0.0);
    }

    #[test]
    fn draw_card_does_not_panic() {
        let mut pix = Pixmap::new(800, 180).unwrap();
        let mut ui = UiState::new("pt · turbo".to_string());
        ui.phase = Phase::Recording;
        for i in 0..10 {
            ui.push_level(i as f32 / 10.0);
        }
        draw_card(&mut pix, Transform::identity(), &ui, font());
        // fundo desenhado: pixel central não é zero
        let px = pix.pixel(400, 90).unwrap();
        assert!(px.alpha() > 0);
    }

    #[test]
    fn text_measure_is_positive_and_monotonic() {
        let f = font();
        let m1 = measure(f, "a", 13.0);
        let m2 = measure(f, "aa", 13.0);
        assert!(m1 > 0.0);
        assert!(m2 > m1);
    }

    #[test]
    fn footer_warning_fits_in_card() {
        // O aviso de wtype ausente (e as dicas padrão) não podem estourar a
        // largura do cartão; draw_text truncaria com "…", perdendo info.
        let f = font();
        let msg = "wtype ausente — a digitação na app não vai funcionar (só clipboard)";
        assert!(measure(f, msg, 12.0) <= CARD_W - 40.0);
        let hints = "Space pausar   ·   Enter concluir   ·   Esc cancelar";
        assert!(measure(f, hints, 12.0) <= CARD_W - 40.0);
    }

    /// Renderiza o OSD em PNG para inspeção visual (ignorado por padrão).
    #[test]
    #[ignore = "gera PNG para inspeção visual"]
    fn render_osd_png() {
        let mut pix = Pixmap::new(1920, 180).unwrap();
        let mut ui = UiState::new("en · tiny".to_string());
        ui.phase = Phase::Recording;
        for i in 0..48 {
            let v = ((i as f32 / 48.0) * std::f32::consts::PI).sin().abs();
            ui.push_level(0.02 + v * 0.3);
        }
        ui.status = Some(
            "✓  And so, my fellow Americans, ask not what your country can do for you…".to_string(),
        );
        draw_card(&mut pix, Transform::identity(), &ui, font());
        pix.save_png("/tmp/whisper-osd.png").unwrap();

        // Variante com aviso de wtype ausente no rodapé.
        let mut pix = Pixmap::new(1920, 180).unwrap();
        let mut ui = UiState::new("pt · turbo".to_string());
        ui.phase = Phase::Recording;
        ui.warning = Some("wtype ausente — a digitação na app não vai funcionar (só clipboard)".to_string());
        draw_card(&mut pix, Transform::identity(), &ui, font());
        pix.save_png("/tmp/whisper-osd-warning.png").unwrap();
    }
}
