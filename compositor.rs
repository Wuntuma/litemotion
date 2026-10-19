//! Software compositor. Everything is expressed in canvas fractions, so the
//! same code renders a 360p preview and a 4K export identically.
use super::text::render_text;
use crate::project::model::*;
use crate::time::{to_secs, Us};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Bitmap {
    pub w: u32,
    pub h: u32,
    /// RGBA, straight (non-premultiplied) alpha.
    pub data: Vec<u8>,
}
impl Bitmap {
    pub fn new(w: u32, h: u32, rgba: [u8; 4]) -> Bitmap {
        let mut data = vec![0u8; (w * h * 4) as usize];
        for px in data.chunks_exact_mut(4) {
            px.copy_from_slice(&rgba);
        }
        Bitmap { w, h, data }
    }
    pub fn bytes(&self) -> usize {
        self.data.len()
    }
}

/// Supplies decoded media pictures. `size` is the exact pixel size wanted.
pub trait FrameProvider {
    fn media_frame(&mut self, clip: &Clip, media: &MediaRef, local_t: Us, size: (u32, u32)) -> Option<Arc<Bitmap>>;
}

/// Small bounded cache for generated text/shape layers.
#[derive(Default)]
pub struct LayerCache {
    map: HashMap<u64, Option<Arc<Bitmap>>>,
}
impl LayerCache {
    fn get_or(&mut self, key: u64, f: impl FnOnce() -> Option<Bitmap>) -> Option<Arc<Bitmap>> {
        if self.map.len() > 48 {
            self.map.clear();
        }
        self.map.entry(key).or_insert_with(|| f().map(Arc::new)).clone()
    }
}

fn hash<T: std::hash::Hash>(t: &T) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}

/// Pixel size at which a media clip's picture should be decoded.
pub fn media_layout(m: &MediaRef, clip: &Clip, cw: u32, ch: u32) -> (u32, u32) {
    let sw = (m.width as f32 * (1.0 - clip.crop.left - clip.crop.right)).max(2.0);
    let sh = (m.height as f32 * (1.0 - clip.crop.top - clip.crop.bottom)).max(2.0);
    match clip.fit {
        FitMode::Stretch | FitMode::Fill => (cw, ch),
        FitMode::Fit => {
            let s = (cw as f32 / sw).min(ch as f32 / sh);
            (((sw * s).round() as u32).max(2), ((sh * s).round() as u32).max(2))
        }
    }
}

fn ease_p(kind: TransitionKind, p: f32) -> f32 {
    match kind {
        TransitionKind::Dissolve => p * p * (3.0 - 2.0 * p),
        _ => p,
    }
}

/// Opacity and brightness multipliers contributed by a clip's transitions.
pub fn transition_factors(clip: &Clip, local: Us) -> (f32, f32) {
    let (mut alpha, mut dim) = (1.0f32, 1.0f32);
    if let Some(t) = clip.transition_in {
        if local < t.duration {
            let p = ease_p(t.kind, local as f32 / t.duration as f32);
            match t.kind {
                TransitionKind::FadeFromBlack => dim *= p,
                _ => alpha *= p,
            }
        }
    }
    if let Some(t) = clip.transition_out {
        let rem = clip.duration - local;
        if rem < t.duration {
            dim *= (rem as f32 / t.duration as f32).clamp(0.0, 1.0);
        }
    }
    (alpha, dim)
}

pub fn compose(project: &Project, w: u32, h: u32, t: Us, provider: &mut dyn FrameProvider, cache: &mut LayerCache) -> Bitmap {
    let mut canvas = Bitmap::new(w, h, [0, 0, 0, 255]);
    for track in project.tracks.iter().filter(|t| t.kind.is_visual() && t.visible) {
        for clip in track.clips.iter().filter(|c| c.start <= t && t < c.end()) {
            let local = t - clip.start;
            let s = clip.sample(local);
            let (ta, dim) = transition_factors(clip, local);
            let alpha = s.opacity.clamp(0.0, 1.0) * ta;
            if alpha <= 0.0 || dim <= 0.0 || s.scale <= 0.0 {
                continue;
            }
            let layer: Option<Arc<Bitmap>> = match &clip.content {
                Content::Media { media_id } => match project.media(*media_id) {
                    Some(m) => provider.media_frame(clip, m, local, media_layout(m, clip, w, h)),
                    None => None,
                },
                Content::Text(st) => {
                    let visible = match st.typewriter_cps {
                        Some(cps) => ((to_secs(local) * cps as f64).floor() as usize).min(st.text.chars().count()),
                        None => usize::MAX,
                    };
                    let shown = visible.min(st.text.chars().count());
                    let key = hash(&(&st.text, shown, st.font as u8, st.size_pct.to_bits(), st.color, st.align as u8, st.outline, st.outline_color, st.shadow, h, w));
                    cache.get_or(key, || render_text(st, shown, w, h))
                }
                Content::Shape { shape, color, w: sw, h: sh } => {
                    let (pw, ph) = (((sw * w as f32).round() as u32).max(1), ((sh * h as f32).round() as u32).max(1));
                    let key = hash(&(*shape as u8, color, pw, ph));
                    cache.get_or(key, || Some(shape_bitmap(*shape, *color, pw, ph)))
                }
                Content::Solid { color } => {
                    let key = hash(&("solid", color, w, h));
                    cache.get_or(key, || Some(Bitmap::new(w, h, *color)))
                }
            };
            if let Some(l) = layer {
                let cx = w as f32 / 2.0 + s.x * w as f32;
                let cy = h as f32 / 2.0 + s.y * h as f32;
                draw_layer(&mut canvas, &l, cx, cy, s.scale, s.rotation, alpha, dim);
            }
        }
    }
    canvas
}

fn shape_bitmap(shape: ShapeKind, c: Color, w: u32, h: u32) -> Bitmap {
    let mut b = Bitmap::new(w, h, c);
    if shape == ShapeKind::Ellipse {
        let (rx, ry) = (w as f32 / 2.0, h as f32 / 2.0);
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = ((x as f32 + 0.5 - rx) / rx, (y as f32 + 0.5 - ry) / ry);
                let d = (dx * dx + dy * dy).sqrt();
                // 1px anti-aliased edge
                let edge = ((1.0 - d) * rx.min(ry)).clamp(0.0, 1.0);
                let i = ((y * w + x) * 4 + 3) as usize;
                b.data[i] = (c[3] as f32 * edge) as u8;
            }
        }
    }
    b
}

/// Draw `layer` centred at (cx,cy) with uniform `scale` and `rot_deg`.
pub fn draw_layer(canvas: &mut Bitmap, layer: &Bitmap, cx: f32, cy: f32, scale: f32, rot_deg: f32, alpha: f32, dim: f32) {
    let (cw, ch) = (canvas.w as i32, canvas.h as i32);
    let (lw, lh) = (layer.w as f32, layer.h as f32);
    let th = rot_deg.to_radians();
    let (sin, cos) = th.sin_cos();
    // Bounding box of the transformed layer.
    let (hw, hh) = (lw * scale / 2.0, lh * scale / 2.0);
    let corners = [(-hw, -hh), (hw, -hh), (hw, hh), (-hw, hh)];
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (x, y) in corners {
        let (rx, ry) = (x * cos - y * sin + cx, x * sin + y * cos + cy);
        x0 = x0.min(rx);
        x1 = x1.max(rx);
        y0 = y0.min(ry);
        y1 = y1.max(ry);
    }
    let (bx0, bx1) = ((x0.floor() as i32).max(0), (x1.ceil() as i32).min(cw));
    let (by0, by1) = ((y0.floor() as i32).max(0), (y1.ceil() as i32).min(ch));
    if bx0 >= bx1 || by0 >= by1 {
        return;
    }
    let inv_s = 1.0 / scale;
    let row_len = canvas.w as usize * 4;
    let layer_w = layer.w as i32;
    let layer_h = layer.h as i32;
    let ldata = &layer.data;
    let aligned = rot_deg == 0.0 && (scale - 1.0).abs() < 1e-6 && ((cx - lw / 2.0).fract().abs() < 1e-3 || (cx - lw / 2.0).fract().abs() > 0.999) && ((cy - lh / 2.0).fract().abs() < 1e-3 || (cy - lh / 2.0).fract().abs() > 0.999);
    let (ox, oy) = ((cx - lw / 2.0).round() as i32, (cy - lh / 2.0).round() as i32);
    canvas.data.par_chunks_mut(row_len).enumerate().for_each(|(py, row)| {
        let py = py as i32;
        if py < by0 || py >= by1 {
            return;
        }
        for px in bx0..bx1 {
            let (sr, sg, sb, sa);
            if aligned {
                let (u, v) = (px - ox, py - oy);
                if u < 0 || v < 0 || u >= layer_w || v >= layer_h {
                    continue;
                }
                let i = ((v * layer_w + u) * 4) as usize;
                sr = ldata[i] as f32;
                sg = ldata[i + 1] as f32;
                sb = ldata[i + 2] as f32;
                sa = ldata[i + 3] as f32 / 255.0;
            } else {
                let (dx, dy) = (px as f32 + 0.5 - cx, py as f32 + 0.5 - cy);
                let u = (dx * cos + dy * sin) * inv_s + lw / 2.0 - 0.5;
                let v = (-dx * sin + dy * cos) * inv_s + lh / 2.0 - 0.5;
                if u < -0.5 || v < -0.5 || u > lw - 0.5 || v > lh - 0.5 {
                    continue;
                }
                let (u0, v0) = (u.floor(), v.floor());
                let (fu, fv) = (u - u0, v - v0);
                let mut acc = [0f32; 4];
                for (ddx, ddy, wgt) in [(0, 0, (1.0 - fu) * (1.0 - fv)), (1, 0, fu * (1.0 - fv)), (0, 1, (1.0 - fu) * fv), (1, 1, fu * fv)] {
                    if wgt <= 0.0 {
                        continue;
                    }
                    let (tx, ty) = (((u0 as i32) + ddx).clamp(0, layer_w - 1), ((v0 as i32) + ddy).clamp(0, layer_h - 1));
                    let i = ((ty * layer_w + tx) * 4) as usize;
                    let a = ldata[i + 3] as f32 / 255.0 * wgt;
                    acc[0] += ldata[i] as f32 * a;
                    acc[1] += ldata[i + 1] as f32 * a;
                    acc[2] += ldata[i + 2] as f32 * a;
                    acc[3] += a;
                }
                if acc[3] <= 0.0 {
                    continue;
                }
                sr = acc[0] / acc[3];
                sg = acc[1] / acc[3];
                sb = acc[2] / acc[3];
                sa = acc[3];
            }
            let a = sa * alpha;
            if a <= 0.0 {
                continue;
            }
            let o = &mut row[px as usize * 4..px as usize * 4 + 4];
            let inv = 1.0 - a;
            o[0] = (sr * dim * a + o[0] as f32 * inv).round().min(255.0) as u8;
            o[1] = (sg * dim * a + o[1] as f32 * inv).round().min(255.0) as u8;
            o[2] = (sb * dim * a + o[2] as f32 * inv).round().min(255.0) as u8;
        }
    });
}
