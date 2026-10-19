//! Pure timeline editing operations on a `Project`. No UI, no I/O.
use crate::animation::AnimTrack;
use crate::error::{EngineError, Result};
use crate::project::model::*;
use crate::time::Us;

pub mod editor;

fn err<T>(m: impl Into<String>) -> Result<T> {
    Err(EngineError::Edit(m.into()))
}

/// Shortest clip we allow, so edits can't create invisible zero-length clips.
pub const MIN_CLIP_US: Us = 50_000;

fn kind_accepts(track: TrackKind, content: &Content, project: &Project) -> bool {
    match content {
        Content::Media { media_id } => match project.media(*media_id).map(|m| m.kind) {
            Some(MediaKind::Audio) => track == TrackKind::Audio,
            Some(MediaKind::Video) => matches!(track, TrackKind::Video | TrackKind::Overlay | TrackKind::Audio),
            Some(MediaKind::Image) => matches!(track, TrackKind::Video | TrackKind::Overlay),
            None => false,
        },
        Content::Text(_) => track == TrackKind::Text,
        Content::Shape { .. } | Content::Solid { .. } => matches!(track, TrackKind::Overlay | TrackKind::Video),
    }
}

/// May `later` overlap `earlier`? Only through a crossfade/dissolve at least
/// as long as the overlap.
fn overlap_allowed(earlier: &Clip, later: &Clip) -> bool {
    let ov = earlier.end() - later.start;
    if ov <= 0 {
        return true;
    }
    matches!(
        later.transition_in,
        Some(Transition { kind: TransitionKind::Crossfade | TransitionKind::Dissolve, duration }) if duration >= ov
    ) && ov < earlier.duration
        && ov < later.duration
}

/// Would `clip` collide with other clips on `track` (ignoring `ignore_id`)?
pub fn collides(track: &Track, clip: &Clip, ignore_id: Option<u64>) -> bool {
    track.clips.iter().filter(|c| Some(c.id) != ignore_id).any(|o| {
        let disjoint = clip.end() <= o.start || clip.start >= o.end();
        if disjoint {
            return false;
        }
        let ok = if o.start <= clip.start { overlap_allowed(o, clip) } else { overlap_allowed(clip, o) };
        !ok
    })
}

fn sort_track(t: &mut Track) {
    t.clips.sort_by_key(|c| (c.start, c.id));
}

/// First start time >= `from` where a clip of `duration` fits on the track.
pub fn first_free_start(track: &Track, from: Us, duration: Us) -> Us {
    let mut start = from.max(0);
    let mut clips: Vec<&Clip> = track.clips.iter().collect();
    clips.sort_by_key(|c| c.start);
    for c in clips {
        if c.end() <= start {
            continue;
        }
        if c.start >= start + duration {
            break;
        }
        start = c.end();
    }
    start
}

impl Project {
    pub fn track_index(&self, id: u64) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    /// Build a media clip from an imported media entry.
    pub fn media_clip(&mut self, media_id: u64, start: Us) -> Result<Clip> {
        let m = self.media(media_id).ok_or_else(|| EngineError::Edit("unknown media".into()))?.clone();
        let dur = match m.kind {
            MediaKind::Image => 5_000_000,
            _ => m.duration_us.max(MIN_CLIP_US),
        };
        let id = self.alloc_id();
        Ok(Clip::new(id, Content::Media { media_id }, start, dur))
    }

    /// Add `clip` to `track_id`. If it doesn't fit at `clip.start` and
    /// `auto_place` is set it's moved to the next free slot.
    pub fn add_clip(&mut self, track_id: u64, mut clip: Clip, auto_place: bool) -> Result<u64> {
        let ti = self.track_index(track_id).ok_or_else(|| EngineError::Edit("no such track".into()))?;
        if !kind_accepts(self.tracks[ti].kind, &clip.content, self) {
            return err(format!("this item can't go on a {} track", self.tracks[ti].kind.label()));
        }
        if clip.duration < MIN_CLIP_US {
            return err("clip is too short");
        }
        if collides(&self.tracks[ti], &clip, None) {
            if auto_place {
                clip.start = first_free_start(&self.tracks[ti], clip.start, clip.duration);
            } else {
                return err("there's already a clip there");
            }
        }
        let id = clip.id;
        self.tracks[ti].clips.push(clip);
        sort_track(&mut self.tracks[ti]);
        Ok(id)
    }

    pub fn delete_clip(&mut self, id: u64) -> Result<()> {
        let (t, c) = self.find_clip(id).ok_or_else(|| EngineError::Edit("no such clip".into()))?;
        self.tracks[t].clips.remove(c);
        Ok(())
    }

    /// Move a clip to a new start (and optionally another track).
    pub fn move_clip(&mut self, id: u64, new_start: Us, new_track: Option<u64>) -> Result<()> {
        let (ti, ci) = self.find_clip(id).ok_or_else(|| EngineError::Edit("no such clip".into()))?;
        let mut clip = self.tracks[ti].clips[ci].clone();
        clip.start = new_start.max(0);
        let dest = match new_track {
            Some(t) => self.track_index(t).ok_or_else(|| EngineError::Edit("no such track".into()))?,
            None => ti,
        };
        if dest != ti && !kind_accepts(self.tracks[dest].kind, &clip.content, self) {
            return err(format!("this item can't go on a {} track", self.tracks[dest].kind.label()));
        }
        if collides(&self.tracks[dest], &clip, Some(id)) {
            return err("there's already a clip there");
        }
        self.tracks[ti].clips.remove(ci);
        self.tracks[dest].clips.push(clip);
        sort_track(&mut self.tracks[dest]);
        Ok(())
    }

    /// Maximum timeline duration a clip may have given its source media.
    fn max_duration(&self, c: &Clip) -> Option<Us> {
        let m = self.media(c.media_id()?)?;
        if m.kind == MediaKind::Image {
            return None;
        }
        Some(((m.duration_us - c.source_in).max(0) as f64 / c.speed as f64) as Us)
    }

    /// Trim the clip's start edge to timeline time `t` (non-destructive).
    pub fn trim_start(&mut self, id: u64, t: Us) -> Result<()> {
        let (ti, ci) = self.find_clip(id).ok_or_else(|| EngineError::Edit("no such clip".into()))?;
        let mut c = self.tracks[ti].clips[ci].clone();
        let end = c.end();
        let mut t = t.clamp(0, end - MIN_CLIP_US);
        // For media, can't extend before the beginning of the source.
        if c.media_id().is_some() && self.max_duration(&c).is_some() {
            let delta = t - c.start;
            let new_in = c.source_in + (delta as f64 * c.speed as f64).round() as Us;
            if new_in < 0 {
                t = c.start - (c.source_in as f64 / c.speed as f64).round() as Us;
            }
        }
        let delta = t - c.start;
        c.source_in = (c.source_in + (delta as f64 * c.speed as f64).round() as Us).max(0);
        c.start = t;
        c.duration = end - t;
        // Keyframes are clip-relative: shift so motion stays pinned to content.
        shift_keys(&mut c.keyframes, -delta, c.duration);
        if collides(&self.tracks[ti], &c, Some(id)) {
            return err("trimming would overlap the neighbouring clip");
        }
        self.tracks[ti].clips[ci] = c;
        sort_track(&mut self.tracks[ti]);
        Ok(())
    }

    /// Trim the clip's end edge to timeline time `t`.
    pub fn trim_end(&mut self, id: u64, t: Us) -> Result<()> {
        let (ti, ci) = self.find_clip(id).ok_or_else(|| EngineError::Edit("no such clip".into()))?;
        let mut c = self.tracks[ti].clips[ci].clone();
        let mut dur = (t - c.start).max(MIN_CLIP_US);
        if let Some(max) = self.max_duration(&c) {
            dur = dur.min(max.max(MIN_CLIP_US));
        }
        c.duration = dur;
        c.fade_in = c.fade_in.min(dur);
        c.fade_out = c.fade_out.min(dur);
        for a in &mut c.keyframes {
            a.keys.retain(|k| k.t <= dur);
        }
        c.keyframes.retain(|a| !a.keys.is_empty());
        if collides(&self.tracks[ti], &c, Some(id)) {
            return err("that would overlap the next clip");
        }
        self.tracks[ti].clips[ci] = c;
        Ok(())
    }

    /// Split at timeline time `at`; returns the id of the new right-hand clip.
    pub fn split_clip(&mut self, id: u64, at: Us) -> Result<u64> {
        let (ti, ci) = self.find_clip(id).ok_or_else(|| EngineError::Edit("no such clip".into()))?;
        let c = self.tracks[ti].clips[ci].clone();
        let local = at - c.start;
        if local < MIN_CLIP_US || c.duration - local < MIN_CLIP_US {
            return err("move the playhead inside the clip to split it");
        }
        let (lk, rk) = c.split_keys(local);
        let rid = self.alloc_id();
        let mut right = c.clone();
        right.id = rid;
        right.start = at;
        right.duration = c.duration - local;
        right.source_in = c.source_in + (local as f64 * c.speed as f64).round() as Us;
        right.keyframes = rk;
        right.fade_in = 0;
        right.transition_in = None;
        let mut left = c;
        left.duration = local;
        left.keyframes = lk;
        left.fade_out = 0;
        left.transition_out = None;
        if let Content::Text(t) = &mut right.content {
            // Typewriter reveal restarts on the right half only if it was
            // already finished; keep it simple and leave settings untouched.
            let _ = t;
        }
        self.tracks[ti].clips[ci] = left;
        self.tracks[ti].clips.push(right);
        sort_track(&mut self.tracks[ti]);
        Ok(rid)
    }

    /// Duplicate right after the original (or the next free slot).
    pub fn duplicate_clip(&mut self, id: u64) -> Result<u64> {
        let (ti, ci) = self.find_clip(id).ok_or_else(|| EngineError::Edit("no such clip".into()))?;
        let mut c = self.tracks[ti].clips[ci].clone();
        c.id = self.alloc_id();
        c.start = first_free_start(&self.tracks[ti], c.end(), c.duration);
        c.transition_in = None;
        c.transition_out = None;
        let nid = c.id;
        self.tracks[ti].clips.push(c);
        sort_track(&mut self.tracks[ti]);
        Ok(nid)
    }

    /// Change speed keeping the same source range (duration adapts).
    pub fn set_speed(&mut self, id: u64, speed: f32) -> Result<()> {
        if !(0.1..=8.0).contains(&speed) {
            return err("speed must be between 0.1x and 8x");
        }
        let (ti, ci) = self.find_clip(id).ok_or_else(|| EngineError::Edit("no such clip".into()))?;
        let mut c = self.tracks[ti].clips[ci].clone();
        let span = c.source_span();
        c.speed = speed;
        c.duration = ((span as f64 / speed as f64).round() as Us).max(MIN_CLIP_US);
        let ratio = 1.0; // keyframes stay at timeline positions
        let _ = ratio;
        c.fade_in = c.fade_in.min(c.duration);
        c.fade_out = c.fade_out.min(c.duration);
        for a in &mut c.keyframes {
            a.keys.retain(|k| k.t <= c.duration);
        }
        if collides(&self.tracks[ti], &c, Some(id)) {
            return err("that speed would overlap the next clip");
        }
        self.tracks[ti].clips[ci] = c;
        Ok(())
    }

    pub fn set_transition_in(&mut self, id: u64, t: Option<Transition>) -> Result<()> {
        if let Some(tr) = t {
            if !tr.kind.valid_in() {
                return err("that transition belongs at the end of a clip");
            }
        }
        let c = self.clip_mut(id).ok_or_else(|| EngineError::Edit("no such clip".into()))?;
        c.transition_in = t.map(|mut t| {
            t.duration = t.duration.clamp(MIN_CLIP_US, c.duration);
            t
        });
        Ok(())
    }
    pub fn set_transition_out(&mut self, id: u64, t: Option<Transition>) -> Result<()> {
        if let Some(tr) = t {
            if !tr.kind.valid_out() {
                return err("that transition belongs at the start of a clip");
            }
        }
        let c = self.clip_mut(id).ok_or_else(|| EngineError::Edit("no such clip".into()))?;
        c.transition_out = t.map(|mut t| {
            t.duration = t.duration.clamp(MIN_CLIP_US, c.duration);
            t
        });
        Ok(())
    }

    /// Paste copies of `clips` with the earliest one starting at `at`.
    pub fn paste_clips(&mut self, clips: &[(u64, Clip)], at: Us) -> Result<Vec<u64>> {
        let origin = clips.iter().map(|(_, c)| c.start).min().unwrap_or(0);
        let mut new_ids = vec![];
        for (track_id, c) in clips {
            let mut n = c.clone();
            n.id = self.alloc_id();
            n.start = at + (c.start - origin);
            n.transition_in = None;
            n.transition_out = None;
            let tid = if self.track_index(*track_id).is_some() {
                *track_id
            } else {
                return err("the original track no longer exists");
            };
            new_ids.push(self.add_clip(tid, n, true)?);
        }
        Ok(new_ids)
    }

    /// Snap `edge` to nearby boundaries (playhead, 0, other clip edges).
    pub fn snap(&self, edge: Us, exclude: &[u64], playhead: Us, threshold: Us) -> Us {
        let mut best = None::<(Us, Us)>;
        let mut consider = |p: Us| {
            let d = (p - edge).abs();
            if d <= threshold && best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, p));
            }
        };
        consider(0);
        consider(playhead);
        for t in &self.tracks {
            for c in t.clips.iter().filter(|c| !exclude.contains(&c.id)) {
                consider(c.start);
                consider(c.end());
            }
        }
        best.map(|(_, p)| p).unwrap_or(edge)
    }

    /// Snap a moving clip so either its start or its end lands on a boundary.
    pub fn snap_move(&self, id: u64, desired_start: Us, playhead: Us, threshold: Us) -> Us {
        let Some(c) = self.clip(id) else { return desired_start };
        let s = self.snap(desired_start, &[id], playhead, threshold);
        if s != desired_start {
            return s;
        }
        let e = self.snap(desired_start + c.duration, &[id], playhead, threshold);
        if e != desired_start + c.duration {
            return e - c.duration;
        }
        desired_start
    }

    /// A track of `kind` that is free for [start, start+dur); adds one if none is.
    pub fn track_with_room(&mut self, kind: TrackKind, start: Us, dur: Us) -> u64 {
        let probe = Clip::new(0, Content::Solid { color: [0; 4] }, start, dur);
        if let Some(t) = self.tracks.iter().find(|t| t.kind == kind && !collides(t, &probe, None)) {
            return t.id;
        }
        let n = self.tracks.iter().filter(|t| t.kind == kind).count() + 1;
        self.add_track(kind, &format!("{} {n}", kind.label()))
    }

    pub fn lower_third(&mut self, title: &str, subtitle: &str, start: Us, duration: Us) -> Result<Vec<u64>> {
        let overlay = self.track_with_room(TrackKind::Overlay, start, duration);
        let bar_id = self.alloc_id();
        let mut bar = Clip::new(
            bar_id,
            Content::Shape { shape: ShapeKind::Rectangle, color: [0, 0, 0, 170], w: 0.7, h: 0.16 },
            start,
            duration,
        );
        bar.x = -0.1;
        bar.y = 0.3;
        crate::animation::apply_preset(&mut bar, crate::animation::Preset::SlideIn(crate::animation::Direction::Left), 400_000);
        let b = self.add_clip(overlay, bar, true)?;
        let mut ids = vec![b];
        for (txt, size, dy, font) in [(title, 6.5, 0.27, FontId::SansBold), (subtitle, 4.0, 0.33, FontId::Sans)] {
            if txt.is_empty() {
                continue;
            }
            let tid = self.alloc_id();
            let mut t = Clip::new(
                tid,
                Content::Text(TextStyle { text: txt.into(), size_pct: size, font, align: Align::Left, shadow: false, ..TextStyle::default() }),
                start,
                duration,
            );
            t.x = -0.1;
            t.y = dy;
            crate::animation::apply_preset(&mut t, crate::animation::Preset::FadeIn, 500_000);
            let text = self.track_with_room(TrackKind::Text, start, duration);
            ids.push(self.add_clip(text, t, true)?);
        }
        Ok(ids)
    }
}

fn shift_keys(tracks: &mut Vec<AnimTrack>, delta: Us, new_dur: Us) {
    for a in tracks.iter_mut() {
        for k in a.keys.iter_mut() {
            k.t += delta;
        }
        // Keep the value at the new start by clamping earlier keys to t=0.
        let pre: Vec<_> = a.keys.iter().filter(|k| k.t < 0).copied().collect();
        if let Some(last_pre) = pre.last() {
            let v_at0 = crate::animation::eval_keys(&a.keys, 0).unwrap_or(last_pre.v);
            a.keys.retain(|k| k.t > 0);
            a.keys.insert(0, crate::animation::Keyframe { t: 0, v: v_at0, interp: last_pre.interp });
        }
        a.keys.retain(|k| k.t <= new_dur);
    }
    tracks.retain(|a| !a.keys.is_empty());
}
