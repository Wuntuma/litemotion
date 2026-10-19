//! Contextual properties panel: shows only what applies to the selected clip.
use crate::App;
use egui::{Color32, Slider, Ui};
use litemotion_engine::animation::*;
use litemotion_engine::project::model::*;
use litemotion_engine::time::{secs, to_secs, Us};
use litemotion_engine::timeline::editor::Editor;
use litemotion_engine::{EngineError, Result};
use std::ops::RangeInclusive;

type Err = Option<String>;

fn edit_clip(ed: &mut Editor, err: &mut Err, id: u64, key: &str, f: impl FnOnce(&mut Clip) -> Result<()>) {
    let r = ed.edit_coalesced(&format!("{key}{id}"), |p| {
        let c = p.clip_mut(id).ok_or_else(|| EngineError::Edit("no such clip".into()))?;
        f(c)
    });
    if let Err(e) = r {
        *err = Some(e.to_string());
    }
}

fn set_base(c: &mut Clip, prop: Prop, v: f32) {
    match prop {
        Prop::X => c.x = v,
        Prop::Y => c.y = v,
        Prop::Scale => c.scale = v,
        Prop::Rotation => c.rotation = v,
        Prop::Opacity => c.opacity = v,
        Prop::Volume => c.volume = v,
    }
}

fn color_edit(ui: &mut Ui, label: &str, c: &mut Color) -> bool {
    let mut col = Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
    let mut changed = false;
    ui.horizontal(|ui| {
        changed = egui::color_picker::color_edit_button_srgba(ui, &mut col, egui::color_picker::Alpha::OnlyBlend).changed();
        ui.label(label);
    });
    if changed {
        *c = col.to_srgba_unmultiplied();
    }
    changed
}

/// A slider that edits the base value, or a keyframe at the playhead if the
/// property is animated, plus a Key button to add/remove a keyframe.
#[allow(clippy::too_many_arguments)]
fn prop_row(ui: &mut Ui, ed: &mut Editor, err: &mut Err, clip: &Clip, ph: Us, interp: Interp, prop: Prop, label: &str, range: RangeInclusive<f32>) {
    let inside = ph >= clip.start && ph <= clip.end();
    let local = (ph - clip.start).clamp(0, clip.duration);
    let animated = clip.anim(prop).map_or(false, |a| !a.keys.is_empty());
    let has_key = clip.anim(prop).map_or(false, |a| a.keys.iter().any(|k| k.t == local));
    let mut v = clip.value_at(prop, local);
    ui.horizontal(|ui| {
        if ui.add(Slider::new(&mut v, range).text(label)).changed() {
            edit_clip(ed, err, clip.id, label, |c| {
                if animated && inside {
                    c.set_key(prop, local, v, interp);
                } else {
                    set_base(c, prop, v);
                }
                Ok(())
            });
        }
        let tip = if inside { "Add or remove a keyframe at the playhead" } else { "Move the playhead onto this clip to add keyframes" };
        if ui.add_enabled(inside, egui::SelectableLabel::new(has_key, "Key")).on_hover_text(tip).clicked() {
            edit_clip(ed, err, clip.id, "key", |c| {
                if has_key {
                    c.remove_key(prop, local);
                } else {
                    c.set_key(prop, local, v, interp);
                }
                Ok(())
            });
            ed.end_coalesce();
        }
    });
}

fn presets(is_text: bool) -> Vec<(String, Preset)> {
    let mut v = vec![("Fade in".to_string(), Preset::FadeIn), ("Fade out".into(), Preset::FadeOut)];
    for (n, d) in [("left", Direction::Left), ("right", Direction::Right), ("top", Direction::Up), ("bottom", Direction::Down)] {
        v.push((format!("Slide in from {n}"), Preset::SlideIn(d)));
    }
    for (n, d) in [("left", Direction::Left), ("right", Direction::Right), ("top", Direction::Up), ("bottom", Direction::Down)] {
        v.push((format!("Slide out to {n}"), Preset::SlideOut(d)));
    }
    v.extend([("Scale in".into(), Preset::ScaleIn), ("Scale out".into(), Preset::ScaleOut), ("Pop".into(), Preset::Pop), ("Spin".into(), Preset::Spin), ("Logo reveal".into(), Preset::LogoReveal)]);
    if is_text {
        v.push(("Typewriter".into(), Preset::Typewriter(12.0)));
    }
    v
}

impl App {
    pub fn props_ui(&mut self, ui: &mut Ui) {
        let ph = self.playhead;
        let interp = self.interp;
        let mut new_interp = interp;
        let mut err: Err = None;
        let mut jump: Option<Us> = None;
        {
            let Some(ed) = self.ed.as_mut() else { return };
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if ed.selection.len() != 1 {
                    ui.weak(if ed.selection.is_empty() { "Select a clip on the timeline to edit it." } else { "Several clips selected. Select one to change its properties." });
                    return;
                }
                let id = ed.selection[0];
                let Some(clip) = ed.project.clip(id).cloned() else { return };
                let media = clip.media_id().and_then(|m| ed.project.media(m).cloned());
                let kind = media.as_ref().map(|m| m.kind);
                let visual = !matches!(kind, Some(MediaKind::Audio));
                let has_audio = matches!(kind, Some(MediaKind::Audio)) || media.as_ref().map_or(false, |m| m.kind == MediaKind::Video && m.has_audio);
                let is_text = matches!(clip.content, Content::Text(_));

                ui.strong(match &clip.content {
                    Content::Text(_) => "Text",
                    Content::Shape { .. } => "Shape",
                    Content::Solid { .. } => "Background",
                    Content::Media { .. } => match kind {
                        Some(MediaKind::Audio) => "Audio",
                        Some(MediaKind::Image) => "Picture",
                        _ => "Video",
                    },
                });
                ui.weak(format!("Starts at {:.2}s, lasts {:.2}s", to_secs(clip.start), to_secs(clip.duration)));

                // ---- content
                match &clip.content {
                    Content::Text(t) => {
                        let mut t = t.clone();
                        let mut ch = ui.text_edit_multiline(&mut t.text).changed();
                        egui::ComboBox::from_label("Font").selected_text(t.font.label()).show_ui(ui, |ui| {
                            for f in FontId::ALL {
                                ch |= ui.selectable_value(&mut t.font, f, f.label()).changed();
                            }
                        });
                        ch |= ui.add(Slider::new(&mut t.size_pct, 2.0..=30.0).text("Size")).changed();
                        ch |= color_edit(ui, "Color", &mut t.color);
                        egui::ComboBox::from_label("Align").selected_text(format!("{:?}", t.align)).show_ui(ui, |ui| {
                            for a in [Align::Left, Align::Center, Align::Right] {
                                ch |= ui.selectable_value(&mut t.align, a, format!("{a:?}")).changed();
                            }
                        });
                        ch |= ui.checkbox(&mut t.outline, "Outline").changed();
                        ch |= ui.checkbox(&mut t.shadow, "Shadow").changed();
                        if ch {
                            edit_clip(ed, &mut err, id, "text", |c| {
                                c.content = Content::Text(t);
                                Ok(())
                            });
                        }
                    }
                    Content::Shape { shape, color, w, h } => {
                        let (mut col, mut w, mut h) = (*color, *w, *h);
                        let mut ch = color_edit(ui, "Color", &mut col);
                        ch |= ui.add(Slider::new(&mut w, 0.02..=1.5).text("Width")).changed();
                        ch |= ui.add(Slider::new(&mut h, 0.02..=1.5).text("Height")).changed();
                        if ch {
                            let shape = *shape;
                            edit_clip(ed, &mut err, id, "shape", |c| {
                                c.content = Content::Shape { shape, color: col, w, h };
                                Ok(())
                            });
                        }
                    }
                    Content::Solid { color } => {
                        let mut col = *color;
                        if color_edit(ui, "Color", &mut col) {
                            edit_clip(ed, &mut err, id, "solid", |c| {
                                c.content = Content::Solid { color: col };
                                Ok(())
                            });
                        }
                    }
                    Content::Media { .. } => {}
                }

                ui.separator();
                ui.strong("Move and size");
                if visual {
                    prop_row(ui, ed, &mut err, &clip, ph, interp, Prop::X, "Position left/right", -1.0..=1.0);
                    prop_row(ui, ed, &mut err, &clip, ph, interp, Prop::Y, "Position up/down", -1.0..=1.0);
                    prop_row(ui, ed, &mut err, &clip, ph, interp, Prop::Scale, "Size", 0.05..=4.0);
                    prop_row(ui, ed, &mut err, &clip, ph, interp, Prop::Rotation, "Rotation", -360.0..=360.0);
                    prop_row(ui, ed, &mut err, &clip, ph, interp, Prop::Opacity, "Opacity", 0.0..=1.0);
                }
                if has_audio {
                    prop_row(ui, ed, &mut err, &clip, ph, interp, Prop::Volume, "Volume", 0.0..=2.0);
                    let mut m = clip.muted;
                    if ui.checkbox(&mut m, "Mute").changed() {
                        edit_clip(ed, &mut err, id, "mute", |c| {
                            c.muted = m;
                            Ok(())
                        });
                    }
                    let max = to_secs(clip.duration) as f32;
                    let (mut fi, mut fo) = (to_secs(clip.fade_in) as f32, to_secs(clip.fade_out) as f32);
                    if ui.add(Slider::new(&mut fi, 0.0..=max).text("Fade in (s)")).changed() {
                        edit_clip(ed, &mut err, id, "fadein", |c| {
                            c.fade_in = secs(fi as f64).min(c.duration);
                            Ok(())
                        });
                    }
                    if ui.add(Slider::new(&mut fo, 0.0..=max).text("Fade out (s)")).changed() {
                        edit_clip(ed, &mut err, id, "fadeout", |c| {
                            c.fade_out = secs(fo as f64).min(c.duration);
                            Ok(())
                        });
                    }
                }
                if kind != Some(MediaKind::Image) && media.is_some() {
                    let mut sp = clip.speed;
                    if ui.add(Slider::new(&mut sp, 0.1..=8.0).logarithmic(true).text("Speed")).changed() {
                        if let Err(e) = ed.edit_coalesced(&format!("speed{id}"), |p| p.set_speed(id, sp)) {
                            err = Some(e.to_string());
                        }
                    }
                }

                // ---- animate
                ui.separator();
                ui.strong("Animate");
                ui.weak("Add a Key at one time, move the playhead, change a value, add another Key.");
                egui::ComboBox::from_label("Easing").selected_text(interp.label()).show_ui(ui, |ui| {
                    for i in [Interp::Linear, Interp::EaseIn, Interp::EaseOut, Interp::EaseInOut] {
                        ui.selectable_value(&mut new_interp, i, i.label());
                    }
                });
                ui.menu_button("Ready-made animation", |ui| {
                    for (label, p) in presets(is_text) {
                        if ui.button(label).clicked() {
                            let dur = secs(0.8);
                            if let Err(e) = ed.edit("Animation", |pr| {
                                let c = pr.clip_mut(id).ok_or_else(|| EngineError::Edit("no such clip".into()))?;
                                apply_preset(c, p, dur);
                                Ok(())
                            }) {
                                err = Some(e.to_string());
                            }
                            ui.close_menu();
                        }
                    }
                });
                let local = (ph - clip.start).clamp(0, clip.duration);
                ui.horizontal(|ui| {
                    if ui.button("Previous key").clicked() {
                        jump = clip.prev_key_time(local).map(|t| clip.start + t);
                    }
                    if ui.button("Next key").clicked() {
                        jump = clip.next_key_time(local).map(|t| clip.start + t);
                    }
                });
                let animated: Vec<Prop> = clip.keyframes.iter().filter(|a| !a.keys.is_empty()).map(|a| a.prop).collect();
                if !animated.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Animated:");
                        for p in animated {
                            if ui.small_button(format!("{} (clear)", p.label())).on_hover_text("Remove all keyframes for this property").clicked() {
                                edit_clip(ed, &mut err, id, "clearkeys", |c| {
                                    c.clear_keys(p);
                                    Ok(())
                                });
                                ed.end_coalesce();
                            }
                        }
                    });
                }

                // ---- more
                if visual {
                    egui::CollapsingHeader::new("Fade and transitions").show(ui, |ui| {
                        let max = to_secs(clip.duration) as f32;
                        let mut tdur = clip.transition_in.map(|t| to_secs(t.duration) as f32).unwrap_or(0.5).min(max.max(0.1));
                        ui.add(Slider::new(&mut tdur, 0.1..=2.0).text("Length (s)"));
                        let d = secs(tdur as f64).min(clip.duration / 2);
                        ui.horizontal_wrapped(|ui| {
                            ui.label("At start:");
                            for k in [TransitionKind::Crossfade, TransitionKind::Dissolve, TransitionKind::FadeFromBlack] {
                                if ui.selectable_label(clip.transition_in.map_or(false, |t| t.kind == k), k.label()).clicked() {
                                    if let Err(e) = ed.edit("Transition", |p| p.set_transition_in(id, Some(Transition { kind: k, duration: d }))) {
                                        err = Some(e.to_string());
                                    }
                                }
                            }
                            if ui.selectable_label(clip.transition_in.is_none(), "Cut").clicked() {
                                let _ = ed.edit("Transition", |p| p.set_transition_in(id, None));
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("At end:");
                            if ui.selectable_label(clip.transition_out.is_some(), "Fade to black").clicked() {
                                if let Err(e) = ed.edit("Transition", |p| p.set_transition_out(id, Some(Transition { kind: TransitionKind::FadeToBlack, duration: d }))) {
                                    err = Some(e.to_string());
                                }
                            }
                            if ui.selectable_label(clip.transition_out.is_none(), "Cut").clicked() {
                                let _ = ed.edit("Transition", |p| p.set_transition_out(id, None));
                            }
                        });
                    });
                }
                if visual && media.is_some() {
                    egui::CollapsingHeader::new("Fit, crop and color").show(ui, |ui| {
                        let mut fit = clip.fit;
                        egui::ComboBox::from_label("Fit").selected_text(format!("{fit:?}")).show_ui(ui, |ui| {
                            for f in [FitMode::Fit, FitMode::Fill, FitMode::Stretch] {
                                ui.selectable_value(&mut fit, f, format!("{f:?}"));
                            }
                        });
                        if fit != clip.fit {
                            edit_clip(ed, &mut err, id, "fit", |c| {
                                c.fit = fit;
                                Ok(())
                            });
                        }
                        let mut cr = clip.crop;
                        let mut ch = false;
                        for (v, l) in [(&mut cr.left, "Crop left"), (&mut cr.right, "Crop right"), (&mut cr.top, "Crop top"), (&mut cr.bottom, "Crop bottom")] {
                            ch |= ui.add(Slider::new(v, 0.0..=0.9).text(l)).changed();
                        }
                        if ch {
                            edit_clip(ed, &mut err, id, "crop", |c| {
                                c.crop = cr;
                                Ok(())
                            });
                        }
                        let mut ca = clip.color;
                        let mut ch = ui.add(Slider::new(&mut ca.brightness, -1.0..=1.0).text("Brightness")).changed();
                        ch |= ui.add(Slider::new(&mut ca.contrast, 0.0..=2.0).text("Contrast")).changed();
                        ch |= ui.add(Slider::new(&mut ca.saturation, 0.0..=3.0).text("Color strength")).changed();
                        if ch {
                            edit_clip(ed, &mut err, id, "color", |c| {
                                c.color = ca;
                                Ok(())
                            });
                        }
                    });
                }
            });
        }
        self.interp = new_interp;
        if let Some(e) = err {
            self.error(e);
        }
        if let Some(t) = jump {
            self.seek(t);
        }
    }
}
