//! Keyframes, interpolation and animation presets.
//! Keyframe times are relative to the start of their clip, in microseconds.
use crate::project::model::{Clip, Content};
use crate::time::Us;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Interp {
    #[default]
    Linear,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl Interp {
    pub const ALL: [Interp; 4] = [Interp::Linear, Interp::EaseIn, Interp::EaseOut, Interp::EaseInOut];
    pub fn label(self) -> &'static str {
        match self {
            Interp::Linear => "Linear",
            Interp::EaseIn => "Ease in",
            Interp::EaseOut => "Ease out",
            Interp::EaseInOut => "Ease in-out",
        }
    }
    /// Map linear progress p in 0..=1 to eased progress.
    pub fn apply(self, p: f32) -> f32 {
        let p = p.clamp(0.0, 1.0);
        match self {
            Interp::Linear => p,
            Interp::EaseIn => p * p,
            Interp::EaseOut => 1.0 - (1.0 - p) * (1.0 - p),
            Interp::EaseInOut => p * p * (3.0 - 2.0 * p),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Prop {
    X,
    Y,
    Scale,
    Rotation,
    Opacity,
    Volume,
}
impl Prop {
    pub const ALL: [Prop; 6] = [Prop::X, Prop::Y, Prop::Scale, Prop::Rotation, Prop::Opacity, Prop::Volume];
    pub fn label(self) -> &'static str {
        match self {
            Prop::X => "Position X",
            Prop::Y => "Position Y",
            Prop::Scale => "Scale",
            Prop::Rotation => "Rotation",
            Prop::Opacity => "Opacity",
            Prop::Volume => "Volume",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Keyframe {
    pub t: Us,
    pub v: f32,
    /// Easing used for the segment that STARTS at this keyframe.
    #[serde(default)]
    pub interp: Interp,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct AnimTrack {
    pub prop: Prop,
    pub keys: Vec<Keyframe>,
}

/// Evaluate a sorted key list at clip-local time `t`.
pub fn eval_keys(keys: &[Keyframe], t: Us) -> Option<f32> {
    let first = keys.first()?;
    if t <= first.t {
        return Some(first.v);
    }
    let last = keys.last().unwrap();
    if t >= last.t {
        return Some(last.v);
    }
    let i = keys.partition_point(|k| k.t <= t) - 1;
    let (a, b) = (keys[i], keys[i + 1]);
    let p = (t - a.t) as f32 / (b.t - a.t).max(1) as f32;
    Some(a.v + (b.v - a.v) * a.interp.apply(p))
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sampled {
    pub x: f32,
    pub y: f32,
    pub scale: f32,
    pub rotation: f32,
    pub opacity: f32,
    pub volume: f32,
}

impl Clip {
    pub fn anim(&self, prop: Prop) -> Option<&AnimTrack> {
        self.keyframes.iter().find(|a| a.prop == prop)
    }
    pub fn base_value(&self, prop: Prop) -> f32 {
        match prop {
            Prop::X => self.x,
            Prop::Y => self.y,
            Prop::Scale => self.scale,
            Prop::Rotation => self.rotation,
            Prop::Opacity => self.opacity,
            Prop::Volume => self.volume,
        }
    }
    pub fn value_at(&self, prop: Prop, t_local: Us) -> f32 {
        self.anim(prop)
            .and_then(|a| eval_keys(&a.keys, t_local))
            .unwrap_or_else(|| self.base_value(prop))
    }
    pub fn sample(&self, t_local: Us) -> Sampled {
        Sampled {
            x: self.value_at(Prop::X, t_local),
            y: self.value_at(Prop::Y, t_local),
            scale: self.value_at(Prop::Scale, t_local),
            rotation: self.value_at(Prop::Rotation, t_local),
            opacity: self.value_at(Prop::Opacity, t_local),
            volume: self.value_at(Prop::Volume, t_local),
        }
    }
    /// Add or replace a keyframe (same time = replace). Keeps keys sorted.
    pub fn set_key(&mut self, prop: Prop, t: Us, v: f32, interp: Interp) {
        let t = t.clamp(0, self.duration);
        let track = match self.keyframes.iter().position(|a| a.prop == prop) {
            Some(i) => &mut self.keyframes[i],
            None => {
                self.keyframes.push(AnimTrack { prop, keys: vec![] });
                self.keyframes.last_mut().unwrap()
            }
        };
        match track.keys.binary_search_by(|k| k.t.cmp(&t)) {
            Ok(i) => track.keys[i] = Keyframe { t, v, interp },
            Err(i) => track.keys.insert(i, Keyframe { t, v, interp }),
        }
    }
    pub fn remove_key(&mut self, prop: Prop, t: Us) -> bool {
        let Some(i) = self.keyframes.iter().position(|a| a.prop == prop) else { return false };
        let before = self.keyframes[i].keys.len();
        self.keyframes[i].keys.retain(|k| k.t != t);
        let removed = self.keyframes[i].keys.len() != before;
        if self.keyframes[i].keys.is_empty() {
            self.keyframes.remove(i);
        }
        removed
    }
    pub fn clear_keys(&mut self, prop: Prop) {
        self.keyframes.retain(|a| a.prop != prop);
    }
    /// Nearest keyframe time (of any property) strictly after/before `t`.
    pub fn next_key_time(&self, t: Us) -> Option<Us> {
        self.keyframes.iter().flat_map(|a| a.keys.iter().map(|k| k.t)).filter(|&k| k > t).min()
    }
    pub fn prev_key_time(&self, t: Us) -> Option<Us> {
        self.keyframes.iter().flat_map(|a| a.keys.iter().map(|k| k.t)).filter(|&k| k < t).max()
    }
    /// Split keyframes at clip-local time `at` into (left, right) sets.
    /// Boundary values are inserted so both halves keep the exact motion.
    pub fn split_keys(&self, at: Us) -> (Vec<AnimTrack>, Vec<AnimTrack>) {
        let mut left = vec![];
        let mut right = vec![];
        for a in &self.keyframes {
            let v_at = eval_keys(&a.keys, at).unwrap();
            let mut l: Vec<Keyframe> = a.keys.iter().copied().filter(|k| k.t < at).collect();
            // Interp of the segment crossing the cut = interp of the key before it.
            let crossing = a.keys.iter().rev().find(|k| k.t <= at).map(|k| k.interp).unwrap_or_default();
            // Re-sampling an eased segment across a cut cannot be exact with
            // piecewise easing, so both halves use the boundary value with the
            // original easing; exact for linear, close for eased segments.
            let has_after = a.keys.iter().any(|k| k.t >= at);
            if has_after || !l.is_empty() {
                l.push(Keyframe { t: at, v: v_at, interp: crossing });
            }
            let mut r: Vec<Keyframe> = vec![Keyframe { t: 0, v: v_at, interp: crossing }];
            r.extend(a.keys.iter().filter(|k| k.t > at).map(|k| Keyframe { t: k.t - at, ..*k }));
            if !l.is_empty() {
                left.push(AnimTrack { prop: a.prop, keys: l });
            }
            right.push(AnimTrack { prop: a.prop, keys: r });
        }
        (left, right)
    }
}

// ---------------------------------------------------------------- presets

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Preset {
    FadeIn,
    FadeOut,
    SlideIn(Direction),
    SlideOut(Direction),
    ScaleIn,
    ScaleOut,
    Pop,
    Spin,
    LogoReveal,
    /// Characters revealed per second; text clips only.
    Typewriter(f32),
}

impl Preset {
    pub fn label(&self) -> &'static str {
        match self {
            Preset::FadeIn => "Fade in",
            Preset::FadeOut => "Fade out",
            Preset::SlideIn(_) => "Slide in",
            Preset::SlideOut(_) => "Slide out",
            Preset::ScaleIn => "Scale in",
            Preset::ScaleOut => "Scale out",
            Preset::Pop => "Pop",
            Preset::Spin => "Spin",
            Preset::LogoReveal => "Logo reveal",
            Preset::Typewriter(_) => "Typewriter",
        }
    }
}

fn dir_offset(d: Direction) -> (f32, f32) {
    // Distance in canvas fractions; 1.0 puts the layer fully off-canvas.
    match d {
        Direction::Left => (-1.0, 0.0),
        Direction::Right => (1.0, 0.0),
        Direction::Up => (0.0, -1.0),
        Direction::Down => (0.0, 1.0),
    }
}

/// Apply a preset as ordinary, editable keyframes over `dur` microseconds.
/// "In" presets run from the clip start, "out" presets end at the clip end.
pub fn apply_preset(clip: &mut Clip, preset: Preset, dur: Us) {
    let d = dur.clamp(1, clip.duration.max(1));
    let end = clip.duration;
    let (bx, by, bs, bo, br) = (clip.x, clip.y, clip.scale, clip.opacity, clip.rotation);
    use Interp::*;
    match preset {
        Preset::FadeIn => {
            clip.set_key(Prop::Opacity, 0, 0.0, Linear);
            clip.set_key(Prop::Opacity, d, bo, Linear);
        }
        Preset::FadeOut => {
            clip.set_key(Prop::Opacity, end - d, bo, Linear);
            clip.set_key(Prop::Opacity, end, 0.0, Linear);
        }
        Preset::SlideIn(dir) => {
            let (dx, dy) = dir_offset(dir);
            clip.set_key(Prop::X, 0, bx + dx, EaseOut);
            clip.set_key(Prop::X, d, bx, Linear);
            clip.set_key(Prop::Y, 0, by + dy, EaseOut);
            clip.set_key(Prop::Y, d, by, Linear);
        }
        Preset::SlideOut(dir) => {
            let (dx, dy) = dir_offset(dir);
            clip.set_key(Prop::X, end - d, bx, EaseIn);
            clip.set_key(Prop::X, end, bx + dx, Linear);
            clip.set_key(Prop::Y, end - d, by, EaseIn);
            clip.set_key(Prop::Y, end, by + dy, Linear);
        }
        Preset::ScaleIn => {
            clip.set_key(Prop::Scale, 0, 0.0, EaseOut);
            clip.set_key(Prop::Scale, d, bs, Linear);
        }
        Preset::ScaleOut => {
            clip.set_key(Prop::Scale, end - d, bs, EaseIn);
            clip.set_key(Prop::Scale, end, 0.0, Linear);
        }
        Preset::Pop => {
            clip.set_key(Prop::Scale, 0, 0.0, EaseOut);
            clip.set_key(Prop::Scale, d * 6 / 10, bs * 1.15, EaseInOut);
            clip.set_key(Prop::Scale, d, bs, Linear);
        }
        Preset::Spin => {
            clip.set_key(Prop::Rotation, 0, br, Linear);
            clip.set_key(Prop::Rotation, d, br + 360.0, Linear);
        }
        Preset::LogoReveal => {
            clip.set_key(Prop::Opacity, 0, 0.0, EaseOut);
            clip.set_key(Prop::Opacity, d, bo, Linear);
            clip.set_key(Prop::Scale, 0, bs * 0.6, EaseOut);
            clip.set_key(Prop::Scale, d, bs, Linear);
        }
        Preset::Typewriter(cps) => {
            if let Content::Text(t) = &mut clip.content {
                t.typewriter_cps = Some(cps.max(0.5));
            }
        }
    }
}
