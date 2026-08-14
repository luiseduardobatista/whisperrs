//! Desenho puro do cartão do OSD, independente do Wayland.

use ab_glyph::{Font as _, FontArc, PxScale, ScaleFont as _};
use tiny_skia::{FillRule, Paint, PathBuilder, PixmapMut, Rect, Transform};

use crate::osd::{Phase, UiState};

/// Dimensões lógicas do cartão (escaladas pela saída na renderização).
pub(crate) const CARD_W: f32 = 460.0;
pub(crate) const CARD_H: f32 = 180.0;

// Fonte embutida via include_bytes (DejaVu Sans, licença livre).
static FONT: std::sync::OnceLock<FontArc> = std::sync::OnceLock::new();

pub(crate) fn font() -> &'static FontArc {
    FONT.get_or_init(|| {
        FontArc::try_from_slice(include_bytes!("../assets/DejaVuSans.ttf"))
            .expect("fonte embutida inválida")
    })
}

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

pub(crate) fn draw_card(pix: &mut PixmapMut<'_>, t: Transform, ui: &UiState, font: &FontArc) {
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
        let base = Rect::from_xywh(
            cx + w / 2.0 - 24.0,
            cy + wave_top + wave_h / 2.0 - 1.0,
            48.0,
            2.0,
        )
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
    fn draw_card_does_not_panic() {
        let mut pix = Pixmap::new(800, 180).unwrap();
        let mut ui = UiState::new("pt · turbo".to_string());
        ui.phase = Phase::Recording;
        for i in 0..10 {
            ui.push_level(i as f32 / 10.0);
        }
        draw_card(&mut pix.as_mut(), Transform::identity(), &ui, font());
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
        draw_card(&mut pix.as_mut(), Transform::identity(), &ui, font());
        pix.save_png("/tmp/whisper-osd.png").unwrap();

        // Variante com aviso de wtype ausente no rodapé.
        let mut pix = Pixmap::new(1920, 180).unwrap();
        let mut ui = UiState::new("pt · turbo".to_string());
        ui.phase = Phase::Recording;
        ui.warning =
            Some("wtype ausente — a digitação na app não vai funcionar (só clipboard)".to_string());
        draw_card(&mut pix.as_mut(), Transform::identity(), &ui, font());
        pix.save_png("/tmp/whisper-osd-warning.png").unwrap();
    }
}
