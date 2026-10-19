//! Timeline: tracks, clips, ruler, playhead, drag/trim/seek interaction.
//! Painting only reads the project; edits are collected and applied afterwards.
use crate::util::*;
use crate::App;
use egui::{pos2, vec2, Align2, Color32, FontId, Rect, Sense, Shape, Stroke};
use litemotion_engine::project::model::*;
use litemotion_engine::time::{secs, to_secs, Us};

const HEADER_W: f32 = 118.0;
const RULER_H: f32 = 24.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Zone {
    Body,
    TrimStart,
    TrimEnd,
}

#[derive(Clone, Debug)]
pub struct Drag {
    pub clip: u64,
    pub zone: Zone,
    pub grab_us: Us,
    pub orig_start: Us,
    pub seeking: bool,
}

#[derive(Clone, Copy)]
enum Header {
    Hide(u64),
    Mute(u64),
    Remove(u64),
}

impl App {
    pub fn track_height(&self) -> f32 {
        if self.compact {
            56.0
        } else {
            46.0
        }
    }

    pub fn timeline_toolbar(&mut self, ui: &mut egui::Ui) {
        let ph = self.playhead;
        let Some((has_sel, can_split, can_paste)) = self.ed.as_ref().map(|ed| {
            let split = ed.selection.iter().any(|id| ed.project.clip(*id).map_or(false, |c| ph > c.start && ph < c.end()));
            (!ed.selection.is_empty(), split, !ed.clipboard.is_empty())
        }) else {
            return;
        };
        let big = if self.compact { vec2(0.0, 34.0) } else { vec2(0.0, 22.0) };
        let (mut split, mut delete, mut dup, mut copy, mut paste) = (false, false, false, false, false);
        let mut add_track: Option<TrackKind> = None;
        ui.horizontal_wrapped(|ui| {
            split = ui.add_enabled(can_split, egui::Button::new("Split").min_size(big)).on_hover_text("Cut the selected clip at the playhead (S)").clicked();
            delete = ui.add_enabled(has_sel, egui::Button::new("Delete").min_size(big)).on_hover_text("Remove the selected clips (Delete). You can undo this.").clicked();
            dup = ui.add_enabled(has_sel, egui::Button::new("Duplicate").min_size(big)).on_hover_text("Copy right after the original (Ctrl+D)").clicked();
            copy = ui.add_enabled(has_sel, egui::Button::new("Copy").min_size(big)).on_hover_text("Ctrl+C").clicked();
            paste = ui.add_enabled(can_paste, egui::Button::new("Paste").min_size(big)).on_hover_text("Paste at the playhead (Ctrl+V)").clicked();
            ui.separator();
            ui.checkbox(&mut self.snap, "Snap").on_hover_text("Clip edges stick to the playhead and other clips");
            ui.label("Zoom");
            ui.add(egui::Slider::new(&mut self.zoom, 8.0..=600.0).logarithmic(true).show_value(false));
            ui.menu_button("+ Track", |ui| {
                for k in [TrackKind::Video, TrackKind::Overlay, TrackKind::Text, TrackKind::Audio] {
                    if ui.button(k.label()).clicked() {
                        add_track = Some(k);
                        ui.close_menu();
                    }
                }
            });
        });
        if split {
            if let Err(e) = self.action_split() {
                self.error(e.to_string());
            }
        }
        if delete {
            self.action_delete();
        }
        if dup {
            self.action_duplicate();
        }
        if copy {
            if let Some(ed) = self.ed.as_mut() {
                ed.copy_selection();
            }
        }
        if paste {
            self.action_paste();
        }
        if let (Some(k), Some(ed)) = (add_track, self.ed.as_mut()) {
            let n = ed.project.tracks.iter().filter(|t| t.kind == k).count() + 1;
            let _ = ed.edit("Add track", |p| {
                p.add_track(k, &format!("{} {n}", k.label()));
                Ok(())
            });
        }
    }

    pub fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        self.timeline_toolbar(ui);
        let th = self.track_height();
        let Some(n) = self.ed.as_ref().map(|e| e.project.tracks.len()) else { return };
        let total_h = RULER_H + th * n.max(1) as f32 + 8.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), total_h.max(ui.available_height())), Sense::click_and_drag());
            self.timeline_body(ui, rect, resp, th);
        });
    }

    fn timeline_body(&mut self, ui: &mut egui::Ui, rect: Rect, resp: egui::Response, th: f32) {
        let lanes = Rect::from_min_max(pos2(rect.left() + HEADER_W, rect.top()), rect.right_bottom());
        let dark = ui.visuals().dark_mode;

        // ---- zoom (pinch / Ctrl+wheel) and scroll
        if resp.hovered() {
            let z = ui.input(|i| i.zoom_delta());
            if z != 1.0 {
                let anchor = ui.input(|i| i.pointer.hover_pos()).map_or(lanes.left(), |p| p.x);
                let t_at = (anchor - lanes.left() + self.scroll_x) / self.zoom;
                self.zoom = (self.zoom * z).clamp(8.0, 600.0);
                self.scroll_x = (t_at * self.zoom - (anchor - lanes.left())).max(0.0);
            }
            let d = ui.input(|i| i.smooth_scroll_delta);
            self.scroll_x = (self.scroll_x - d.x - if ui.input(|i| i.modifiers.shift) { d.y } else { 0.0 }).max(0.0);
        }
        if self.playing {
            let x = to_secs(self.playhead) as f32 * self.zoom;
            if x - self.scroll_x > lanes.width() * 0.92 || x < self.scroll_x {
                self.scroll_x = (x - lanes.width() * 0.1).max(0.0);
            }
        }
        let (zoom, sx, ph) = (self.zoom, self.scroll_x, self.playhead);
        let x_of = |t: Us| lanes.left() + to_secs(t) as f32 * zoom - sx;
        let t_of = |x: f32| secs(((x - lanes.left() + sx) / zoom).max(0.0) as f64);
        let top = rect.top() + RULER_H;

        // ---- paint (read-only)
        let mut hit_boxes: Vec<(Rect, u64, usize)> = vec![];
        let mut headers: Vec<Header> = vec![];
        {
            let Some(ed) = self.ed.as_ref() else { return };
            let p = &ed.project;
            let painter = ui.painter_at(rect);
            let lane_painter = ui.painter_at(lanes);
            let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
            painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);

            // ruler
            let steps = [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0];
            let step = steps.iter().copied().find(|s| s * zoom as f64 >= 70.0).unwrap_or(600.0);
            let first = (sx as f64 / zoom as f64 / step).floor() as i64;
            let last = ((sx + lanes.width()) as f64 / zoom as f64 / step).ceil() as i64;
            for i in first.max(0)..=last {
                let t = i as f64 * step;
                let x = lanes.left() + t as f32 * zoom - sx;
                lane_painter.line_segment([pos2(x, rect.top() + RULER_H - 7.0), pos2(x, rect.top() + RULER_H)], stroke);
                lane_painter.text(pos2(x + 3.0, rect.top() + 3.0), Align2::LEFT_TOP, fmt_secs(t as f32), FontId::proportional(11.0), ui.visuals().weak_text_color());
            }
            painter.line_segment([pos2(rect.left(), top), pos2(rect.right(), top)], stroke);

            for (ti, track) in p.tracks.iter().enumerate() {
                let y0 = top + ti as f32 * th;
                let row = Rect::from_min_max(pos2(lanes.left(), y0), pos2(rect.right(), y0 + th));
                if ti % 2 == 1 {
                    lane_painter.rect_filled(row, 0.0, Color32::from_white_alpha(if dark { 6 } else { 0 }));
                }
                painter.line_segment([pos2(rect.left(), y0 + th), pos2(rect.right(), y0 + th)], stroke);
                // header
                let hr = Rect::from_min_max(pos2(rect.left(), y0), pos2(lanes.left(), y0 + th));
                painter.rect_filled(hr, 0.0, ui.visuals().panel_fill);
                painter.text(hr.left_top() + vec2(6.0, 4.0), Align2::LEFT_TOP, &track.name, FontId::proportional(12.0), ui.visuals().text_color());
                let bh = (th - 24.0).max(16.0);
                let mut x = hr.left() + 4.0;
                let mut put = |ui: &mut egui::Ui, label: &str, on: bool, tip: &str, w: f32| -> bool {
                    let r = Rect::from_min_size(pos2(x, hr.top() + 21.0), vec2(w, bh));
                    x += w + 3.0;
                    ui.put(r, egui::SelectableLabel::new(on, label)).on_hover_text(tip).clicked()
                };
                if track.kind.is_visual() && put(ui, "Hide", !track.visible, "Hide this track in the preview and export", 38.0) {
                    headers.push(Header::Hide(track.id));
                }
                if put(ui, "Mute", track.muted, "Silence this track", 44.0) {
                    headers.push(Header::Mute(track.id));
                }
                if put(ui, "x", false, "Remove this track", 18.0) {
                    headers.push(Header::Remove(track.id));
                }

                // clips
                for c in &track.clips {
                    let (x0, x1) = (x_of(c.start), x_of(c.end()));
                    if x1 < lanes.left() - 2.0 || x0 > lanes.right() + 2.0 {
                        continue;
                    }
                    let r = Rect::from_min_max(pos2(x0, y0 + 3.0), pos2(x1.max(x0 + 3.0), y0 + th - 3.0));
                    hit_boxes.push((r, c.id, ti));
                    let selected = ed.selection.contains(&c.id);
                    let mut col = clip_color(p, c, dark);
                    if (!track.visible && track.kind.is_visual()) || (track.muted && !track.kind.is_visual()) {
                        col = col.gamma_multiply(0.4);
                    }
                    lane_painter.rect_filled(r, 4.0, col);
                    let mut label_dx = 5.0;
                    if let Content::Media { media_id } = c.content {
                        if let Some(tex) = self.thumbs.get(&media_id) {
                            let ts = tex.size_vec2();
                            let h = r.height() - 6.0;
                            let w = (ts.x / ts.y * h).min(r.width() - 6.0);
                            if w > 8.0 {
                                lane_painter.image(tex.id(), Rect::from_min_size(r.left_top() + vec2(3.0, 3.0), vec2(w, h)), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                                label_dx = w + 9.0;
                            }
                        }
                        if track.kind == TrackKind::Audio {
                            if let (Some(wave), Some(m)) = (self.waves.get(&media_id), p.media(media_id)) {
                                let n = wave.len().max(1);
                                let (mid, amp) = (r.center().y, (r.height() - 8.0) / 2.0);
                                let mut px = r.left().max(lanes.left());
                                while px < r.right().min(lanes.right()) {
                                    let tl = to_secs(c.source_in) + ((px - r.left()) / zoom) as f64 * c.speed as f64;
                                    let idx = ((tl / to_secs(m.duration_us.max(1))) * n as f64) as usize;
                                    let v = (wave.get(idx.min(n - 1)).copied().unwrap_or(0.0) * 4.0).min(1.0);
                                    lane_painter.line_segment([pos2(px, mid - amp * v), pos2(px, mid + amp * v)], Stroke::new(1.0_f32, Color32::from_white_alpha(170)));
                                    px += 2.0;
                                }
                            }
                        }
                    }
                    for a in &c.keyframes {
                        for k in &a.keys {
                            let kx = x_of(c.start + k.t);
                            let ky = r.bottom() - 7.0;
                            let pts = vec![pos2(kx, ky - 4.0), pos2(kx + 4.0, ky), pos2(kx, ky + 4.0), pos2(kx - 4.0, ky)];
                            lane_painter.add(Shape::convex_polygon(pts, Color32::from_rgb(255, 214, 90), Stroke::new(1.0_f32, Color32::BLACK)));
                        }
                    }
                    lane_painter.with_clip_rect(r.intersect(lanes)).text(r.left_top() + vec2(label_dx, 3.0), Align2::LEFT_TOP, clip_label(p, c), FontId::proportional(11.0), Color32::WHITE);
                    if selected {
                        lane_painter.rect_stroke(r, 4.0, Stroke::new(2.0_f32, Color32::WHITE));
                        lane_painter.rect_filled(Rect::from_min_size(r.left_top(), vec2(4.0, r.height())), 2.0, ACCENT);
                        lane_painter.rect_filled(Rect::from_min_size(pos2(r.right() - 4.0, r.top()), vec2(4.0, r.height())), 2.0, ACCENT);
                    }
                }
            }
            // playhead
            let px = x_of(ph);
            if px >= lanes.left() && px <= lanes.right() {
                lane_painter.line_segment([pos2(px, rect.top()), pos2(px, rect.bottom())], Stroke::new(1.5_f32, Color32::from_rgb(255, 90, 90)));
                lane_painter.add(Shape::convex_polygon(vec![pos2(px - 6.0, rect.top()), pos2(px + 6.0, rect.top()), pos2(px, rect.top() + 10.0)], Color32::from_rgb(255, 90, 90), Stroke::NONE));
            }
        }

        // ---- header actions
        for h in headers {
            let Some(ed) = self.ed.as_mut() else { return };
            match h {
                Header::Hide(tid) | Header::Mute(tid) => {
                    let hide = matches!(h, Header::Hide(_));
                    let _ = ed.edit(if hide { "Show or hide track" } else { "Mute track" }, |p| {
                        if let Some(t) = p.tracks.iter_mut().find(|t| t.id == tid) {
                            if hide {
                                t.visible = !t.visible;
                            } else {
                                t.muted = !t.muted;
                            }
                        }
                        Ok(())
                    });
                }
                Header::Remove(tid) => {
                    if ed.project.tracks.iter().find(|t| t.id == tid).map_or(true, |t| t.clips.is_empty()) {
                        let _ = ed.edit("Remove track", |p| {
                            p.tracks.retain(|t| t.id != tid);
                            Ok(())
                        });
                    } else {
                        self.confirm_remove_track = Some(tid);
                    }
                }
            }
        }

        // ---- pointer interaction
        let pick = |pos: egui::Pos2, compact: bool| -> Option<(u64, Zone)> {
            let grab = if compact { 14.0 } else { 7.0 };
            hit_boxes.iter().rev().find(|(r, _, _)| r.expand2(vec2(2.0, 0.0)).contains(pos)).map(|(r, id, _)| {
                let z = if pos.x - r.left() < grab && r.width() > grab * 3.0 {
                    Zone::TrimStart
                } else if r.right() - pos.x < grab && r.width() > grab * 3.0 {
                    Zone::TrimEnd
                } else {
                    Zone::Body
                };
                (*id, z)
            })
        };
        let shift = ui.input(|i| i.modifiers.shift);
        let compact = self.compact;
        let in_ruler = |pos: egui::Pos2| pos.y < rect.top() + RULER_H && pos.x >= lanes.left();

        if resp.drag_started() {
            if let Some(pos) = ui.input(|i| i.pointer.press_origin()) {
                if in_ruler(pos) {
                    self.drag = Some(Drag { clip: 0, zone: Zone::Body, grab_us: 0, orig_start: 0, seeking: true });
                } else if pos.x >= lanes.left() {
                    if let Some((id, zone)) = pick(pos, compact) {
                        if let Some(ed) = self.ed.as_mut() {
                            if !ed.selection.contains(&id) {
                                if shift {
                                    ed.selection.push(id);
                                } else {
                                    ed.selection = vec![id];
                                }
                            }
                            let start = ed.project.clip(id).map(|c| c.start).unwrap_or(0);
                            self.drag = Some(Drag { clip: id, zone, grab_us: t_of(pos.x) - start, orig_start: start, seeking: false });
                        }
                    }
                }
            }
        }
        if resp.dragged() {
            if let (Some(d), Some(pos)) = (self.drag.clone(), resp.interact_pointer_pos()) {
                if d.seeking {
                    let dur = self.ed.as_ref().map_or(0, |e| e.project.duration());
                    self.seek(t_of(pos.x).clamp(0, dur.max(secs(1.0))));
                } else if let Some(ed) = self.ed.as_mut() {
                    let thr = secs((8.0 / zoom) as f64);
                    let snap = self.snap;
                    let id = d.clip;
                    let row = (((pos.y - top) / th).floor() as isize).clamp(0, ed.project.tracks.len() as isize - 1) as usize;
                    let target_track = ed.project.tracks.get(row).map(|t| t.id);
                    let cur_track = ed.project.find_clip(id).map(|(ti, _)| ed.project.tracks[ti].id);
                    let key = format!("drag{id}");
                    match d.zone {
                        Zone::Body => {
                            let mut desired = (t_of(pos.x) - d.grab_us).max(0);
                            if snap {
                                desired = ed.project.snap_move(id, desired, ph, thr);
                            }
                            let nt = if target_track != cur_track { target_track } else { None };
                            let _ = ed.edit_coalesced(&key, |p| p.move_clip(id, desired, nt));
                        }
                        Zone::TrimStart => {
                            let mut t = t_of(pos.x);
                            if snap {
                                t = ed.project.snap(t, &[id], ph, thr);
                            }
                            let _ = ed.edit_coalesced(&key, |p| p.trim_start(id, t));
                        }
                        Zone::TrimEnd => {
                            let mut t = t_of(pos.x);
                            if snap {
                                t = ed.project.snap(t, &[id], ph, thr);
                            }
                            let _ = ed.edit_coalesced(&key, |p| p.trim_end(id, t));
                        }
                    }
                }
            }
        }
        if resp.drag_stopped() {
            if let Some(ed) = self.ed.as_mut() {
                ed.end_coalesce();
            }
            self.drag = None;
        }
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                if in_ruler(pos) {
                    self.seek(t_of(pos.x));
                } else if pos.x >= lanes.left() {
                    let hit = pick(pos, compact);
                    if let Some(ed) = self.ed.as_mut() {
                        match hit {
                            Some((id, _)) if shift => {
                                if let Some(i) = ed.selection.iter().position(|s| *s == id) {
                                    ed.selection.remove(i);
                                } else {
                                    ed.selection.push(id);
                                }
                            }
                            Some((id, _)) => ed.selection = vec![id],
                            None => ed.selection.clear(),
                        }
                    }
                }
            }
        }
        if let Some(pos) = resp.hover_pos() {
            if let Some((_, z)) = pick(pos, compact) {
                ui.ctx().set_cursor_icon(if z != Zone::Body { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::Grab });
            }
        }
    }
}
