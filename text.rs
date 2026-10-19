//! Text rasteriser using bundled DejaVu fonts (see assets/fonts/LICENSE-DejaVu.txt).
use super::compositor::Bitmap;
use crate::project::model::{Align, FontId, TextStyle};
use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use std::sync::OnceLock;

static SANS: &[u8] = include_bytes!("../../assets/fonts/DejaVuSans.ttf");
static SANS_BOLD: &[u8] = include_bytes!("../../assets/fonts/DejaVuSans-Bold.ttf");
static SERIF: &[u8] = include_bytes!("../../assets/fonts/DejaVuSerif.ttf");
static MONO: &[u8] = include_bytes!("../../assets/fonts/DejaVuSansMono.ttf");

fn font(id: FontId) -> &'static FontRef<'static> {
    static F: OnceLock<[FontRef<'static>; 4]> = OnceLock::new();
    let f = F.get_or_init(|| {
        [SANS, SANS_BOLD, SERIF, MONO].map(|b| FontRef::try_from_slice(b).expect("bundled font is valid"))
    });
    &f[match id {
        FontId::Sans => 0,
        FontId::SansBold => 1,
        FontId::Serif => 2,
        FontId::Mono => 3,
    }]
}

struct Line {
    text: String,
    width: f32,
}

fn measure(font: &FontRef<'static>, scale: PxScale, s: &str) -> f32 {
    let sc = font.as_scaled(scale);
    let mut w = 0.0;
    let mut prev = None;
    for ch in s.chars() {
        let id = sc.glyph_id(ch);
        if let Some(p) = prev {
            w += sc.kern(p, id);
        }
        w += sc.h_advance(id);
        prev = Some(id);
    }
    w
}

fn wrap(font: &FontRef<'static>, scale: PxScale, text: &str, max_w: f32) -> Vec<Line> {
    let mut lines = vec![];
    for para in text.split('\n') {
        let mut cur = String::new();
        for word in para.split(' ') {
            let trial = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
            if !cur.is_empty() && measure(font, scale, &trial) > max_w {
                let width = measure(font, scale, &cur);
                lines.push(Line { text: std::mem::take(&mut cur), width });
                cur = word.to_string();
            } else {
                cur = trial;
            }
        }
        let width = measure(font, scale, &cur);
        lines.push(Line { text: cur, width });
    }
    lines
}

/// Render `style.text` (first `visible` chars) into a tight RGBA bitmap sized
/// relative to the canvas. Returns None when nothing is visible.
pub fn render_text(style: &TextStyle, visible: usize, canvas_w: u32, canvas_h: u32) -> Option<Bitmap> {
    let shown: String = style.text.chars().take(visible).collect();
    if shown.trim().is_empty() {
        return None;
    }
    let f = font(style.font);
    let px = (style.size_pct / 100.0 * canvas_h as f32).clamp(4.0, 4000.0);
    let scale = PxScale::from(px);
    let sc = f.as_scaled(scale);
    // Wrap against the text's FULL length so the layout doesn't jump while the
    // typewriter reveals characters.
    let max_w = canvas_w as f32 * 0.9;
    let full_lines = wrap(f, scale, &style.text, max_w);
    let lines_shown = {
        let mut remaining = visible;
        let mut out = vec![];
        for l in &full_lines {
            let n = l.text.chars().count();
            let take = remaining.min(n);
            out.push(l.text.chars().take(take).collect::<String>());
            // wrapping consumed a space/newline between lines
            remaining = remaining.saturating_sub(n + 1);
            if remaining == 0 && take < n {
                break;
            }
        }
        out
    };
    let line_h = (sc.ascent() - sc.descent() + sc.line_gap()) * 1.05;
    let block_w = full_lines.iter().map(|l| l.width).fold(0.0, f32::max).ceil();
    let r_out = if style.outline { (px * 0.06).max(1.5) } else { 0.0 };
    let sh = if style.shadow { (px * 0.05).max(2.0) } else { 0.0 };
    let pad = (r_out + sh + 2.0).ceil();
    let bw = (block_w + 2.0 * pad) as u32;
    let bh = (line_h * full_lines.len() as f32 + 2.0 * pad).ceil() as u32;
    if bw == 0 || bh == 0 || bw > 16384 || bh > 16384 {
        return None;
    }
    let raster = |dx: f32, dy: f32, buf: &mut Vec<f32>| {
        for (i, line) in lines_shown.iter().enumerate() {
            let full_w = full_lines[i].width;
            let _ = full_w;
            let lw = measure(f, scale, line);
            let x0 = pad
                + match style.align {
                    Align::Left => 0.0,
                    Align::Center => (block_w - lw) / 2.0,
                    Align::Right => block_w - lw,
                };
            let baseline = pad + sc.ascent() + i as f32 * line_h;
            let (mut x, mut prev) = (x0, None);
            for ch in line.chars() {
                let id = sc.glyph_id(ch);
                if let Some(p) = prev {
                    x += sc.kern(p, id);
                }
                let g = id.with_scale_and_position(scale, point(x + dx, baseline + dy));
                x += sc.h_advance(id);
                prev = Some(id);
                if let Some(og) = f.outline_glyph(g) {
                    let b = og.px_bounds();
                    og.draw(|gx, gy, c| {
                        let (px_, py_) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                        if px_ >= 0 && py_ >= 0 && (px_ as u32) < bw && (py_ as u32) < bh {
                            let i = py_ as usize * bw as usize + px_ as usize;
                            buf[i] = buf[i].max(c);
                        }
                    });
                }
            }
        }
    };
    let n = (bw * bh) as usize;
    let mut fill = vec![0f32; n];
    raster(0.0, 0.0, &mut fill);
    let mut out = vec![0f32; n * 4]; // premultiplied RGBA
    let over = |out: &mut Vec<f32>, cov: &[f32], c: [u8; 4], k: f32| {
        for i in 0..n {
            let a = cov[i] * (c[3] as f32 / 255.0) * k;
            if a <= 0.0 {
                continue;
            }
            let o = &mut out[i * 4..i * 4 + 4];
            let inv = 1.0 - a;
            o[0] = c[0] as f32 / 255.0 * a + o[0] * inv;
            o[1] = c[1] as f32 / 255.0 * a + o[1] * inv;
            o[2] = c[2] as f32 / 255.0 * a + o[2] * inv;
            o[3] = a + o[3] * inv;
        }
    };
    if style.shadow {
        let mut s = vec![0f32; n];
        raster(sh, sh, &mut s);
        over(&mut out, &s, [0, 0, 0, 255], 0.6);
    }
    if style.outline {
        let mut o = vec![0f32; n];
        for k in 0..12 {
            let a = k as f32 / 12.0 * std::f32::consts::TAU;
            raster(r_out * a.cos(), r_out * a.sin(), &mut o);
        }
        over(&mut out, &o, style.outline_color, 1.0);
    }
    over(&mut out, &fill, style.color, 1.0);
    let mut data = vec![0u8; n * 4];
    for i in 0..n {
        let a = out[i * 4 + 3];
        if a > 0.0 {
            for c in 0..3 {
                data[i * 4 + c] = ((out[i * 4 + c] / a).clamp(0.0, 1.0) * 255.0).round() as u8;
            }
            data[i * 4 + 3] = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
        }
    }
    Some(Bitmap { w: bw, h: bh, data })
}
