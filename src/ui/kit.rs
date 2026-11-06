// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! The Drums view: the profile's drum map (with Learn), the recent-hits monitor and the
//! retrigger guard.

use femtovg::renderer::OpenGl;
use femtovg::{Align, Baseline, Canvas, Color, Paint, Path};

use super::theme::{self, WithAlpha, HEIGHT, WIDTH};
use super::{Action, Editor, Overlay, Rect, Ui};
use crate::kit;
use crate::stats;

const ROW_H: f32 = 30.0;
const MAP_ROWS: usize = 10;

fn ignored() -> Color {
    Color::rgba(240, 150, 170, 200)
}

/// "38 · D2"
fn note_label(note: u8) -> String {
    format!("{} · {}", note, stats::note_name(note))
}

impl Editor {
    /// Truncates `s` with "…" to fit `max_w`.
    pub(super) fn fit(&self, c: &Canvas<OpenGl>, s: &str, size: f32, max_w: f32) -> String {
        if self.measure(c, s, size, 300.0) <= max_w {
            return s.to_string();
        }
        let mut chars: Vec<char> = s.chars().collect();
        while !chars.is_empty() {
            chars.pop();
            let t: String = chars.iter().collect::<String>() + "…";
            if self.measure(c, &t, size, 300.0) <= max_w {
                return t;
            }
        }
        String::new()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn gradient_text(&self, c: &mut Canvas<OpenGl>, x: f32, y: f32, s: &str, size: f32, w: f32, align: Align) {
        let x0 = match align {
            Align::Right => x - w,
            Align::Center => x - w / 2.0,
            _ => x,
        };
        let paint = theme::gradient(x0, 0.0, x0 + w, 0.0, 255)
            .with_font(&[self.font])
            .with_font_size(size)
            .with_font_weight(300.0)
            .with_text_align(align)
            .with_text_baseline(Baseline::Middle);
        let _ = c.fill_text(x, y, s, &paint);
    }

    pub(super) fn draw_kit(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui) {
        let w = WIDTH as f32;
        self.top_bar(c, ui);
        self.label(c, w - 16.0, 28.0, "Drums", theme::faint(), Align::Right);
        Self::hline(c, 16.0, w - 16.0, 52.0, theme::hairline());

        // The drum map.
        let panel = Rect::new(16.0, 64.0, 470.0, 370.0);
        Self::rounded(c, panel, 6.0, Some(theme::panel_fill()), Some(Paint::color(theme::hairline())));
        self.label(c, panel.x + 14.0, panel.y + 20.0, "Drum map", theme::dim(), Align::Left);
        let learn = Rect::new(panel.x + panel.w - 92.0, panel.y + 6.0, 80.0, 28.0);
        let hovered = learn.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
        Self::rounded(c, learn, 4.0, hovered.then(theme::hover_fill), Some(theme::window_gradient(200)));
        self.text(c, learn.x + learn.w / 2.0, learn.y + learn.h / 2.0, "Learn", 13.5, if hovered { theme::text() } else { theme::dim() }, Align::Center);
        ui.regions.push((learn, Action::Learn));

        let names: Vec<(u8, String)> = ui.kit.names.iter().map(|(&n, s)| (n, s.clone())).collect();
        let list = Rect::new(panel.x + 6.0, panel.y + 42.0, panel.w - 12.0, MAP_ROWS as f32 * ROW_H);
        ui.kit_scroll = ui.kit_scroll.min(names.len().saturating_sub(MAP_ROWS));
        if names.is_empty() {
            self.text(c, panel.x + panel.w / 2.0, panel.y + 150.0, "No drums named yet", 15.0, theme::dim(), Align::Center);
            self.text(c, panel.x + panel.w / 2.0, panel.y + 178.0, "Press Learn and hit a pad, or click a hit on the right.", 12.5, theme::faint().with_alpha(110), Align::Center);
            self.text(c, panel.x + panel.w / 2.0, panel.y + 198.0, "Give pads the same name to count them as one drum.", 12.5, theme::faint().with_alpha(110), Align::Center);
        }
        let mut y = list.y;
        for (note, name) in names.iter().skip(ui.kit_scroll).take(MAP_ROWS) {
            let r = Rect::new(list.x, y, list.w, ROW_H);
            let hovered = r.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
            if hovered {
                Self::rounded(c, r, 4.0, Some(theme::hover_fill()), None);
            }
            self.text(c, r.x + 10.0, r.y + r.h / 2.0, &note_label(*note), 12.5, theme::faint().with_alpha(120), Align::Left);
            let fitted = self.fit(c, name, 14.5, 230.0);
            self.text(c, r.x + 92.0, r.y + r.h / 2.0, &fitted, 14.5, theme::text(), Align::Left);
            ui.regions.push((r, Action::PickNote(*note)));
            if hovered {
                let rename = Rect::new(r.x + r.w - 136.0, r.y + 3.0, 66.0, r.h - 6.0);
                let remove = Rect::new(r.x + r.w - 68.0, r.y + 3.0, 64.0, r.h - 6.0);
                for (b, s, a) in [(rename, "Rename", Action::PickNote(*note)), (remove, "Remove", Action::RemoveDrum(*note))] {
                    let on = b.contains(ui.mouse);
                    if on {
                        Self::rounded(c, b, 4.0, Some(theme::hover_fill()), None);
                    }
                    self.text(c, b.x + b.w / 2.0, b.y + b.h / 2.0, s, 12.0, if on { theme::text() } else { theme::dim() }, Align::Center);
                    ui.regions.push((b, a));
                }
            }
            y += ROW_H;
        }
        if names.len() > MAP_ROWS {
            let shown = format!("{}–{} of {}", ui.kit_scroll + 1, (ui.kit_scroll + MAP_ROWS).min(names.len()), names.len());
            self.text(c, panel.x + panel.w - 14.0, panel.y + panel.h - 13.0, &shown, 11.0, theme::faint(), Align::Right);
        }

        self.draw_recent(c, ui, Rect::new(500.0, 64.0, w - 516.0, 232.0));
        self.draw_guard(c, ui, Rect::new(500.0, 304.0, w - 516.0, 68.0));
        self.draw_pedal_setting(c, ui, Rect::new(500.0, 380.0, w - 516.0, 54.0));
    }

    /// Whether to read the hi-hat pedal (CC 4), for how open the hats are played.
    fn draw_pedal_setting(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, panel: Rect) {
        Self::rounded(c, panel, 6.0, Some(theme::panel_fill()), Some(Paint::color(theme::hairline())));
        self.label(c, panel.x + 14.0, panel.y + 18.0, "Hi-hat pedal", theme::dim(), Align::Left);
        self.text(c, panel.x + 14.0, panel.y + 38.0, "Read CC 4 to see how open you play", 12.0, theme::faint().with_alpha(120), Align::Left);
        let on = ui.kit.pedal_cc != 0;
        let r = Rect::new(panel.x + panel.w - 62.0, panel.y + 13.0, 50.0, 28.0);
        let hovered = r.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
        let stroke = if on { theme::window_gradient(220) } else { Paint::color(theme::hairline()) };
        Self::rounded(c, r, 14.0, hovered.then(theme::hover_fill), Some(stroke));
        let label = if on { "On" } else { "Off" };
        if on {
            self.gradient_text(c, r.x + r.w / 2.0, r.y + r.h / 2.0, label, 13.5, 30.0, Align::Center);
        } else {
            self.text(c, r.x + r.w / 2.0, r.y + r.h / 2.0, label, 13.5, if hovered { theme::text() } else { theme::dim() }, Align::Center);
        }
        ui.regions.push((r, Action::TogglePedal));
        ui.tips.push((
            Rect::new(panel.x, panel.y, panel.w - 70.0, panel.h),
            "Maps MIDI CC 4 to the plugin's Hi-hat pedal parameter\nTry it in Reaper: check your hi-hat still works".into(),
        ));
    }

    /// Back, profile picker: the same top bar as the stats view.
    pub(super) fn top_bar(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui) {
        let back = Rect::new(12.0, 12.0, 100.0, 32.0);
        let hovered = back.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
        if hovered {
            Self::rounded(c, back, 4.0, Some(theme::hover_fill()), None);
        }
        let color = if hovered { theme::text() } else { theme::dim() };
        Self::chevron(c, back.x + 14.0, back.y + 16.0, 5.0, -1, color);
        self.text(c, back.x + 26.0, back.y + 16.0, "Counter", 14.0, color, Align::Left);
        ui.regions.push((back, Action::ShowCounter));

        let name = ui.snapshot.profile.clone();
        let nw = self.measure(c, &name, 14.0, 300.0);
        let r = Rect::new(back.x + back.w + 4.0, 12.0, nw + 36.0, 32.0);
        let hovered = r.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
        if hovered {
            Self::rounded(c, r, 4.0, Some(theme::hover_fill()), None);
        }
        self.text(c, r.x + 12.0, r.y + 16.0, &name, 14.0, theme::text(), Align::Left);
        Self::chevron(c, r.x + 12.0 + nw + 12.0, r.y + 16.0, 3.0, 0, theme::dim());
        ui.regions.push((r, Action::OpenProfiles));
    }

    fn draw_recent(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, panel: Rect) {
        Self::rounded(c, panel, 6.0, Some(theme::panel_fill()), Some(Paint::color(theme::hairline())));
        self.label(c, panel.x + 14.0, panel.y + 20.0, "Recent hits", theme::dim(), Align::Left);
        let hits = self.shared.recent_hits();
        if hits.is_empty() {
            self.text(c, panel.x + panel.w / 2.0, panel.y + 104.0, "Hit something", 14.0, theme::dim(), Align::Center);
            self.text(c, panel.x + panel.w / 2.0, panel.y + 128.0, "Every note-on that arrives shows here.", 12.0, theme::faint().with_alpha(110), Align::Center);
            return;
        }
        let row_h = 24.0;
        let mut y = panel.y + 38.0;
        let fit = ((panel.h - 42.0) / row_h) as usize;
        for (i, hit) in hits.iter().take(fit).enumerate() {
            let r = Rect::new(panel.x + 6.0, y, panel.w - 12.0, row_h);
            let hovered = r.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
            if hovered {
                Self::rounded(c, r, 4.0, Some(theme::hover_fill()), None);
            }
            let fade = if i == 0 { 255 } else { (225 - i as i32 * 12).max(110) as u8 };
            let cy = r.y + r.h / 2.0;
            self.text(c, r.x + 8.0, cy, &note_label(hit.note), 12.0, theme::faint().with_alpha(120), Align::Left);
            let (name, named) = match ui.kit.name(hit.note) {
                Some(n) => (n.to_string(), true),
                None => (stats::gm_drum_name(hit.note).unwrap_or("").to_string(), false),
            };
            let name = self.fit(c, &name, 12.5, 84.0);
            let col = if named { theme::text().with_alpha(fade.min(225)) } else { theme::faint().with_alpha(110) };
            self.text(c, r.x + 64.0, cy, &name, 12.5, col, Align::Left);
            self.text(c, r.x + 168.0, cy, &format!("v{}", hit.vel), 11.5, theme::dim(), Align::Right);
            let right = r.x + r.w - 8.0;
            match (hit.counted, hit.gap_ms) {
                (false, Some(g)) => self.text(c, right, cy, &format!("ignored {g} ms"), 11.5, ignored(), Align::Right),
                (false, None) => self.text(c, right, cy, "ignored", 11.5, ignored(), Align::Right),
                (true, Some(g)) if g < 1000 => self.text(c, right, cy, &format!("+{g} ms"), 11.5, theme::faint().with_alpha(110), Align::Right),
                _ => 0.0,
            };
            ui.regions.push((r, Action::PickNote(hit.note)));
            if hovered {
                ui.tips.push((r, format!("{}\nClick to name this note", note_label(hit.note))));
            }
            y += row_h;
        }
    }

    fn draw_guard(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, panel: Rect) {
        Self::rounded(c, panel, 6.0, Some(theme::panel_fill()), Some(Paint::color(theme::hairline())));
        self.label(c, panel.x + 14.0, panel.y + 18.0, "Retrigger guard", theme::dim(), Align::Left);
        self.text(c, panel.x + 14.0, panel.y + 38.0, "Repeats of a drum within", 12.0, theme::faint().with_alpha(120), Align::Left);
        self.text(c, panel.x + 14.0, panel.y + 53.0, "this time count once", 12.0, theme::faint().with_alpha(120), Align::Left);
        let ms = ui.kit.retrigger_ms;
        let value = if ms == 0 { "Off".to_string() } else { format!("{ms} ms") };
        let cx = panel.x + panel.w - 62.0;
        let cy = panel.y + 38.0;
        self.gradient_text(c, cx, cy, &value, 16.0, 60.0, Align::Center);
        for (dir, x) in [(-1, cx - 48.0), (1, cx + 48.0)] {
            let r = Rect::new(x - 13.0, cy - 14.0, 26.0, 28.0);
            let enabled = if dir < 0 { ms > kit::RETRIGGER_CHOICES[0] } else { ms < *kit::RETRIGGER_CHOICES.last().unwrap() };
            let hovered = enabled && r.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
            if hovered {
                Self::rounded(c, r, 4.0, Some(theme::hover_fill()), None);
            }
            let col = if !enabled { theme::faint().with_alpha(40) } else if hovered { theme::text() } else { theme::dim() };
            Self::chevron(c, x, cy, 4.5, dir, col);
            if enabled {
                ui.regions.push((r, Action::Retrigger(dir)));
            }
        }
    }

    pub(super) fn draw_learn(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui) {
        let (w, h) = (WIDTH as f32, HEIGHT as f32);
        Self::scrim(c, ui);
        let panel = Rect::new(w / 2.0 - 190.0, h / 2.0 - 85.0, 380.0, 170.0);
        Self::rounded(c, panel, 8.0, Some(theme::popup_fill()), Some(theme::window_gradient(140)));
        self.label(c, panel.x + 24.0, panel.y + 30.0, "Learn", theme::dim(), Align::Left);
        self.gradient_text(c, panel.x + panel.w / 2.0, panel.y + 72.0, "Hit a drum or pad…", 22.0, 220.0, Align::Center);
        let cancel = Rect::new(panel.x + panel.w / 2.0 - 44.0, panel.y + panel.h - 52.0, 88.0, 32.0);
        let hovered = cancel.contains(ui.mouse);
        Self::rounded(c, cancel, 4.0, hovered.then(theme::hover_fill), Some(Paint::color(theme::hairline())));
        self.text(c, cancel.x + cancel.w / 2.0, cancel.y + cancel.h / 2.0, "Cancel", 13.5, if hovered { theme::text() } else { theme::dim() }, Align::Center);
        ui.regions.push((cancel, Action::CloseOverlay));
    }

    pub(super) fn draw_name_drum(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, note: u8, text: &str, error: Option<&str>) {
        let (w, h) = (WIDTH as f32, HEIGHT as f32);
        Self::scrim(c, ui);
        let panel = Rect::new(w / 2.0 - 260.0, h / 2.0 - 165.0, 520.0, 330.0);
        Self::rounded(c, panel, 8.0, Some(theme::popup_fill()), Some(theme::window_gradient(140)));
        self.label(c, panel.x + 24.0, panel.y + 28.0, "Name this drum", theme::dim(), Align::Left);
        let mut heading = format!("Note {}", note_label(note));
        if let Some(gm) = stats::gm_drum_name(note) {
            heading.push_str(&format!("  ({gm} in GM)"));
        }
        self.gradient_text(c, panel.x + panel.w - 24.0, panel.y + 28.0, &heading, 13.5, 240.0, Align::Right);

        let field = Rect::new(panel.x + 24.0, panel.y + 48.0, panel.w - 48.0, 38.0);
        Self::rounded(c, field, 4.0, Some(Color::rgba(0, 0, 0, 120)), Some(Paint::color(theme::hairline())));
        ui.regions.push((field, Action::Field));
        let tw = if text.is_empty() {
            self.text(c, field.x + 12.0, field.y + field.h / 2.0, "Snare, Crash 2, …", 15.0, theme::faint(), Align::Left);
            0.0
        } else {
            self.text(c, field.x + 12.0, field.y + field.h / 2.0, text, 15.0, theme::text(), Align::Left)
        };
        let mut caret = Path::new();
        caret.rect(field.x + 12.0 + tw + 1.0, field.y + 10.0, 1.0, field.h - 20.0);
        c.fill_path(&caret, &theme::window_gradient(255));

        match error {
            Some(e) => self.text(c, panel.x + 24.0, panel.y + 102.0, e, 12.5, ignored(), Align::Left),
            None => self.text(c, panel.x + 24.0, panel.y + 102.0, "Type a name or pick one. Hit another pad to switch notes.", 12.0, theme::faint().with_alpha(110), Align::Left),
        };

        // Preset chips.
        let cols = 4;
        let gap = 6.0;
        let cw = (panel.w - 48.0 - gap * (cols as f32 - 1.0)) / cols as f32;
        for (i, preset) in kit::PRESETS.iter().enumerate() {
            let (col, row) = (i % cols, i / cols);
            let r = Rect::new(panel.x + 24.0 + col as f32 * (cw + gap), panel.y + 120.0 + row as f32 * 32.0, cw, 27.0);
            let hovered = r.contains(ui.mouse);
            let selected = text.eq_ignore_ascii_case(preset);
            Self::rounded(c, r, 4.0, hovered.then(theme::hover_fill), Some(if selected { theme::window_gradient(200) } else { Paint::color(theme::hairline()) }));
            let col = if selected || hovered { theme::text() } else { theme::dim() };
            self.text(c, r.x + r.w / 2.0, r.y + r.h / 2.0, preset, 12.5, col, Align::Center);
            ui.regions.push((r, Action::Preset(preset)));
        }

        let by = panel.y + panel.h - 54.0;
        if ui.kit.name(note).is_some() {
            let remove = Rect::new(panel.x + 24.0, by, 88.0, 32.0);
            let hovered = remove.contains(ui.mouse);
            Self::rounded(c, remove, 4.0, hovered.then(theme::hover_fill), Some(Paint::color(theme::hairline())));
            self.text(c, remove.x + remove.w / 2.0, remove.y + remove.h / 2.0, "Remove", 13.5, if hovered { ignored() } else { theme::dim() }, Align::Center);
            ui.regions.push((remove, Action::RemoveDrum(note)));
        }
        let cancel = Rect::new(panel.x + panel.w - 210.0, by, 88.0, 32.0);
        let save = Rect::new(panel.x + panel.w - 112.0, by, 88.0, 32.0);
        for (r, s, a) in [(cancel, "Cancel", Action::CloseOverlay), (save, "Save", Action::SaveDrum)] {
            let hovered = r.contains(ui.mouse);
            let stroke = if a == Action::SaveDrum { theme::window_gradient(200) } else { Paint::color(theme::hairline()) };
            Self::rounded(c, r, 4.0, hovered.then(theme::hover_fill), Some(stroke));
            self.text(c, r.x + r.w / 2.0, r.y + r.h / 2.0, s, 13.5, if hovered { theme::text() } else { theme::dim() }, Align::Center);
            ui.regions.push((r, a));
        }
    }

    /// Darkens the window behind a dialog and swallows clicks outside it.
    pub(super) fn scrim(c: &mut Canvas<OpenGl>, ui: &mut Ui) {
        let (w, h) = (WIDTH as f32, HEIGHT as f32);
        let mut p = Path::new();
        p.rect(0.0, 0.0, w, h);
        c.fill_path(&p, &Paint::color(Color::rgba(0, 0, 0, 150)));
        ui.regions.push((Rect::new(0.0, 0.0, w, h), Action::None));
    }

    /// Bars per named drum for the stats view's notes panel.
    pub(super) fn draw_drum_bars(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, notes: &[u64; 128], area: Rect) {
        let counts = ui.kit.drum_counts(notes);
        if ui.kit.names.is_empty() {
            self.text(c, area.x + area.w / 2.0, area.y + area.h / 2.0 - 8.0, "No drums named yet", 14.0, theme::dim(), Align::Center);
            self.text(c, area.x + area.w / 2.0, area.y + area.h / 2.0 + 14.0, "Name them under Drums, on the counter screen", 12.0, theme::faint().with_alpha(110), Align::Center);
            return;
        }
        let values: Vec<u64> = counts.iter().map(|(_, n)| *n).collect();
        let labels: Vec<String> = counts.iter().map(|(s, _)| s.clone()).collect();
        self.bars(c, ui, area, &values, &|i| labels[i].clone());
        let slot = area.w / values.len().max(1) as f32;
        for (i, name) in labels.iter().enumerate() {
            let s = self.fit(c, name, 10.5, slot - 2.0);
            self.text(c, area.x + i as f32 * slot + slot / 2.0, area.y + area.h + 13.0, &s, 10.5, theme::faint(), Align::Center);
        }
    }
}
