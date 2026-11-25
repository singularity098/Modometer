// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! The stats view's pages beyond the overview: Rhythm (tempo and timing), Kit or Keys, Habits
//! (calendar, when you play, sessions, personal bests) and Review.

use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone};
use femtovg::renderer::OpenGl;
use femtovg::{Align, Canvas, Color, Paint, Path};

use super::theme::{self, commas, WithAlpha};
use super::{Action, Editor, Overlay, Rect, TempoSource, Ui};
use crate::kit::Role;
use crate::stats::{self, Span, Stats};
use crate::store::BINS;

/// Velocity bins counted as ghost notes (below 48) and accents (112 and up).
const GHOST_BINS: usize = 3;
const ACCENT_BIN: usize = 7;
/// Swing and offsets need this many measurements before they're shown.
const MIN_SAMPLES: u32 = 8;

fn weekday_name(i: usize) -> &'static str {
    ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"][i % 7]
}

fn part_of_day(hour: usize) -> &'static str {
    match hour {
        0..=5 => "nights",
        6..=11 => "mornings",
        12..=17 => "afternoons",
        _ => "evenings",
    }
}

/// How steady an average distance from the grid is.
fn steadiness(ms: f64) -> &'static str {
    match ms {
        m if m < 6.0 => "very tight",
        m if m < 11.0 => "tight",
        m if m < 18.0 => "a little loose",
        _ => "loose",
    }
}

/// Hits a second as sixteenth notes at a tempo: 4 a beat.
fn sixteenths_bpm(rate: f64) -> f64 {
    rate * 15.0
}

fn minute_label(m: i64) -> String {
    Local.timestamp_opt(m * 60, 0).single().map(|t| t.format("%a %-d %b %Y, %H:%M").to_string()).unwrap_or_default()
}

impl Editor {
    /// A panel with a heading.
    fn panel(&self, c: &mut Canvas<OpenGl>, r: Rect, title: &str) {
        Self::rounded(c, r, 6.0, Some(theme::panel_fill()), Some(Paint::color(theme::hairline())));
        self.label(c, r.x + 14.0, r.y + 18.0, title, theme::dim(), Align::Left);
    }

    /// Two centred lines saying why there's nothing to show.
    fn empty(&self, c: &mut Canvas<OpenGl>, r: Rect, title: &str, hint: &str) {
        self.text(c, r.x + r.w / 2.0, r.y + r.h / 2.0 - 10.0, title, 14.0, theme::dim(), Align::Center);
        self.text(c, r.x + r.w / 2.0, r.y + r.h / 2.0 + 12.0, hint, 12.0, theme::faint().with_alpha(110), Align::Center);
    }

    /// A big gradient number with a small unit after it; returns the right edge.
    fn big(&self, c: &mut Canvas<OpenGl>, x: f32, y: f32, value: &str, unit: &str, size: f32) -> f32 {
        let vw = self.measure(c, value, size, 200.0);
        let paint = theme::gradient(x, 0.0, x + vw.max(40.0), 0.0, 255)
            .with_font(&[self.font])
            .with_font_size(size)
            .with_font_weight(200.0)
            .with_text_align(Align::Left)
            .with_text_baseline(femtovg::Baseline::Middle);
        let _ = c.fill_text(x, y, value, &paint);
        let uw = self.text(c, x + vw + 6.0, y + size * 0.18, unit, 13.0, theme::dim(), Align::Left);
        x + vw + 6.0 + uw
    }

    // ---- Rhythm ----

    pub(super) fn draw_rhythm(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats) {
        let (lw, rx, rw) = self.columns(376.0, 400.0);
        let h = 336.0 + self.extra().1;
        self.draw_tempo(c, ui, stats, Rect::new(16.0, 100.0, lw, h));
        self.draw_timing(c, ui, stats, Rect::new(rx, 100.0, rw, h));
    }

    fn draw_tempo(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats, p: Rect) {
        self.panel(c, p, "Tempo");
        let source = ui.tempo_source;
        let rp = Rect::new(p.x + p.w - 64.0, p.y + 6.0, 56.0, 24.0);
        let rl = Rect::new(rp.x - 58.0, p.y + 6.0, 56.0, 24.0);
        self.small_toggle(c, ui, rl, "Played", source == TempoSource::Played, Action::TempoSource(TempoSource::Played));
        self.small_toggle(c, ui, rp, "Project", source == TempoSource::Project, Action::TempoSource(TempoSource::Project));
        ui.tips.push((rl, "The tempo you played at\nworked out from your notes, no click needed".into()));
        ui.tips.push((rp, "The project's tempo\nwhile the host's transport plays".into()));
        let hist = match source {
            TempoSource::Played => &stats.detail.tempo,
            TempoSource::Project => &stats.detail.host_tempo,
        };
        let Some(sum) = stats::summarize(hist) else {
            let body = Rect::new(p.x, p.y + 30.0, p.w, p.h - 30.0);
            match source {
                TempoSource::Played => self.empty(c, body, "No steady tempo yet", "It shows after a few seconds of steady playing."),
                TempoSource::Project => self.empty(c, body, "No project tempo", "Logged while the host's transport plays."),
            }
            return;
        };
        let right = self.big(c, p.x + 14.0, p.y + 62.0, &format!("{:.0}", sum.mode), "BPM, most often", 40.0);
        let _ = right;
        self.text(c, p.x + 14.0, p.y + 96.0, &format!("median {:.0}  ·  average {:.0}  ·  {} notes", sum.median, sum.mean, commas(sum.count)), 12.5, theme::dim(), Align::Left);

        // Histogram in 2 BPM steps, over at least 60-200.
        let lo = (hist.keys().next().copied().unwrap_or(60) as u32 / 10 * 10).min(60);
        let hi = ((hist.keys().last().copied().unwrap_or(200) as u32).div_ceil(10) * 10).max(200);
        let bins = ((hi - lo) / 2) as usize;
        let mut values = vec![0u64; bins];
        for (&bpm, &n) in hist {
            let i = ((bpm as u32).saturating_sub(lo) / 2) as usize;
            if let Some(v) = values.get_mut(i.min(bins - 1)) {
                *v += n as u64;
            }
        }
        let area = Rect::new(p.x + 14.0, p.y + 120.0, p.w - 28.0, p.h - 156.0);
        self.bars(c, ui, area, &values, &|i| {
            let b = lo + i as u32 * 2;
            format!("{}–{} BPM", b, b + 1)
        });
        let slot = area.w / bins as f32;
        let mut b = lo.div_ceil(20) * 20;
        while b <= hi {
            let x = area.x + ((b - lo) / 2) as f32 * slot;
            self.text(c, x, area.y + area.h + 13.0, &b.to_string(), 10.5, theme::faint(), Align::Center);
            b += 20;
        }
    }

    fn draw_timing(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats, p: Rect) {
        self.panel(c, p, "Timing");
        self.text(c, p.x + p.w - 14.0, p.y + 18.0, "against a grid that follows you", 11.5, theme::faint(), Align::Right);
        let d = &stats.detail;
        if d.timing.0 == 0 {
            let body = Rect::new(p.x, p.y + 30.0, p.w, p.h - 30.0);
            self.empty(c, body, "Nothing measured yet", "Timing needs steady playing: a beat, not free time.");
            return;
        }
        let ms = d.timing.1 as f64 / d.timing.0 as f64 / 10.0;
        let right = self.big(c, p.x + 14.0, p.y + 62.0, &format!("±{ms:.1}"), "ms", 40.0);
        self.gradient_text(c, right + 16.0, p.y + 66.0, steadiness(ms), 15.0, 120.0, Align::Left);
        ui.tips.push((
            Rect::new(p.x + 14.0, p.y + 40.0, right - p.x, 44.0),
            "How far notes land from an imagined grid\nthat follows your own tempo; lower is steadier".into(),
        ));
        self.text(c, p.x + 14.0, p.y + 96.0, &format!("average distance from the grid  ·  {} notes", commas(d.timing.0 as u64)), 12.5, theme::dim(), Align::Left);

        // Swing and drift.
        let y = p.y + 130.0;
        self.label(c, p.x + 14.0, y, "Swing", theme::faint(), Align::Left);
        self.label(c, p.x + 200.0, y, "Drift", theme::faint(), Align::Left);
        if d.swing.0 >= MIN_SAMPLES {
            let sw = d.swing.1 as f64 / d.swing.0 as f64 / 10.0;
            self.big(c, p.x + 14.0, y + 26.0, &format!("{sw:.0}%"), "", 24.0);
            let feel = if sw < 54.0 { "straight" } else if sw < 62.0 { "a light swing" } else if sw < 70.0 { "triplet swing" } else { "hard swing" };
            self.text(c, p.x + 14.0, y + 50.0, feel, 12.0, theme::dim(), Align::Left);
            ui.tips.push((Rect::new(p.x + 14.0, y - 8.0, 170.0, 66.0), format!("Off-beats {sw:.1}% of the way through the beat\n50% is straight, 67% triplet swing")));
        } else {
            self.text(c, p.x + 14.0, y + 26.0, "not enough off-beats", 12.0, theme::faint().with_alpha(110), Align::Left);
        }
        if d.drift.0 > 0 {
            let per_min = d.drift.1 as f64 / d.drift.0 as f64 / 100.0;
            self.big(c, p.x + 200.0, y + 26.0, &format!("{per_min:+.1}"), "BPM a minute", 24.0);
            let feel = if per_min > 0.5 { "you tend to speed up" } else if per_min < -0.5 { "you tend to slow down" } else { "you keep the tempo" };
            self.text(c, p.x + 200.0, y + 50.0, feel, 12.0, theme::dim(), Align::Left);
            ui.tips.push((Rect::new(p.x + 200.0, y - 8.0, 170.0, 66.0), format!("Over {} minutes of steady playing\nhow much the tempo rose or fell", d.drift.0)));
        } else {
            self.text(c, p.x + 200.0, y + 26.0, "needs a steady minute", 12.0, theme::faint().with_alpha(110), Align::Left);
        }

        // Ahead or behind, per drum.
        let y = p.y + 206.0;
        self.label(c, p.x + 14.0, y, "Ahead or behind", theme::faint(), Align::Left);
        if !ui.kit.is_drums() {
            self.text(c, p.x + 14.0, y + 26.0, "Per drum, for profiles with named drums.", 12.0, theme::faint().with_alpha(110), Align::Left);
            return;
        }
        let mut rows: Vec<(String, u32, f64)> = d
            .offsets
            .iter()
            .filter(|(_, (n, _))| *n >= MIN_SAMPLES * 4)
            .map(|(&note, &(n, sum))| (ui.kit.drum_label(note), n, sum as f64 / n as f64 / 10.0))
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.1));
        rows.truncate(5);
        let mid = p.x + 196.0;
        let half = 70.0;
        Self::hline(c, mid, mid, y + 10.0, theme::hairline());
        let mut axis = Path::new();
        axis.move_to(mid + 0.5, y + 12.0);
        axis.line_to(mid + 0.5, y + 14.0 + rows.len() as f32 * 22.0);
        c.stroke_path(&axis, &Paint::color(theme::hairline()).with_line_width(1.0));
        for (i, (name, n, off)) in rows.iter().enumerate() {
            let ry = y + 24.0 + i as f32 * 22.0;
            let name = self.fit(c, name, 12.5, 104.0);
            self.text(c, p.x + 14.0, ry, &name, 12.5, theme::text(), Align::Left);
            let len = (*off as f32 / 15.0).clamp(-1.0, 1.0) * half;
            let mut bar = Path::new();
            bar.rect(mid.min(mid + len), ry - 3.0, len.abs().max(1.0), 6.0);
            c.fill_path(&bar, &theme::gradient(mid - half, 0.0, mid + half, 0.0, 220));
            let s = if off.abs() < 0.5 { "on it".to_string() } else if *off < 0.0 { format!("{:.1} ms early", -off) } else { format!("{off:.1} ms late") };
            self.text(c, p.x + p.w - 14.0, ry, &s, 12.0, theme::dim(), Align::Right);
            ui.tips.push((Rect::new(p.x + 8.0, ry - 11.0, p.w - 16.0, 22.0), format!("{name}: {} notes measured\nrelative to the grid of all your notes", commas(*n as u64))));
        }
        if rows.is_empty() {
            self.text(c, p.x + 14.0, y + 26.0, "Not enough measured per drum yet.", 12.0, theme::faint().with_alpha(110), Align::Left);
        }
    }

    // ---- Kit ----

    pub(super) fn draw_kit_stats(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats) {
        let (lw, rx, rw) = self.columns(470.0, 500.0);
        let ey = self.extra().1;
        let k = ey / 3.0;
        self.draw_dynamics(c, ui, stats, Rect::new(16.0, 100.0, lw, 336.0 + ey));
        self.draw_speed(c, ui, stats, Rect::new(rx, 100.0, rw, 100.0 + k));
        self.draw_balance(c, ui, stats, Rect::new(rx, 208.0 + k, rw, 104.0 + k));
        self.draw_pedal(c, ui, stats, Rect::new(rx, 320.0 + 2.0 * k, rw, 116.0 + k));
    }

    fn draw_dynamics(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats, p: Rect) {
        self.panel(c, p, "Drums");
        let mut drums: Vec<(String, u8, u64)> = ui
            .kit
            .drums()
            .into_iter()
            .map(|(name, notes)| {
                let hits = notes.iter().map(|&n| stats.notes[n as usize]).sum();
                (name, notes[0], hits)
            })
            .collect();
        if drums.is_empty() {
            self.empty(c, p, "No drums named yet", "Name them under Drums, on the counter screen");
            return;
        }
        drums.sort_by_key(|d| std::cmp::Reverse(d.2));
        let total = stats.total.max(1);
        let cols = [(p.x + 124.0, "share"), (p.x + 246.0, "velocity"), (p.x + 336.0, "ghost"), (p.x + 404.0, "accents")];
        for (x, s) in cols {
            self.text(c, x, p.y + 40.0, s, 11.0, theme::faint(), Align::Left);
        }
        let row_h = 30.0;
        for (i, (name, lead, hits)) in drums.iter().take(9).enumerate() {
            let y = p.y + 62.0 + i as f32 * row_h;
            let share = *hits as f64 / total as f64;
            let neglected = share < 0.01;
            let name_fit = self.fit(c, name, 13.5, 104.0);
            self.text(c, p.x + 14.0, y, &name_fit, 13.5, if neglected { theme::dim() } else { theme::text() }, Align::Left);
            // Share of all hits.
            let bw = 70.0;
            let mut track = Path::new();
            track.rect(p.x + 124.0, y - 2.0, bw, 4.0);
            c.fill_path(&track, &Paint::color(theme::hairline()));
            let mut bar = Path::new();
            bar.rect(p.x + 124.0, y - 2.0, (bw * share as f32).max(if *hits > 0 { 1.0 } else { 0.0 }), 4.0);
            c.fill_path(&bar, &theme::gradient(p.x + 124.0, 0.0, p.x + 124.0 + bw, 0.0, 230));
            self.text(c, p.x + 200.0, y, &format!("{:.0}%", share * 100.0), 12.0, theme::dim(), Align::Left);
            let bins = stats.detail.drum_vels.get(lead).copied().unwrap_or([0; BINS]);
            let measured: u32 = bins.iter().sum();
            if neglected {
                self.text(c, p.x + 246.0, y, "hardly played", 12.0, Color::rgba(240, 150, 170, 170), Align::Left);
            } else if measured > 0 {
                // A tiny velocity histogram.
                let max = *bins.iter().max().unwrap_or(&1) as f32;
                let (hx, hw, hh) = (p.x + 246.0, 72.0, 18.0);
                let slot = hw / BINS as f32;
                let mut path = Path::new();
                for (k, &n) in bins.iter().enumerate() {
                    let h = (n as f32 / max * hh).max(if n > 0 { 1.0 } else { 0.0 });
                    path.rect(hx + k as f32 * slot + 1.0, y + hh / 2.0 - h, slot - 2.0, h);
                }
                c.fill_path(&path, &theme::gradient(hx, 0.0, hx + hw, 0.0, 210));
                let ghost = bins[..GHOST_BINS].iter().sum::<u32>() as f64 / measured as f64;
                let accent = bins[ACCENT_BIN] as f64 / measured as f64;
                self.text(c, p.x + 336.0, y, &format!("{:.0}%", ghost * 100.0), 12.5, theme::dim(), Align::Left);
                self.text(c, p.x + 404.0, y, &format!("{:.0}%", accent * 100.0), 12.5, theme::dim(), Align::Left);
            }
            let avg = if measured > 0 {
                format!(" · average velocity about {:.0}", bins.iter().enumerate().map(|(k, &n)| (k as f64 * 16.0 + 8.0) * n as f64).sum::<f64>() / measured as f64)
            } else {
                String::new()
            };
            ui.tips.push((
                Rect::new(p.x + 8.0, y - row_h / 2.0, p.w - 16.0, row_h),
                format!("{name}: {} hits{avg}\nghost notes: velocity under 48, accents: 112 and up", commas(*hits)),
            ));
        }
        if drums.len() > 9 {
            self.text(c, p.x + p.w - 14.0, p.y + p.h - 12.0, &format!("and {} more", drums.len() - 9), 11.0, theme::faint(), Align::Right);
        }
    }

    fn draw_speed(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats, p: Rect) {
        self.panel(c, p, "Speed");
        let d = &stats.detail;
        match d.speed {
            Some((note, r)) => {
                let rate = r as f64 / 10.0;
                self.gradient_text(c, p.x + 14.0, p.y + 44.0, &format!("16ths at {:.0} BPM", sixteenths_bpm(rate)), 18.0, 180.0, Align::Left);
                let who = if ui.kit.is_drums() { ui.kit.drum_label(note) } else { "notes".to_string() };
                self.text(c, p.x + 14.0, p.y + 66.0, &format!("{who}, {rate:.1} hits a second"), 12.0, theme::dim(), Align::Left);
                ui.tips.push((Rect::new(p.x, p.y, p.w, 74.0), "Top speed: the fastest eight hits in a row\non one drum".into()));
            }
            None => {
                self.text(c, p.x + 14.0, p.y + 50.0, "No fast runs yet", 13.0, theme::dim(), Align::Left);
            }
        }
        self.text(c, p.x + 14.0, p.y + 86.0, &format!("{} rolls  ·  {} fills", commas(d.rolls as u64), commas(d.fills as u64)), 12.5, theme::dim(), Align::Left);
        ui.tips.push((
            Rect::new(p.x, p.y + 76.0, p.w, 22.0),
            "Rolls: 6+ fast hits on a snare or tom\nFills: 4+ hits moving across the toms".into(),
        ));
    }

    fn draw_balance(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats, p: Rect) {
        self.panel(c, p, "Balance");
        let roles = ui.kit.roles();
        let total = stats.total.max(1) as f64;
        let mut by_role = [0u64; 7];
        let (mut ride, mut hat) = (0u64, 0u64);
        for n in 0..128usize {
            let hits = stats.notes[n];
            by_role[roles[n] as usize] += hits;
            let name = ui.kit.name(n as u8).map(str::to_lowercase);
            let is_ride = match &name {
                Some(name) => name.contains("ride"),
                None => matches!(n, 51 | 53 | 59),
            };
            if is_ride {
                ride += hits;
            }
            if matches!(roles[n], Role::HiHat) {
                hat += hits;
            }
        }
        let ratio = match (ride, hat) {
            (0, 0) => "no ride or hi-hat".to_string(),
            (0, _) => "all hi-hat".to_string(),
            (_, 0) => "all ride".to_string(),
            (r, h) if h >= r => format!("1 : {:.1}", h as f64 / r as f64),
            (r, h) => format!("{:.1} : 1", r as f64 / h as f64),
        };
        self.text(c, p.x + 14.0, p.y + 44.0, "Ride : hi-hat", 12.5, theme::dim(), Align::Left);
        self.gradient_text(c, p.x + 110.0, p.y + 44.0, &ratio, 16.0, 120.0, Align::Left);
        let pct = |r: Role| by_role[r as usize] as f64 / total * 100.0;
        self.text(c, p.x + 14.0, p.y + 70.0, &format!("Kick {:.0}%  ·  Snare {:.0}%  ·  Toms {:.0}%", pct(Role::Kick), pct(Role::Snare), pct(Role::Tom)), 12.5, theme::dim(), Align::Left);
        self.text(c, p.x + 14.0, p.y + 90.0, &format!("Hi-hat {:.0}%  ·  Cymbals {:.0}%", pct(Role::HiHat) + pct(Role::HatPedal), pct(Role::Cymbal)), 12.5, theme::dim(), Align::Left);
    }

    fn draw_pedal(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats, p: Rect) {
        self.panel(c, p, "Hi-hat pedal");
        let roles = ui.kit.roles();
        let chicks: u64 = (0..128).filter(|&n| roles[n] == Role::HatPedal).map(|n| stats.notes[n]).sum();
        if chicks > 0 {
            self.text(c, p.x + p.w - 14.0, p.y + 18.0, &format!("{} pedal hits", commas(chicks)), 11.5, theme::faint(), Align::Right);
        }
        let bins = stats.detail.pedal;
        let measured: u32 = bins.iter().sum();
        if measured == 0 {
            let (a, b) = if ui.kit.pedal_cc == 0 {
                ("Turn on Hi-hat pedal under Drums", "to see how open you play the hats.")
            } else {
                ("No hi-hat hits with the pedal read yet", "Play the hats with the pedal connected.")
            };
            self.text(c, p.x + 14.0, p.y + 54.0, a, 12.5, theme::dim(), Align::Left);
            self.text(c, p.x + 14.0, p.y + 74.0, b, 12.0, theme::faint().with_alpha(110), Align::Left);
            return;
        }
        let area = Rect::new(p.x + 14.0, p.y + 32.0, p.w - 28.0, 44.0);
        let values: Vec<u64> = bins.iter().map(|&n| n as u64).collect();
        self.bars(c, ui, area, &values, &|i| format!("Pedal {}–{}", i * 16, i * 16 + 15));
        self.text(c, area.x, area.y + area.h + 10.0, "pedal up", 10.5, theme::faint(), Align::Left);
        self.text(c, area.x + area.w, area.y + area.h + 10.0, "down", 10.5, theme::faint(), Align::Right);
        let share = |r: std::ops::Range<usize>| bins[r].iter().sum::<u32>() as f64 / measured as f64 * 100.0;
        let s = format!("open {:.0}%  ·  half {:.0}%  ·  closed {:.0}%", share(0..3), share(3..5), share(5..8));
        self.text(c, p.x + 14.0, p.y + p.h - 14.0, &s, 12.5, theme::dim(), Align::Left);
    }

    // ---- Keys ----

    pub(super) fn draw_keys_stats(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats) {
        let (lw, rx, rw) = self.columns(376.0, 400.0);
        let ey = self.extra().1;
        let k = ey / 3.0;
        self.draw_chords(c, ui, stats, Rect::new(16.0, 100.0, lw, 336.0 + ey));
        self.draw_range(c, ui, stats, Rect::new(rx, 100.0, rw, 92.0 + k));
        self.draw_intervals(c, ui, stats, Rect::new(rx, 200.0 + k, rw, 128.0 + k));
        self.draw_session_keys(c, ui, stats, Rect::new(rx, 336.0 + 2.0 * k, rw, 100.0 + k));
    }

    fn draw_chords(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats, p: Rect) {
        self.panel(c, p, "Chords");
        let mut chords: Vec<(u16, u32)> = stats.detail.chords.iter().map(|(&m, &n)| (m, n)).collect();
        if chords.is_empty() {
            self.empty(c, p, "No chords yet", "Three or more notes at once count as a chord.");
            return;
        }
        chords.sort_by_key(|&(m, n)| (std::cmp::Reverse(n), m));
        let total: u32 = chords.iter().map(|c| c.1).sum();
        self.text(c, p.x + p.w - 14.0, p.y + 18.0, &format!("{} played", commas(total as u64)), 11.5, theme::faint(), Align::Right);
        let max = chords[0].1 as f32;
        for (i, (mask, n)) in chords.iter().take(10).enumerate() {
            let y = p.y + 50.0 + i as f32 * 28.0;
            let name = stats::chord_name(*mask);
            if i == 0 {
                self.gradient_text(c, p.x + 14.0, y, &name, 15.0, 90.0, Align::Left);
            } else {
                self.text(c, p.x + 14.0, y, &self.fit(c, &name, 13.5, 96.0), 13.5, theme::text(), Align::Left);
            }
            let (bx, bw) = (p.x + 118.0, p.w - 196.0);
            let mut bar = Path::new();
            bar.rect(bx, y - 3.0, (bw * *n as f32 / max).max(1.0), 6.0);
            c.fill_path(&bar, &theme::gradient(bx, 0.0, bx + bw, 0.0, 220));
            self.text(c, p.x + p.w - 14.0, y, &commas(*n as u64), 12.0, theme::dim(), Align::Right);
            let notes = (0..12).filter(|k| mask >> k & 1 == 1).map(|k| stats::NOTE_NAMES[k]).collect::<Vec<_>>().join(" ");
            ui.tips.push((Rect::new(p.x + 8.0, y - 14.0, p.w - 16.0, 28.0), format!("{name}: {notes}\n{:.0}% of chords", *n as f64 / total as f64 * 100.0)));
        }
    }

    fn draw_range(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats, p: Rect) {
        self.panel(c, p, "Range");
        let lo = stats.notes.iter().position(|&n| n > 0);
        let hi = stats.notes.iter().rposition(|&n| n > 0);
        let (Some(lo), Some(hi)) = (lo, hi) else {
            self.text(c, p.x + 14.0, p.y + 50.0, "Nothing played", 13.0, theme::dim(), Align::Left);
            return;
        };
        let span = hi - lo;
        let range = format!("{} – {}", stats::note_name(lo as u8), stats::note_name(hi as u8));
        self.gradient_text(c, p.x + 14.0, p.y + 44.0, &range, 18.0, 120.0, Align::Left);
        let span_s = match (span / 12, span % 12) {
            (0, s) => format!("{s} semitones"),
            (o, 0) => format!("{o} octave{}", if o > 1 { "s" } else { "" }),
            (o, s) => format!("{o} octave{} and {s} semitone{}", if o > 1 { "s" } else { "" }, if s > 1 { "s" } else { "" }),
        };
        self.text(c, p.x + 150.0, p.y + 44.0, &span_s, 12.5, theme::dim(), Align::Left);
        if let Some((_, r)) = stats.detail.speed {
            self.text(c, p.x + p.w - 14.0, p.y + 18.0, &format!("up to {:.1} notes a second", r as f64 / 10.0), 11.5, theme::faint(), Align::Right);
        }
        // Where you play, across the keyboard (A0 to C8).
        let (x0, w, y) = (p.x + 14.0, p.w - 28.0, p.y + 66.0);
        let max = stats.notes[21..=108].iter().copied().max().unwrap_or(1).max(1) as f32;
        let slot = w / 88.0;
        let mut track = Path::new();
        track.rect(x0, y, w, 12.0);
        c.fill_path(&track, &Paint::color(theme::hairline().with_alpha(14)));
        for k in 21..=108usize {
            let n = stats.notes[k];
            if n == 0 {
                continue;
            }
            let a = (40.0 + 215.0 * (n as f32 / max).sqrt()) as u8;
            let mut cell = Path::new();
            cell.rect(x0 + (k - 21) as f32 * slot, y, slot.max(1.0), 12.0);
            c.fill_path(&cell, &theme::gradient(x0, 0.0, x0 + w, 0.0, a));
        }
        for k in (24..=108).step_by(12) {
            self.text(c, x0 + (k - 21) as f32 * slot, y + 20.0, &stats::note_name(k as u8), 9.5, theme::faint(), Align::Center);
        }
        let hover = Rect::new(x0, y - 4.0, w, 20.0);
        if hover.contains(ui.mouse) && matches!(ui.overlay, Overlay::None) {
            let k = (((ui.mouse.0 - x0) / slot) as usize).min(87) + 21;
            ui.tips.push((hover, format!("{} · {}\n{} notes", k, stats::note_name(k as u8), commas(stats.notes[k]))));
        }
    }

    fn draw_intervals(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats, p: Rect) {
        self.panel(c, p, "Intervals");
        self.text(c, p.x + p.w - 14.0, p.y + 18.0, "between single notes", 11.5, theme::faint(), Align::Right);
        let iv = &stats.detail.intervals;
        if iv.iter().all(|&n| n == 0) {
            self.empty(c, Rect::new(p.x, p.y + 20.0, p.w, p.h - 20.0), "No melodies yet", "Single notes one after another.");
            return;
        }
        let mut values: Vec<u64> = iv[..13].iter().map(|&n| n as u64).collect();
        values.push(iv[13..].iter().map(|&n| n as u64).sum());
        let area = Rect::new(p.x + 14.0, p.y + 34.0, p.w - 28.0, p.h - 64.0);
        self.bars(c, ui, area, &values, &|i| if i < 13 { stats::interval_name(i) } else { "More than an octave".to_string() });
        let short = ["U", "m2", "M2", "m3", "M3", "P4", "TT", "P5", "m6", "M6", "m7", "M7", "8ve", "+"];
        let slot = area.w / short.len() as f32;
        for (i, s) in short.iter().enumerate() {
            self.text(c, area.x + i as f32 * slot + slot / 2.0, area.y + area.h + 12.0, s, 10.0, theme::faint(), Align::Center);
        }
    }

    fn draw_session_keys(&self, c: &mut Canvas<OpenGl>, _ui: &mut Ui, stats: &Stats, p: Rect) {
        self.panel(c, p, "Keys by session");
        let sessions: Vec<&stats::Session> = stats.sessions.iter().rev().filter(|s| s.hits >= 50).take(3).collect();
        if sessions.is_empty() {
            self.text(c, p.x + 14.0, p.y + 50.0, "No sessions long enough to tell", 12.5, theme::dim(), Align::Left);
            return;
        }
        for (i, s) in sessions.iter().enumerate() {
            let y = p.y + 42.0 + i as f32 * 20.0;
            let when = Local.timestamp_opt(s.start * 60, 0).single().map(|t| t.format("%a %-d %b, %H:%M").to_string()).unwrap_or_default();
            self.text(c, p.x + 14.0, y, &format!("{when}  ·  {}", theme::duration((s.end - s.start) as f64)), 12.5, theme::dim(), Align::Left);
            let scale = stats::likely_scale(&s.pitch_classes).unwrap_or_else(|| "–".to_string());
            self.text(c, p.x + p.w - 14.0, y, &scale, 12.5, theme::text(), Align::Right);
        }
    }

    // ---- Habits ----

    pub(super) fn draw_habits(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats) {
        let Some(history) = ui.history.clone() else { return };
        let today = Local::now().date_naive();

        // Streaks.
        let mut parts = Vec::new();
        match history.current_streak(today) {
            Some(s) => parts.push((format!("Streak: {} day{}", s.days(), if s.days() == 1 { "" } else { "s" }), None)),
            None => parts.push(("No streak going".to_string(), None)),
        }
        if let Some(s) = history.longest_streak() {
            parts.push((
                format!("Longest: {} days", s.days()),
                Some((format!("{} – {}", s.first.format("%-d %b %Y"), s.last.format("%-d %b %Y")), format!("{} days in a row", s.days()))),
            ));
        }
        parts.push((format!("{} days played in all", commas(history.days.len() as u64)), None));
        self.parts_line(c, ui, 16.0, 106.0, self.w() - 16.0, parts);

        self.draw_calendar(c, ui, &history, stats, today);
        let (lw, rx, rw) = self.columns(470.0, 500.0);
        let h = 186.0 + self.extra().1;
        self.draw_week_hours(c, ui, stats, Rect::new(16.0, 250.0, lw, h));
        self.draw_bests(c, ui, &history, stats, Rect::new(rx, 250.0, rw, h));
    }

    /// A year of days, a column per week, darker for more notes. Click a day to see it.
    fn draw_calendar(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, history: &stats::History, stats: &Stats, today: NaiveDate) {
        let last_day = stats.edges.last().map(|e| e.date() - Duration::days(1)).unwrap_or(today).min(today);
        let last_monday = last_day - Duration::days(last_day.weekday().num_days_from_monday() as i64);
        let first = last_monday - Duration::weeks(52);
        let (x0, y0, cell, gap) = (50.0, 136.0, 11.0, 2.0);
        // Levels from the quartiles of the days shown.
        let mut shown: Vec<u64> = history.days.range(first..=last_day).map(|(_, &n)| n).collect();
        shown.sort_unstable();
        let q = |k: f64| shown.get(((shown.len() as f64 - 1.0) * k).round() as usize).copied().unwrap_or(0);
        let cuts = [q(0.25), q(0.5), q(0.75)];
        for (i, d) in ["Mon", "Wed", "Fri"].iter().enumerate() {
            self.text(c, x0 - 8.0, y0 + (i * 2) as f32 * (cell + gap) + cell / 2.0, d, 10.0, theme::faint(), Align::Right);
        }
        let mut month_seen = 0;
        let mut empty = Path::new();
        for week in 0..53 {
            let x = x0 + week as f32 * (cell + gap);
            let monday = first + Duration::weeks(week);
            if monday.month() != month_seen && monday.day() <= 7 {
                month_seen = monday.month();
                self.text(c, x, y0 - 10.0, &monday.format("%b").to_string(), 10.0, theme::faint(), Align::Left);
            }
            for dow in 0..7 {
                let day = monday + Duration::days(dow);
                if day > last_day {
                    break;
                }
                let r = Rect::new(x, y0 + dow as f32 * (cell + gap), cell, cell);
                let n = history.days.get(&day).copied().unwrap_or(0);
                if n == 0 {
                    empty.rounded_rect(r.x, r.y, r.w, r.h, 2.0);
                } else {
                    let level = cuts.iter().filter(|&&cut| n > cut).count();
                    let alpha = [90, 140, 195, 255][level];
                    let mut p = Path::new();
                    p.rounded_rect(r.x, r.y, r.w, r.h, 2.0);
                    c.fill_path(&p, &theme::gradient(x0, y0, x0 + 690.0, y0 + 90.0, alpha));
                }
                if day == today {
                    let mut p = Path::new();
                    p.rounded_rect(r.x - 1.0, r.y - 1.0, r.w + 2.0, r.h + 2.0, 3.0);
                    c.stroke_path(&p, &Paint::color(theme::text()).with_line_width(1.0));
                }
                let hovered = r.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
                if hovered {
                    let mut p = Path::new();
                    p.rounded_rect(r.x - 1.0, r.y - 1.0, r.w + 2.0, r.h + 2.0, 3.0);
                    c.stroke_path(&p, &Paint::color(Color::rgba(255, 255, 255, 160)).with_line_width(1.0));
                }
                let value = if n == 0 { "Not played".to_string() } else { amount(ui, n) };
                ui.tips.push((r, format!("{}\n{value}", day.format("%a %-d %b %Y"))));
                ui.regions.push((r, Action::ShowDay((day - today).num_days() as i32)));
            }
        }
        c.fill_path(&empty, &Paint::color(theme::hairline().with_alpha(16)));
        // Legend.
        let lx = x0 + 53.0 * (cell + gap) - 4.0;
        self.text(c, lx - 5.0 * (cell + gap) - 6.0, 240.0, "less", 10.0, theme::faint(), Align::Right);
        for (i, alpha) in [0u8, 90, 140, 195, 255].iter().enumerate() {
            let x = lx - (5 - i) as f32 * (cell + gap);
            let mut p = Path::new();
            p.rounded_rect(x, 240.0 - cell / 2.0, cell, cell, 2.0);
            if *alpha == 0 {
                c.fill_path(&p, &Paint::color(theme::hairline().with_alpha(16)));
            } else {
                c.fill_path(&p, &theme::gradient(x0, y0, x0 + 690.0, y0 + 90.0, *alpha));
            }
        }
        self.text(c, lx + 4.0, 240.0, "more", 10.0, theme::faint(), Align::Left);
    }

    fn draw_week_hours(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats, p: Rect) {
        self.panel(c, p, "When you play");
        let wh = &stats.week_hours;
        let max = wh.iter().flatten().copied().max().unwrap_or(0);
        if max == 0 {
            self.empty(c, p, "Nothing played in this range", "");
            return;
        }
        // The busiest weekday and part of the day.
        let day = (0..7).max_by_key(|&d| wh[d].iter().sum::<u64>()).unwrap_or(0);
        let part = (0..4).max_by_key(|&k| (0..7).map(|d| wh[d][k * 6..k * 6 + 6].iter().sum::<u64>()).sum::<u64>()).unwrap_or(0);
        let mostly = format!("mostly {}s, {}", weekday_name(day), part_of_day(part * 6));
        self.gradient_text(c, p.x + p.w - 14.0, p.y + 18.0, &mostly, 13.0, 200.0, Align::Right);
        let (x0, y0) = (p.x + 44.0, p.y + 36.0);
        let cw = (p.w - 58.0) / 24.0;
        let ch = 16.0;
        for (d, hours) in wh.iter().enumerate() {
            let y = y0 + d as f32 * (ch + 2.0);
            self.text(c, x0 - 8.0, y + ch / 2.0, &weekday_name(d)[..3], 10.5, theme::faint(), Align::Right);
            for (h, &n) in hours.iter().enumerate() {
                let r = Rect::new(x0 + h as f32 * cw, y, cw - 2.0, ch);
                let mut path = Path::new();
                path.rounded_rect(r.x, r.y, r.w, r.h, 2.0);
                if n == 0 {
                    c.fill_path(&path, &Paint::color(theme::hairline().with_alpha(12)));
                } else {
                    let a = (50.0 + 205.0 * (n as f32 / max as f32).sqrt()) as u8;
                    c.fill_path(&path, &theme::gradient(x0, y0, x0 + 24.0 * cw, y0 + 126.0, a));
                }
                if r.contains(ui.mouse) && matches!(ui.overlay, Overlay::None) {
                    c.stroke_path(&path, &Paint::color(Color::rgba(255, 255, 255, 150)).with_line_width(1.0));
                }
                let value = amount(ui, n);
                ui.tips.push((r, format!("{}s, {:02}:00–{:02}:00\n{value}", weekday_name(d), h, (h + 1) % 24)));
            }
        }
        for h in (0..24).step_by(3) {
            self.text(c, x0 + h as f32 * cw + (cw - 2.0) / 2.0, y0 + 7.0 * (ch + 2.0) + 8.0, &format!("{h}"), 10.0, theme::faint(), Align::Center);
        }
    }

    fn draw_bests(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, history: &stats::History, stats: &Stats, p: Rect) {
        self.panel(c, p, "Sessions");
        let n = stats.sessions.len();
        if n > 0 {
            let avg: f64 = stats.sessions.iter().map(|s| (s.end - s.start) as f64).sum::<f64>() / n as f64;
            self.text(c, p.x + 14.0, p.y + 42.0, &format!("{n} session{}  ·  {} on average", if n == 1 { "" } else { "s" }, theme::duration(avg)), 12.5, theme::dim(), Align::Left);
            if let Some(longest) = stats.sessions.iter().max_by_key(|s| s.end - s.start) {
                let when = Local.timestamp_opt(longest.start * 60, 0).single().map(|t| t.format("%a %-d %b").to_string()).unwrap_or_default();
                self.text(c, p.x + 14.0, p.y + 62.0, &format!("Longest {}  ({when})", theme::duration((longest.end - longest.start) as f64)), 12.5, theme::dim(), Align::Left);
            }
            let without = if ui.kit.is_audio() { "playing" } else { "a note" };
            ui.tips.push((Rect::new(p.x, p.y + 30.0, p.w, 44.0), format!("In this range\na session ends after {} minutes without {without}", stats::SESSION_GAP)));
        } else {
            self.text(c, p.x + 14.0, p.y + 42.0, "None in this range", 12.5, theme::dim(), Align::Left);
        }

        self.label(c, p.x + 14.0, p.y + 92.0, "Personal bests", theme::dim(), Align::Left);
        // (what, (amount, short date, full date)). An audio profile's busiest minute is just a
        // full one, so it has no best minute.
        type Best = Option<(String, String, String)>;
        let audio = ui.kit.is_audio();
        let rows: [(&str, Best); 3] = [
            ("Minute", history.best_minute.filter(|_| !audio).map(|(m, n)| {
                let date = Local.timestamp_opt(m * 60, 0).single().map(|t| t.format("%-d %b %Y").to_string()).unwrap_or_default();
                (amount(ui, n), date, minute_label(m))
            })),
            ("Day", history.best_day().map(|(d, n)| (amount(ui, n), d.format("%-d %b %Y").to_string(), d.format("%A %-d %B %Y").to_string()))),
            ("Week", history.best_week().map(|(d, n)| (amount(ui, n), d.format("%-d %b %Y").to_string(), format!("the week of {}", d.format("%-d %B %Y"))))),
        ];
        let rows: Vec<_> = rows.into_iter().filter(|(what, _)| !(audio && *what == "Minute")).collect();
        for (i, (what, best)) in rows.into_iter().enumerate() {
            let y = p.y + 116.0 + i as f32 * 22.0;
            self.text(c, p.x + 14.0, y, what, 12.5, theme::dim(), Align::Left);
            if let Some((value, short, full)) = best {
                self.gradient_text(c, p.x + 80.0, y, &value, 14.0, 120.0, Align::Left);
                self.text(c, p.x + p.w - 14.0, y, &short, 11.0, theme::faint(), Align::Right);
                ui.tips.push((Rect::new(p.x + 8.0, y - 11.0, p.w - 16.0, 22.0), format!("Best {}: {full}\n{value}", what.to_lowercase())));
            }
        }
    }

    // ---- Review ----

    pub(super) fn draw_review(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats) {
        let kind = match stats.query.span {
            Span::Hour => "The hour",
            Span::Day => "The day",
            Span::Week => "The week",
            Span::Month => "The month",
            Span::Year => "The year",
            Span::All => "All of it",
        };
        self.label(c, 16.0, 108.0, &format!("{kind} in review"), theme::faint(), Align::Left);
        let title = if stats.query.span == Span::Year { format!("Your {}", stats.title) } else { stats.title.clone() };
        let tw = self.measure(c, &title, 26.0, 200.0);
        self.big(c, 16.0, 136.0, &title, "", 26.0);
        let _ = tw;
        if stats.total == 0 {
            self.empty(c, Rect::new(16.0, 160.0, self.w() - 32.0, 270.0 + self.extra().1), "Nothing played in this range", "Pick another one above.");
            return;
        }
        let tiles = review_tiles(stats, ui);
        let (cols, gap) = (4usize, 8.0);
        let tw = (self.w() - 32.0 - gap * (cols as f32 - 1.0)) / cols as f32;
        let th = 84.0 + self.extra().1 / 3.0;
        for (i, (label, value, sub)) in tiles.iter().take(12).enumerate() {
            let (col, row) = (i % cols, i / cols);
            let r = Rect::new(16.0 + col as f32 * (tw + gap), 162.0 + row as f32 * (th + 6.0), tw, th);
            Self::rounded(c, r, 6.0, Some(theme::panel_fill()), Some(Paint::color(theme::hairline())));
            self.label(c, r.x + 12.0, r.y + 18.0, label, theme::faint(), Align::Left);
            let value = self.fit(c, value, 19.0, r.w - 24.0);
            self.gradient_text(c, r.x + 12.0, r.y + 45.0, &value, 19.0, r.w - 24.0, Align::Left);
            let sub = self.fit(c, sub, 11.5, r.w - 24.0);
            self.text(c, r.x + 12.0, r.y + 68.0, &sub, 11.5, theme::dim(), Align::Left);
        }
    }
}


/// The review's tiles: (label, value, detail), the ones with something to say.
fn review_tiles(s: &Stats, ui: &Ui) -> Vec<(String, String, String)> {
    let mut t: Vec<(String, String, String)> = Vec::new();
    let mut add = |l: &str, v: String, d: String| t.push((l.to_string(), v, d));
    let per_min = s.per_minute().map(|r| format!("{} a minute while playing", commas(r.round() as u64))).unwrap_or_default();
    add("Notes", commas(s.total), per_min);
    add("Time played", theme::duration(s.played_total), format!("{} logged", theme::duration(s.logged_total as f64)));
    let today = Local::now().date_naive();
    let first = s.edges.first().map(|e| e.date()).unwrap_or(today);
    let last = s.edges.last().map(|e| e.date() - Duration::days(1)).unwrap_or(today).min(today);
    let span_days = ((last - first).num_days() + 1).max(1);
    if s.query.span != Span::Hour && s.query.span != Span::Day {
        add("Days played", format!("{} of {}", s.days.len(), span_days), format!("{:.0}% of the days", s.days.len() as f64 / span_days as f64 * 100.0));
        let run = stats::longest_run(&s.days);
        if run >= 2 {
            add("Longest streak", format!("{run} days"), "in a row".into());
        }
        if let Some((d, n)) = s.days.iter().rev().max_by_key(|(_, &n)| n) {
            add("Busiest day", format!("{} notes", commas(*n)), d.format("%A %-d %B").to_string());
        }
    }
    let mut prime = (0, 0, 0u64);
    for (d, hours) in s.week_hours.iter().enumerate() {
        for (h, &n) in hours.iter().enumerate() {
            if n > prime.2 {
                prime = (d, h, n);
            }
        }
    }
    if prime.2 > 0 && s.query.span != Span::Hour {
        add("Prime time", format!("{}s, {:02}:00", &weekday_name(prime.0)[..3], prime.1), format!("{} notes in that hour of the week", commas(prime.2)));
    }
    if ui.kit.is_drums() {
        if let Some((name, n)) = ui.kit.drum_counts(&s.notes).into_iter().filter(|(n, _)| n != "Other").max_by_key(|(_, n)| *n) {
            add("Favourite drum", name, format!("{:.0}% of your hits", n as f64 / s.total.max(1) as f64 * 100.0));
        }
    } else {
        let pc = stats::pitch_classes(&s.notes);
        if let Some(scale) = stats::likely_scale(&pc) {
            add("Key", scale, "the scale that fits your notes best".into());
        } else if let Some((i, _)) = pc.iter().enumerate().max_by_key(|(_, &n)| n) {
            add("Favourite note", stats::NOTE_NAMES[i].to_string(), "the pitch class you played most".into());
        }
    }
    if let Some(sum) = stats::summarize(&s.detail.tempo) {
        add("Tempo", format!("{:.0} BPM", sum.mode), format!("most often · median {:.0}", sum.median));
    }
    let d = &s.detail;
    if d.timing.0 > 0 {
        let ms = d.timing.1 as f64 / d.timing.0 as f64 / 10.0;
        add("Timing", format!("±{ms:.1} ms"), steadiness(ms).to_string());
    }
    if d.swing.0 >= MIN_SAMPLES {
        add("Swing", format!("{:.0}%", d.swing.1 as f64 / d.swing.0 as f64 / 10.0), "50% straight, 67% triplet".into());
    }
    if let Some((note, r)) = d.speed {
        let rate = r as f64 / 10.0;
        if ui.kit.is_drums() {
            add("Top speed", format!("16ths at {:.0}", sixteenths_bpm(rate)), format!("BPM, on {}", ui.kit.drum_label(note)));
        } else {
            add("Top speed", format!("{rate:.1} a second"), "notes, eight in a row".into());
        }
    }
    if ui.kit.is_drums() {
        if d.rolls + d.fills > 0 {
            add("Rolls and fills", format!("{} · {}", commas(d.rolls as u64), commas(d.fills as u64)), "rolls · fills".into());
        }
    } else if let Some((&mask, &n)) = d.chords.iter().max_by_key(|(_, &n)| n) {
        add("Favourite chord", stats::chord_name(mask), format!("played {} times", commas(n as u64)));
    }
    if let Some(longest) = s.sessions.iter().max_by_key(|x| x.end - x.start) {
        add("Sessions", s.sessions.len().to_string(), format!("longest {}", theme::duration((longest.end - longest.start) as f64)));
    }
    if let Some((m, n)) = s.busiest {
        let when = Local.timestamp_opt(m * 60, 0).single().map(|t| t.format("%a %-d %b, %H:%M").to_string()).unwrap_or_default();
        add("Busiest minute", format!("{} notes", commas(n)), when);
    }
    t
}

/// An amount of a profile's unit: notes, or time for an audio profile (which logs seconds).
fn amount(ui: &Ui, n: u64) -> String {
    if ui.kit.is_audio() { super::seconds_text(n) } else { format!("{} notes", commas(n)) }
}
