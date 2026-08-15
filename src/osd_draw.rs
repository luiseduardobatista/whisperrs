//! Desenho puro do cartão do OSD, independente do Wayland.

use ab_glyph::{Font as _, FontArc, PxScale, ScaleFont as _};
use tiny_skia::{FillRule, Paint, PathBuilder, PixmapMut, Rect, Stroke, Transform};

use crate::osd::{Phase, UiState};

/// Dimensões lógicas do cartão (escaladas pela saída na renderização).
pub(crate) const CARD_W: f32 = 460.0;
pub(crate) const CARD_H: f32 = 148.0;

const GREEN: (u8, u8, u8) = (46, 204, 113);
const BLUE: (u8, u8, u8) = (52, 152, 219);
const RED: (u8, u8, u8) = (235, 103, 103);
const CARD_RADIUS: f32 = 13.0;
const KEYCAP_TEXT_SIZE: f32 = 10.5;
const ACTION_TEXT_SIZE: f32 = 11.0;

// Fonte embutida via include_bytes (DejaVu Sans, licença livre).
static FONT: std::sync::OnceLock<FontArc> = std::sync::OnceLock::new();

pub(crate) fn font() -> &'static FontArc {
    FONT.get_or_init(|| {
        FontArc::try_from_slice(include_bytes!("../assets/DejaVuSans.ttf"))
            .expect("fonte embutida inválida")
    })
}

fn rounded_rect_path(r: Rect, radius: f32) -> tiny_skia::Path {
    let radius = radius.min(r.width() / 2.0).min(r.height() / 2.0);
    let kappa = 0.552_284_8;
    let control = radius * kappa;
    let mut pb = PathBuilder::new();
    pb.move_to(r.x() + radius, r.y());
    pb.line_to(r.right() - radius, r.y());
    pb.cubic_to(
        r.right() - radius + control,
        r.y(),
        r.right(),
        r.y() + radius - control,
        r.right(),
        r.y() + radius,
    );
    pb.line_to(r.right(), r.bottom() - radius);
    pb.cubic_to(
        r.right(),
        r.bottom() - radius + control,
        r.right() - radius + control,
        r.bottom(),
        r.right() - radius,
        r.bottom(),
    );
    pb.line_to(r.x() + radius, r.bottom());
    pb.cubic_to(
        r.x() + radius - control,
        r.bottom(),
        r.x(),
        r.bottom() - radius + control,
        r.x(),
        r.bottom() - radius,
    );
    pb.line_to(r.x(), r.y() + radius);
    pb.cubic_to(
        r.x(),
        r.y() + radius - control,
        r.x() + radius - control,
        r.y(),
        r.x() + radius,
        r.y(),
    );
    pb.close();
    pb.finish().expect("path de retângulo arredondado")
}

fn circle_path(x: f32, y: f32, radius: f32) -> tiny_skia::Path {
    let mut pb = PathBuilder::new();
    pb.push_circle(x, y, radius);
    pb.finish().expect("path de círculo")
}

fn fill_path(
    pix: &mut PixmapMut<'_>,
    path: &tiny_skia::Path,
    color: (u8, u8, u8, u8),
    t: Transform,
) {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.0, color.1, color.2, color.3);
    pix.fill_path(path, &paint, FillRule::Winding, t, None);
}

fn stroke_path(
    pix: &mut PixmapMut<'_>,
    path: &tiny_skia::Path,
    color: (u8, u8, u8, u8),
    width: f32,
    t: Transform,
) {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.0, color.1, color.2, color.3);
    let stroke = Stroke {
        width,
        ..Stroke::default()
    };
    pix.stroke_path(path, &paint, &stroke, t, None);
}

fn fill_rounded_rect(
    pix: &mut PixmapMut<'_>,
    rect: Rect,
    radius: f32,
    color: (u8, u8, u8, u8),
    t: Transform,
) {
    let path = rounded_rect_path(rect, radius);
    fill_path(pix, &path, color, t);
}

fn fill_circle(
    pix: &mut PixmapMut<'_>,
    x: f32,
    y: f32,
    radius: f32,
    color: (u8, u8, u8, u8),
    t: Transform,
) {
    let path = circle_path(x, y, radius);
    fill_path(pix, &path, color, t);
}

fn phase_label(phase: Phase) -> &'static str {
    match phase {
        Phase::Recording => "gravando",
        Phase::Paused => "pausado",
        Phase::Transcribing => "transcrevendo",
        Phase::Loading => "carregando",
        Phase::Cleaning => "transcrevendo",
        Phase::Error => "erro",
    }
}

fn phase_color(phase: Phase) -> (u8, u8, u8) {
    match phase {
        Phase::Recording => GREEN,
        Phase::Error => RED,
        Phase::Paused | Phase::Transcribing | Phase::Loading | Phase::Cleaning => BLUE,
    }
}

#[derive(Clone, Copy)]
struct CardLayout {
    x: f32,
    y: f32,
    w: f32,
}

pub(crate) fn draw_card(
    pix: &mut PixmapMut<'_>,
    t: Transform,
    ui: &UiState,
    font: &FontArc,
    animation_time: f32,
) {
    let accent = phase_color(ui.phase);
    let surface_w = pix.width() as f32 / t.sx;
    // No fallback para xdg-toplevel, a superfície pode ter exatamente CARD_W.
    // Deixe uma margem mínima para que o stroke não seja cortado nas laterais.
    let w = CARD_W.min((surface_w - 4.0).max(1.0));
    let layout = CardLayout {
        x: (surface_w - w) / 2.0,
        y: 2.0,
        w,
    };
    let card_h = CARD_H - layout.y * 2.0;
    let card = Rect::from_xywh(layout.x, layout.y, layout.w, card_h).unwrap();

    // A borda é uma segunda forma arredondada, em vez de um stroke centrado:
    // assim sua espessura não é cortada nas extremidades da superfície.
    fill_rounded_rect(
        pix,
        card,
        CARD_RADIUS,
        (accent.0, accent.1, accent.2, 46),
        t,
    );
    let inner = Rect::from_xywh(
        layout.x + 1.0,
        layout.y + 1.0,
        (layout.w - 2.0).max(1.0),
        (card_h - 2.0).max(1.0),
    )
    .unwrap();
    fill_rounded_rect(
        pix,
        inner,
        (CARD_RADIUS - 1.0).max(0.0),
        (16, 20, 25, 244),
        t,
    );

    draw_header(pix, font, ui, accent, layout, animation_time, t);
    match ui.phase {
        Phase::Recording | Phase::Paused => {
            draw_recording_waveform(pix, ui, accent, layout, t);
        }
        Phase::Transcribing | Phase::Loading | Phase::Cleaning => {
            draw_processing_indicator(pix, accent, layout.x, layout.y, layout.w, animation_time, t);
        }
        Phase::Error => draw_error_indicator(pix, accent, layout.x, layout.y, layout.w, t),
    }
    draw_center_status(pix, font, ui, layout.x, layout.y, layout.w, t);
    draw_footer(pix, font, ui, layout.x, layout.y, layout.w, t);
}

fn draw_header(
    pix: &mut PixmapMut<'_>,
    font: &FontArc,
    ui: &UiState,
    accent: (u8, u8, u8),
    layout: CardLayout,
    animation_time: f32,
    t: Transform,
) {
    let CardLayout {
        x: card_x,
        y: card_y,
        w,
    } = layout;
    let baseline = card_y + 30.0;
    let marker = Rect::from_xywh(card_x + 16.0, card_y + 18.0, 3.0, 18.0).unwrap();
    fill_rounded_rect(pix, marker, 1.5, (accent.0, accent.1, accent.2, 255), t);

    let label = phase_label(ui.phase);
    let label_size = 13.0;
    let label_w = measure(font, label, label_size);
    let label_x = card_x + w - 18.0 - label_w;
    draw_text(
        pix,
        font,
        label,
        label_x,
        baseline,
        label_size,
        (accent.0, accent.1, accent.2, 255),
        label_w,
        t,
    );
    draw_status_indicator(
        pix,
        label_x - 13.0,
        card_y + 25.0,
        accent,
        ui.phase,
        animation_time,
        t,
    );

    let left_x = card_x + 28.0;
    let left_limit = (label_x - 24.0).max(left_x);
    let available_w = left_limit - left_x;
    let smart = "smart";
    let smart_size = 10.5;
    let smart_w = measure(font, smart, smart_size) + 14.0;
    let smart_fits = ui.smart && available_w > smart_w + 10.0;
    let lang_max_w = if smart_fits {
        available_w - smart_w - 10.0
    } else {
        available_w
    };
    draw_text(
        pix,
        font,
        &ui.lang_model,
        left_x,
        baseline,
        label_size,
        (163, 168, 177, 255),
        lang_max_w,
        t,
    );

    if smart_fits {
        let lang_w = measure(font, &ui.lang_model, label_size).min(lang_max_w);
        let badge_x = left_x + lang_w + 10.0;
        let badge = Rect::from_xywh(badge_x, card_y + 20.0, smart_w, 17.0).unwrap();
        fill_rounded_rect(pix, badge, 5.0, (52, 152, 219, 34), t);
        stroke_path(
            pix,
            &rounded_rect_path(badge, 5.0),
            (78, 166, 255, 80),
            1.0,
            t,
        );
        draw_text(
            pix,
            font,
            smart,
            badge_x + 7.0,
            card_y + 32.5,
            smart_size,
            (105, 182, 255, 255),
            smart_w - 14.0,
            t,
        );
    }
}

fn draw_status_indicator(
    pix: &mut PixmapMut<'_>,
    x: f32,
    y: f32,
    color: (u8, u8, u8),
    phase: Phase,
    animation_time: f32,
    t: Transform,
) {
    match phase {
        Phase::Recording => {
            fill_circle(pix, x, y, 7.0, (color.0, color.1, color.2, 255), t);
            fill_circle(pix, x, y, 3.7, (16, 20, 25, 255), t);
            fill_circle(pix, x, y, 2.0, (color.0, color.1, color.2, 255), t);
        }
        Phase::Paused => fill_circle(pix, x, y, 4.0, (color.0, color.1, color.2, 255), t),
        Phase::Transcribing | Phase::Loading | Phase::Cleaning => {
            fill_circle(pix, x, y, 7.0, (color.0, color.1, color.2, 205), t);
            fill_circle(pix, x, y, 4.8, (16, 20, 25, 255), t);
            let angle = animation_time * 3.2;
            fill_circle(
                pix,
                x + angle.cos() * 5.9,
                y + angle.sin() * 5.9,
                1.8,
                (color.0, color.1, color.2, 255),
                t,
            );
        }
        Phase::Error => {
            fill_circle(pix, x, y, 7.0, (color.0, color.1, color.2, 210), t);
            fill_circle(pix, x, y, 4.8, (16, 20, 25, 255), t);
            fill_circle(pix, x, y, 1.8, (color.0, color.1, color.2, 255), t);
        }
    }
}

fn draw_recording_waveform(
    pix: &mut PixmapMut<'_>,
    ui: &UiState,
    accent: (u8, u8, u8),
    layout: CardLayout,
    t: Transform,
) {
    let CardLayout {
        x: card_x,
        y: card_y,
        w,
    } = layout;
    let center_y = card_y + 72.0;
    let wave_h = 38.0;
    let bar_w = 3.0;
    let gap = 5.0;
    let n = ui.levels.len();
    let total = n as f32 * (bar_w + gap) - gap;
    let x0 = card_x + (w - total) / 2.0;

    for (i, level) in ui.levels.iter().enumerate() {
        let normalized = level.max(0.0).sqrt();
        let bar_h = (normalized * wave_h * 1.65).clamp(3.0, wave_h);
        let x = x0 + i as f32 * (bar_w + gap);
        let y = center_y - bar_h / 2.0;
        let bar = Rect::from_xywh(x, y, bar_w, bar_h).unwrap();
        fill_rounded_rect(
            pix,
            bar,
            bar_w / 2.0,
            (accent.0, accent.1, accent.2, 218),
            t,
        );
    }

    if n == 0 {
        let base = Rect::from_xywh(card_x + w / 2.0 - 24.0, center_y - 1.0, 48.0, 2.0).unwrap();
        fill_rounded_rect(pix, base, 1.0, (accent.0, accent.1, accent.2, 90), t);
    }
}

fn draw_processing_indicator(
    pix: &mut PixmapMut<'_>,
    accent: (u8, u8, u8),
    card_x: f32,
    card_y: f32,
    w: f32,
    animation_time: f32,
    t: Transform,
) {
    let count = 46;
    let bar_w = 3.0;
    let gap = 5.0;
    let total = count as f32 * (bar_w + gap) - gap;
    let x0 = card_x + (w - total) / 2.0;
    let center_y = card_y + 72.0;
    let moving_peak = (animation_time * 18.0) % (count as f32 + 12.0) - 6.0;

    for i in 0..count {
        let distance = i as f32 - moving_peak;
        let intensity = (-distance * distance / 18.0).exp();
        let bar_h = 3.0 + intensity * 25.0;
        let alpha = (82.0 + intensity * 155.0) as u8;
        let x = x0 + i as f32 * (bar_w + gap);
        let y = center_y - bar_h / 2.0;
        let bar = Rect::from_xywh(x, y, bar_w, bar_h).unwrap();
        fill_rounded_rect(
            pix,
            bar,
            bar_w / 2.0,
            (accent.0, accent.1, accent.2, alpha),
            t,
        );
    }
}

fn draw_error_indicator(
    pix: &mut PixmapMut<'_>,
    accent: (u8, u8, u8),
    card_x: f32,
    card_y: f32,
    w: f32,
    t: Transform,
) {
    let line = Rect::from_xywh(card_x + w / 2.0 - 28.0, card_y + 71.0, 56.0, 2.0).unwrap();
    fill_rounded_rect(pix, line, 1.0, (accent.0, accent.1, accent.2, 110), t);
}

fn draw_center_status(
    pix: &mut PixmapMut<'_>,
    font: &FontArc,
    ui: &UiState,
    card_x: f32,
    card_y: f32,
    w: f32,
    t: Transform,
) {
    let label = phase_label(ui.phase);
    let Some(status) = center_status(ui, label) else {
        return;
    };
    let size = 14.0;
    let max_w = w - 48.0;
    let status_w = measure(font, status, size).min(max_w);
    let color = if ui.phase == Phase::Error {
        (RED.0, RED.1, RED.2, 255)
    } else {
        (164, 173, 185, 255)
    };
    draw_text(
        pix,
        font,
        status,
        card_x + (w - status_w) / 2.0,
        card_y + 106.0,
        size,
        color,
        max_w,
        t,
    );
}

fn center_status<'a>(ui: &'a UiState, label: &str) -> Option<&'a str> {
    match ui.phase {
        Phase::Recording | Phase::Paused => None,
        Phase::Transcribing | Phase::Cleaning => match ui.status.as_deref() {
            Some(status) if !status.trim().eq_ignore_ascii_case(label) => Some(status),
            _ => Some("Processando…"),
        },
        Phase::Loading => Some(ui.status.as_deref().unwrap_or("Carregando…")),
        Phase::Error => Some(ui.status.as_deref().unwrap_or("Erro")),
    }
}

#[derive(Clone, Copy)]
struct FooterAction {
    key: &'static str,
    label: &'static str,
}

const RECORDING_ACTIONS: &[FooterAction] = &[
    FooterAction {
        key: "Space",
        label: "pausar",
    },
    FooterAction {
        key: "Enter",
        label: "concluir",
    },
    FooterAction {
        key: "Esc",
        label: "cancelar",
    },
];
const PAUSED_ACTIONS: &[FooterAction] = &[
    FooterAction {
        key: "Space",
        label: "retomar",
    },
    FooterAction {
        key: "Enter",
        label: "concluir",
    },
    FooterAction {
        key: "Esc",
        label: "cancelar",
    },
];

fn footer_actions(phase: Phase) -> Option<&'static [FooterAction]> {
    match phase {
        Phase::Recording => Some(RECORDING_ACTIONS),
        Phase::Paused => Some(PAUSED_ACTIONS),
        Phase::Transcribing | Phase::Loading | Phase::Cleaning | Phase::Error => None,
    }
}

fn draw_footer(
    pix: &mut PixmapMut<'_>,
    font: &FontArc,
    ui: &UiState,
    card_x: f32,
    card_y: f32,
    w: f32,
    t: Transform,
) {
    let divider = Rect::from_xywh(card_x + 1.0, card_y + 110.0, w - 2.0, 1.0).unwrap();
    fill_path(
        pix,
        &rounded_rect_path(divider, 0.5),
        (255, 255, 255, 10),
        t,
    );

    if let Some(warning) = ui.warning.as_deref() {
        let max_w = w - 36.0;
        let text_w = measure(font, warning, 11.0).min(max_w);
        draw_text(
            pix,
            font,
            warning,
            card_x + (w - text_w) / 2.0,
            card_y + 133.0,
            11.0,
            (218, 171, 86, 255),
            max_w,
            t,
        );
        return;
    }

    let Some(actions) = footer_actions(ui.phase) else {
        return;
    };
    let gap = 13.0;
    let total: f32 = actions
        .iter()
        .map(|action| {
            measure(font, action.key, KEYCAP_TEXT_SIZE)
                + 14.0
                + 7.0
                + measure(font, action.label, ACTION_TEXT_SIZE)
        })
        .sum::<f32>()
        + gap * (actions.len().saturating_sub(1) as f32);
    let mut x = card_x + (w - total) / 2.0;
    for (index, action) in actions.iter().enumerate() {
        let group_w = draw_keycap(pix, font, *action, x, card_y + 117.0, t);
        x += group_w;
        if index + 1 < actions.len() {
            x += gap;
        }
    }
}

fn draw_keycap(
    pix: &mut PixmapMut<'_>,
    font: &FontArc,
    action: FooterAction,
    x: f32,
    y: f32,
    t: Transform,
) -> f32 {
    let key_w = measure(font, action.key, KEYCAP_TEXT_SIZE) + 14.0;
    let key_h = 20.0;
    let key = Rect::from_xywh(x, y, key_w, key_h).unwrap();
    fill_rounded_rect(pix, key, 4.5, (31, 37, 44, 235), t);
    stroke_path(
        pix,
        &rounded_rect_path(key, 4.5),
        (255, 255, 255, 34),
        1.0,
        t,
    );
    draw_text(
        pix,
        font,
        action.key,
        x + 7.0,
        y + 14.0,
        KEYCAP_TEXT_SIZE,
        (228, 232, 238, 255),
        key_w - 14.0,
        t,
    );

    let label_x = x + key_w + 7.0;
    let label_w = measure(font, action.label, ACTION_TEXT_SIZE);
    draw_text(
        pix,
        font,
        action.label,
        label_x,
        y + 14.0,
        ACTION_TEXT_SIZE,
        (145, 152, 163, 255),
        label_w,
        t,
    );
    key_w + 7.0 + label_w
}

fn measure(font: &FontArc, text: &str, size: f32) -> f32 {
    let sf = font.as_scaled(PxScale::from(size));
    text.chars().map(|c| sf.h_advance(font.glyph_id(c))).sum()
}

/// Desenha texto simples, truncando com "…" sem ultrapassar `max_w`.
// Parâmetros posicionais de desenho; agrupar num struct não simplifica
// os ~10 call sites (lint clippy::too_many_arguments aceito de propósito).
#[expect(
    clippy::too_many_arguments,
    reason = "coordenadas e estilo ficam explícitos no renderer manual"
)]
fn draw_text(
    pix: &mut PixmapMut<'_>,
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
    let max_w = max_w.max(0.0);
    let text_w = measure(font, text, size);
    let ellipsis = '…';
    let ellipsis_w = sf.h_advance(font.glyph_id(ellipsis));
    let truncated = text_w > max_w;
    let content_w = if truncated {
        (max_w - ellipsis_w).max(0.0)
    } else {
        max_w
    };

    let mut pen = x;
    for c in text.chars() {
        let gid = font.glyph_id(c);
        let adv = sf.h_advance(gid);
        if truncated && pen + adv > x + content_w {
            break;
        }
        let glyph = gid.with_scale_and_position(scale, ab_glyph::point(pen, y));
        if let Some(outline) = font.outline_glyph(glyph) {
            paint_glyph(pix, &outline, color, t);
        }
        pen += adv;
    }
    if truncated && max_w >= ellipsis_w {
        let glyph = font
            .glyph_id(ellipsis)
            .with_scale_and_position(scale, ab_glyph::point(x + max_w - ellipsis_w, y));
        if let Some(outline) = font.outline_glyph(glyph) {
            paint_glyph(pix, &outline, color, t);
        }
    }
}

/// Rasteriza um glifo via callback de cobertura (AA) e compõe pixel a pixel
/// (source-over em espaço premultiplicado — o `fill_rect` com máscara do
/// tiny-skia não pintou nada neste setup, então o blend é manual).
fn paint_glyph(
    pix: &mut PixmapMut<'_>,
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
    use tiny_skia::Pixmap;

    #[test]
    fn embedded_font_loads() {
        let f = font();
        assert!(measure(f, "abc", 13.0) > 0.0);
    }

    #[test]
    fn phase_colors_keep_recording_processing_and_error_distinct() {
        assert_eq!(phase_color(Phase::Recording), GREEN);
        assert_eq!(phase_color(Phase::Error), RED);
        assert_eq!(phase_color(Phase::Transcribing), BLUE);
        assert_eq!(phase_color(Phase::Paused), BLUE);
    }

    #[test]
    fn draw_card_does_not_panic_for_each_phase() {
        for phase in [
            Phase::Recording,
            Phase::Paused,
            Phase::Transcribing,
            Phase::Loading,
            Phase::Cleaning,
            Phase::Error,
        ] {
            let mut pix = Pixmap::new(800, CARD_H as u32).unwrap();
            let mut ui = UiState::new("pt · turbo".to_string());
            ui.phase = phase;
            ui.smart = true;
            for i in 0..10 {
                ui.push_level(i as f32 / 10.0);
            }
            draw_card(&mut pix.as_mut(), Transform::identity(), &ui, font(), 0.5);
            assert!(pix.pixel(400, 70).unwrap().alpha() > 0);
        }
    }

    #[test]
    fn rounded_corners_do_not_fill_square_corner_pixels() {
        let mut pix = Pixmap::new(800, CARD_H as u32).unwrap();
        let mut ui = UiState::new("pt · turbo".to_string());
        ui.phase = Phase::Recording;
        draw_card(&mut pix.as_mut(), Transform::identity(), &ui, font(), 0.0);
        assert_eq!(pix.pixel(170, 2).unwrap().alpha(), 0);
    }

    #[test]
    fn narrow_surface_keeps_card_border_inside_buffer() {
        let mut pix = Pixmap::new(CARD_W as u32, CARD_H as u32).unwrap();
        let ui = UiState::new("pt · turbo".to_string());
        draw_card(&mut pix.as_mut(), Transform::identity(), &ui, font(), 0.0);
        assert_eq!(pix.pixel(0, 70).unwrap().alpha(), 0);
    }

    #[test]
    fn hidpi_transform_keeps_card_inside_scaled_surface() {
        let mut pix = Pixmap::new(1600, CARD_H as u32 * 2).unwrap();
        let mut ui = UiState::new("pt · turbo".to_string());
        ui.phase = Phase::Recording;
        draw_card(
            &mut pix.as_mut(),
            Transform::from_scale(2.0, 2.0),
            &ui,
            font(),
            0.0,
        );
        assert!(pix.pixel(800, 140).unwrap().alpha() > 0);
    }

    #[test]
    fn processing_states_do_not_show_recording_actions() {
        assert!(footer_actions(Phase::Recording).is_some());
        assert!(footer_actions(Phase::Paused).is_some());
        assert!(footer_actions(Phase::Transcribing).is_none());
        assert!(footer_actions(Phase::Loading).is_none());
        assert!(footer_actions(Phase::Cleaning).is_none());
    }

    #[test]
    fn redundant_processing_status_becomes_processando() {
        let mut ui = UiState::new("pt · turbo".to_string());
        ui.phase = Phase::Transcribing;
        ui.status = Some("Transcrevendo".to_string());
        assert_eq!(center_status(&ui, "transcrevendo"), Some("Processando…"));
    }

    #[test]
    fn recording_has_no_duplicate_center_status() {
        let mut ui = UiState::new("pt · turbo".to_string());
        ui.phase = Phase::Recording;
        assert_eq!(center_status(&ui, "gravando"), None);
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
    fn footer_warnings_fit_without_truncation() {
        // Os avisos de wtype/VAD ausentes devem caber no rodapé em fonte menor.
        let f = font();
        let warnings = [
            "wtype ausente — a digitação na app não vai funcionar (só clipboard)",
            "VAD ausente — transcrevendo sem filtro de voz; rode whisper setup",
            "wtype ausente (só clipboard) · VAD ausente (sem filtro de voz)",
            "Qwen ausente — cleanup Rust; rode whisper setup",
            "wtype e Qwen ausentes — digitação e cleanup limitados",
            "VAD e Qwen ausentes — cleanup limitado; rode whisper setup",
            "wtype, VAD e Qwen ausentes — digitação e cleanup limitados",
        ];
        for warning in warnings {
            assert!(
                measure(f, warning, 11.0) <= CARD_W - 36.0,
                "aviso não cabe no rodapé: {warning}"
            );
        }
    }

    /// Renderiza exemplos do OSD em PNG para inspeção visual (ignorado por padrão).
    #[test]
    #[ignore = "gera PNG para inspeção visual"]
    fn render_osd_png() {
        let render = |path: &str, width: u32, ui: &UiState, time: f32| {
            let mut pix = Pixmap::new(width, CARD_H as u32).unwrap();
            draw_card(&mut pix.as_mut(), Transform::identity(), ui, font(), time);
            pix.save_png(path).unwrap();
        };

        let mut recording = UiState::new("auto · turbo".to_string());
        recording.phase = Phase::Recording;
        recording.smart = true;
        for i in 0..48 {
            let v = ((i as f32 / 48.0) * std::f32::consts::PI).sin().abs();
            recording.push_level(0.02 + v * 0.3);
        }
        render("/tmp/whisper-osd-recording.png", 1920, &recording, 0.0);
        render(
            "/tmp/whisper-osd-fallback.png",
            CARD_W as u32,
            &recording,
            0.0,
        );

        let mut paused = UiState::new("auto · turbo".to_string());
        paused.phase = Phase::Paused;
        for i in 0..48 {
            paused.push_level(0.03 + (i as f32 / 48.0).sin().abs() * 0.2);
        }
        render("/tmp/whisper-osd-paused.png", 1920, &paused, 0.0);

        let mut transcribing = UiState::new("auto · turbo".to_string());
        transcribing.phase = Phase::Transcribing;
        transcribing.status = Some("Transcrevendo".to_string());
        render(
            "/tmp/whisper-osd-transcribing.png",
            1920,
            &transcribing,
            0.8,
        );
    }
}
