// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! The plugin window: baseview + femtovg (OpenGL).
//!
//! It draws only when something changed: a 30 Hz timer checks one atomic counter for new hits,
//! and mouse movement redraws only when what's under the mouse changes.

mod kit;
pub mod theme;

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use baseview::gl::{GlConfig, GlContext};
use baseview::{
    Event, EventStatus, HandlerError, MouseButton, MouseCursor, MouseEvent, ScrollDelta, TimerHandle,
    WindowContext, WindowHandler, WindowSettings, WindowSize,
};
use femtovg::renderer::OpenGl;
use femtovg::{Align, Baseline, Canvas, Color, FontId, Paint, Path};
use keyboard_types::{Key, KeyState, NamedKey};

use crate::kit::{Kit, RETRIGGER_CHOICES};
use crate::session::{Shared, Snapshot};
use crate::stats::{self, Bucket, Query, Span, Stats};
use theme::{commas, WithAlpha, HEIGHT, WIDTH};

#[derive(Clone, Default)]
pub struct Options {
    /// Save a PNG of the first frame here, then exit (for the preview tool).
    pub screenshot: Option<PathBuf>,
    /// Open on the stats view (for the preview tool).
    pub start_in_stats: bool,
    /// Open on the drums view (for the preview tool).
    pub start_in_kit: bool,
}

/// Window settings to try, best first: multisampled, then plain (some drivers and virtual displays
/// have no multisampled configs).
pub fn window_settings(scale: f64) -> Vec<WindowSettings> {
    let w = (WIDTH as f64 * scale).round() as u32;
    let h = (HEIGHT as f64 * scale).round() as u32;
    let base = WindowSettings::new()
        .with_title("Modometer")
        .with_size(baseview::dpi::PhysicalSize::new(w, h))
        .with_resizable(true);
    [Some(4), None]
        .into_iter()
        .flat_map(|samples| {
            [true, false].into_iter().map(move |srgb| GlConfig { samples, srgb, ..GlConfig::default() })
        })
        .map(|gl| base.clone().with_gl_config(gl))
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn new(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, w, h }
    }
    fn contains(&self, (px, py): (f32, f32)) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

#[derive(Clone, PartialEq, Debug)]
enum Action {
    OpenProfiles,
    CloseOverlay,
    SelectProfile(String),
    NewProfile,
    CreateProfile,
    Field,
    ShowStats,
    ShowCounter,
    Span(Span),
    Bucket(Bucket),
    Prev,
    Next,
    NoteMode(NoteMode),
    ChartMode(ChartMode),
    ShowKit,
    Learn,
    /// Open the naming dialog for a note.
    PickNote(u8),
    Preset(&'static str),
    SaveDrum,
    RemoveDrum(u8),
    Retrigger(i32),
    /// Swallows clicks on a panel.
    None,
}

#[derive(PartialEq)]
enum View {
    Counter,
    Stats,
    Kit,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum NoteMode {
    PitchClass,
    All,
    Drums,
}

/// What the timeline shows per bucket.
#[derive(Clone, Copy, PartialEq, Debug)]
enum ChartMode {
    Notes,
    Time,
}

/// A profile's pace (see `stats::pace`), and the minute it was worked out.
type PaceCache = Option<(String, i64, Option<f64>)>;

/// How long a cached pace is used before it's worked out again, in minutes.
const PACE_MAX_AGE: i64 = 60;

enum Overlay {
    None,
    Profiles { scroll: usize },
    NewProfile { text: String, error: Option<String> },
    /// Waiting for a hit; `since` is the monitor's sequence number when Learn was pressed.
    Learn { since: u32 },
    /// Naming `note`. `edited` once the user typed or picked a name.
    NameDrum { note: u8, text: String, edited: bool, error: Option<String> },
}

struct Ui {
    view: View,
    overlay: Overlay,
    mouse: (f32, f32),
    /// Clickable regions from the last frame, topmost last.
    regions: Vec<(Rect, Action)>,
    /// Tooltip regions from the last frame.
    tips: Vec<(Rect, String)>,
    /// What was under the mouse when the last frame was drawn.
    drawn_hover: (Option<usize>, Option<usize>),
    snapshot: Snapshot,
    seen_hits: u64,
    seen_generation: u64,
    profiles: Vec<String>,
    query: Query,
    stats: Option<Stats>,
    incoming: Arc<Mutex<Option<Stats>>>,
    loading: bool,
    pace: Arc<Mutex<PaceCache>>,
    note_mode: NoteMode,
    chart_mode: ChartMode,
    /// The mouse cursor last set on the window.
    cursor: MouseCursor,
    kit: Kit,
    kit_scroll: usize,
    seen_recent: u32,
    screenshot_done: bool,
}

pub struct Editor {
    ctx: WindowContext,
    gl: GlContext,
    canvas: RefCell<Canvas<OpenGl>>,
    font: FontId,
    shared: Arc<Shared>,
    ui: RefCell<Ui>,
    timer: RefCell<Option<TimerHandle>>,
    options: Options,
}

fn handler_error(msg: impl Into<String>) -> HandlerError {
    let msg: String = msg.into();
    HandlerError::from_boxed(msg.into())
}

impl Editor {
    pub fn new(ctx: WindowContext, shared: Arc<Shared>, options: Options) -> Result<Editor, HandlerError> {
        let gl = ctx.gl_context().ok_or_else(|| handler_error("no OpenGL context"))?;
        unsafe { gl.make_current() }.map_err(|e| handler_error(e.to_string()))?;
        let renderer = unsafe { OpenGl::new_from_function_cstr(|s| gl.get_proc_address(s)) }
            .map_err(|e| handler_error(format!("{e:?}")))?;
        let mut canvas = Canvas::new(renderer).map_err(|e| handler_error(format!("{e:?}")))?;
        let font = canvas.add_font_mem(theme::FONT).map_err(|e| handler_error(format!("{e:?}")))?;
        let timer = ctx.create_timer(Duration::from_millis(33)).ok();
        // Give the window its own cursor. Without one, X11 shows the parent's, so the pointer keeps
        // whatever shape it had outside (a resize arrow, say). baseview assumes the arrow is already
        // set and skips setting it again, hence the detour.
        let _ = ctx.set_mouse_cursor(MouseCursor::Text);
        let _ = ctx.set_mouse_cursor(MouseCursor::Default);

        let snapshot = shared.snapshot();
        let span = Span::Week;
        let ui = Ui {
            view: if options.start_in_stats {
                View::Stats
            } else if options.start_in_kit {
                View::Kit
            } else {
                View::Counter
            },
            overlay: Overlay::None,
            mouse: (-1.0, -1.0),
            regions: Vec::new(),
            tips: Vec::new(),
            drawn_hover: (None, None),
            snapshot,
            seen_hits: shared.hits_seen(),
            seen_generation: shared.generation.load(std::sync::atomic::Ordering::Relaxed),
            profiles: shared.store().list_profiles(),
            query: Query { span, offset: 0, bucket: span.default_bucket() },
            stats: None,
            incoming: Arc::new(Mutex::new(None)),
            loading: false,
            pace: Arc::new(Mutex::new(None)),
            note_mode: NoteMode::PitchClass,
            chart_mode: ChartMode::Notes,
            kit: shared.kit(),
            cursor: MouseCursor::Default,
            kit_scroll: 0,
            seen_recent: shared.recent_seq(),
            screenshot_done: false,
        };
        let editor = Editor {
            ctx,
            gl,
            canvas: RefCell::new(canvas),
            font,
            shared,
            ui: RefCell::new(ui),
            timer: RefCell::new(timer),
            options,
        };
        if editor.ui.borrow().view == View::Stats {
            editor.load_stats(&mut editor.ui.borrow_mut());
        }
        Ok(editor)
    }

    /// How the 800 × 450 layout maps onto the window: (scale, x offset, y offset). The layout keeps
    /// its shape at any window size; a window of another shape gets even margins.
    fn layout(&self) -> (f32, f32, f32) {
        let size = self.ctx.size();
        fit(size.physical.width as f32, size.physical.height as f32)
    }

    /// Hand over things you can click, an I-beam over text fields, the arrow elsewhere.
    fn update_cursor(&self, ui: &mut Ui) {
        let cursor = match ui.regions.iter().rev().find(|(r, _)| r.contains(ui.mouse)).map(|(_, a)| a) {
            Some(Action::Field) => MouseCursor::Text,
            Some(Action::None | Action::CloseOverlay) | None => MouseCursor::Default,
            Some(_) => MouseCursor::Hand,
        };
        if ui.cursor != cursor {
            ui.cursor = cursor;
            let _ = self.ctx.set_mouse_cursor(cursor);
        }
    }

    fn load_stats(&self, ui: &mut Ui) {
        ui.loading = true;
        let shared = Arc::clone(&self.shared);
        let incoming = Arc::clone(&ui.incoming);
        let waker = self.ctx.waker();
        let query = ui.query;
        let pace_cache = Arc::clone(&ui.pace);
        std::thread::spawn(move || {
            let pending = shared.pending();
            let profile = shared.profile_name();
            let now = crate::store::now_minute();
            let cached = pace_cache.lock().unwrap().clone();
            let pace = match cached {
                Some((p, at, pace)) if p == profile && now - at < PACE_MAX_AGE => pace,
                _ => {
                    let pace = stats::pace(shared.store(), &profile, &pending);
                    *pace_cache.lock().unwrap() = Some((profile.clone(), now, pace));
                    pace
                }
            };
            let s = stats::compute(shared.store(), &profile, query, &pending, pace);
            *incoming.lock().unwrap() = Some(s);
            waker.request_redraw();
        });
    }

    fn hover_state(ui: &Ui) -> (Option<usize>, Option<usize>) {
        let region = ui.regions.iter().rposition(|(r, _)| r.contains(ui.mouse));
        let tip = ui.tips.iter().rposition(|(r, _)| r.contains(ui.mouse));
        (region, tip)
    }

    fn act(&self, ui: &mut Ui, action: Action) {
        match action {
            Action::OpenProfiles => {
                ui.profiles = self.shared.store().list_profiles();
                ui.overlay = Overlay::Profiles { scroll: 0 };
            }
            Action::CloseOverlay => ui.overlay = Overlay::None,
            Action::SelectProfile(name) => {
                self.shared.select_profile(&name);
                ui.overlay = Overlay::None;
                if ui.view == View::Stats {
                    self.load_stats(ui);
                }
            }
            Action::NewProfile => {
                ui.overlay = Overlay::NewProfile { text: String::new(), error: None };
                let _ = self.ctx.focus();
            }
            Action::CreateProfile => self.create_profile(ui),
            Action::Field => {
                let _ = self.ctx.focus();
            }
            Action::ShowStats => {
                ui.view = View::Stats;
                self.load_stats(ui);
            }
            Action::ShowCounter => ui.view = View::Counter,
            Action::Span(span) => {
                ui.query = Query { span, offset: 0, bucket: span.default_bucket() };
                self.load_stats(ui);
            }
            Action::Bucket(bucket) => {
                ui.query.bucket = bucket;
                self.load_stats(ui);
            }
            Action::Prev => {
                ui.query.offset -= 1;
                self.load_stats(ui);
            }
            Action::Next => {
                ui.query.offset = (ui.query.offset + 1).min(0);
                self.load_stats(ui);
            }
            Action::NoteMode(mode) => ui.note_mode = mode,
            Action::ChartMode(mode) => ui.chart_mode = mode,
            Action::ShowKit => ui.view = View::Kit,
            Action::Learn => {
                ui.overlay = Overlay::Learn { since: self.shared.recent_seq() };
            }
            Action::PickNote(note) => {
                let text = ui.kit.name(note).unwrap_or("").to_string();
                ui.overlay = Overlay::NameDrum { note, edited: !text.is_empty(), text, error: None };
                let _ = self.ctx.focus();
            }
            Action::Preset(name) => {
                if let Overlay::NameDrum { text, edited, error, .. } = &mut ui.overlay {
                    *text = name.to_string();
                    *edited = true;
                    *error = None;
                }
            }
            Action::SaveDrum => self.save_drum(ui),
            Action::RemoveDrum(note) => {
                let _ = self.shared.set_drum_name(note, None);
                ui.kit = self.shared.kit();
                if matches!(ui.overlay, Overlay::NameDrum { .. }) {
                    ui.overlay = Overlay::None;
                }
            }
            Action::Retrigger(dir) => {
                let ms = ui.kit.retrigger_ms;
                let i = RETRIGGER_CHOICES.iter().position(|&c| c >= ms).unwrap_or(RETRIGGER_CHOICES.len() - 1);
                // `i` is the first choice >= ms (ms may be an in-between value edited by hand).
                let i = if dir < 0 {
                    i.saturating_sub(1)
                } else if RETRIGGER_CHOICES[i] > ms {
                    i
                } else {
                    (i + 1).min(RETRIGGER_CHOICES.len() - 1)
                };
                self.shared.set_retrigger_ms(RETRIGGER_CHOICES[i]);
                ui.kit = self.shared.kit();
            }
            Action::None => {}
        }
    }

    fn create_profile(&self, ui: &mut Ui) {
        if let Overlay::NewProfile { text, error } = &mut ui.overlay {
            match self.shared.create_profile(text) {
                Ok(_) => {
                    ui.overlay = Overlay::None;
                    ui.profiles = self.shared.store().list_profiles();
                    if ui.view == View::Stats {
                        self.load_stats(ui);
                    }
                }
                Err(e) => *error = Some(e),
            }
        }
    }

    fn save_drum(&self, ui: &mut Ui) {
        if let Overlay::NameDrum { note, text, error, .. } = &mut ui.overlay {
            match self.shared.set_drum_name(*note, Some(text)) {
                Ok(()) => {
                    ui.overlay = Overlay::None;
                    ui.kit = self.shared.kit();
                }
                Err(e) => *error = Some(e),
            }
        }
    }

    /// A note-on arrived while learning or naming: (re)target the dialog at it.
    fn on_recent_hit(&self, ui: &mut Ui) {
        let Some(hit) = self.shared.recent_hits().first().copied() else { return };
        let kit_name = ui.kit.name(hit.note).unwrap_or("").to_string();
        match &mut ui.overlay {
            Overlay::Learn { since } if self.shared.recent_seq() != *since => {
                ui.overlay = Overlay::NameDrum { note: hit.note, edited: !kit_name.is_empty(), text: kit_name, error: None };
                let _ = self.ctx.focus();
            }
            Overlay::NameDrum { note, text, edited, error } if *note != hit.note => {
                *note = hit.note;
                *error = None;
                if !*edited || text.is_empty() {
                    *edited = !kit_name.is_empty();
                    *text = kit_name;
                }
            }
            _ => {}
        }
    }

    // ---- Drawing ----

    fn paint_text(&self, size: f32, weight: f32, color: Color, align: Align) -> Paint {
        Paint::color(color)
            .with_font(&[self.font])
            .with_font_size(size)
            .with_font_weight(weight)
            .with_text_align(align)
            .with_text_baseline(Baseline::Middle)
    }

    fn text(&self, c: &mut Canvas<OpenGl>, x: f32, y: f32, s: &str, size: f32, color: Color, align: Align) -> f32 {
        let paint = self.paint_text(size, 300.0, color, align);
        c.fill_text(x, y, s, &paint).map(|m| m.width()).unwrap_or(0.0)
    }

    /// Small letter-spaced capitals, for headings.
    fn label(&self, c: &mut Canvas<OpenGl>, x: f32, y: f32, s: &str, color: Color, align: Align) -> f32 {
        let paint = self.paint_text(10.5, 400.0, color, align).with_letter_spacing(1.6);
        c.fill_text(x, y, s.to_uppercase(), &paint).map(|m| m.width()).unwrap_or(0.0)
    }

    fn measure(&self, c: &Canvas<OpenGl>, s: &str, size: f32, weight: f32) -> f32 {
        let paint = self.paint_text(size, weight, Color::white(), Align::Left);
        c.measure_text(0.0, 0.0, s, &paint).map(|m| m.width()).unwrap_or(0.0)
    }

    fn rounded(c: &mut Canvas<OpenGl>, r: Rect, radius: f32, fill: Option<Color>, stroke: Option<Paint>) {
        let mut path = Path::new();
        path.rounded_rect(r.x, r.y, r.w, r.h, radius);
        if let Some(f) = fill {
            c.fill_path(&path, &Paint::color(f));
        }
        if let Some(s) = stroke {
            c.stroke_path(&path, &s.with_line_width(1.0));
        }
    }

    fn hline(c: &mut Canvas<OpenGl>, x0: f32, x1: f32, y: f32, color: Color) {
        let mut path = Path::new();
        path.move_to(x0, y + 0.5);
        path.line_to(x1, y + 0.5);
        c.stroke_path(&path, &Paint::color(color).with_line_width(1.0));
    }

    fn chevron(c: &mut Canvas<OpenGl>, x: f32, y: f32, size: f32, dir: i32, color: Color) {
        // dir: 0 down, -1 left, 1 right
        let mut path = Path::new();
        match dir {
            0 => {
                path.move_to(x - size, y - size * 0.5);
                path.line_to(x, y + size * 0.5);
                path.line_to(x + size, y - size * 0.5);
            }
            d => {
                let d = d as f32;
                path.move_to(x - size * 0.5 * d, y - size);
                path.line_to(x + size * 0.5 * d, y);
                path.line_to(x - size * 0.5 * d, y + size);
            }
        }
        c.stroke_path(&path, &Paint::color(color).with_line_width(1.2));
    }

    /// A text button; returns its rect. Highlighted when hovered or `selected`.
    fn button(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, r: Rect, s: &str, selected: bool, action: Action) -> Rect {
        let hovered = r.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
        if hovered {
            Self::rounded(c, r, 4.0, Some(theme::hover_fill()), None);
        }
        let color = if selected || hovered { theme::text() } else { theme::dim() };
        self.text(c, r.x + r.w / 2.0, r.y + r.h / 2.0, s, 13.5, color, Align::Center);
        if selected {
            let mut path = Path::new();
            path.rect(r.x + r.w * 0.2, r.y + r.h - 2.0, r.w * 0.6, 1.5);
            c.fill_path(&path, &theme::window_gradient(255));
        }
        ui.regions.push((r, action));
        r
    }

    fn draw_counter(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui) {
        let w = WIDTH as f32;
        // Profile picker, top left.
        let name = ui.snapshot.profile.clone();
        let name_w = self.measure(c, &name, 15.0, 300.0);
        let r = Rect::new(16.0, 14.0, name_w + 40.0, 32.0);
        let hovered = r.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
        if hovered || matches!(ui.overlay, Overlay::Profiles { .. }) {
            Self::rounded(c, r, 4.0, Some(theme::hover_fill()), None);
        }
        self.text(c, 28.0, r.y + r.h / 2.0, &name, 15.0, theme::text(), Align::Left);
        Self::chevron(c, 28.0 + name_w + 14.0, r.y + r.h / 2.0, 3.5, 0, theme::dim());
        ui.regions.push((r, Action::OpenProfiles));

        // Stats, top right.
        let r = Rect::new(w - 100.0, 14.0, 84.0, 32.0);
        let hovered = r.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
        if hovered {
            Self::rounded(c, r, 4.0, Some(theme::hover_fill()), None);
        }
        let color = if hovered { theme::text() } else { theme::dim() };
        let (ix, iy) = (r.x + 16.0, r.y + 22.0);
        for (i, h) in [6.0, 11.0, 8.0].iter().enumerate() {
            let mut p = Path::new();
            p.rect(ix + i as f32 * 4.5, iy - h, 2.0, *h);
            c.fill_path(&p, &Paint::color(color));
        }
        self.text(c, r.x + 34.0, r.y + r.h / 2.0, "Stats", 14.0, color, Align::Left);
        ui.regions.push((r, Action::ShowStats));

        // Drums, left of Stats.
        let r = Rect::new(w - 196.0, 14.0, 92.0, 32.0);
        let hovered = r.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
        if hovered {
            Self::rounded(c, r, 4.0, Some(theme::hover_fill()), None);
        }
        let color = if hovered { theme::text() } else { theme::dim() };
        let (ix, iy) = (r.x + 21.0, r.y + r.h / 2.0);
        let mut p = Path::new();
        p.ellipse(ix, iy - 3.0, 6.5, 2.5);
        p.move_to(ix - 6.5, iy - 3.0);
        p.line_to(ix - 6.5, iy + 4.0);
        p.move_to(ix + 6.5, iy - 3.0);
        p.line_to(ix + 6.5, iy + 4.0);
        c.stroke_path(&p, &Paint::color(color).with_line_width(1.1));
        let mut p = Path::new();
        p.move_to(ix - 6.5, iy + 4.0);
        p.quad_to(ix, iy + 8.0, ix + 6.5, iy + 4.0);
        c.stroke_path(&p, &Paint::color(color).with_line_width(1.1));
        self.text(c, r.x + 36.0, r.y + r.h / 2.0, "Drums", 14.0, color, Align::Left);
        ui.regions.push((r, Action::ShowKit));

        // The count.
        let s = commas(ui.snapshot.total);
        let mut size = 132.0;
        let tw = self.measure(c, &s, size, 200.0);
        if tw > w - 80.0 {
            size *= (w - 80.0) / tw;
        }
        let tw = self.measure(c, &s, size, 200.0);
        let (cx, cy) = (w / 2.0, 208.0);
        let paint = theme::gradient(cx - tw / 2.0, cy - size * 0.4, cx + tw / 2.0, cy + size * 0.4, 255)
            .with_font(&[self.font])
            .with_font_size(size)
            .with_font_weight(200.0)
            .with_text_align(Align::Center)
            .with_text_baseline(Baseline::Middle);
        let _ = c.fill_text(cx, cy, &s, &paint);

        self.label(c, cx, 300.0, "notes played", theme::dim(), Align::Center);
        let today = format!("Today  {}", commas(ui.snapshot.today));
        self.text(c, cx, 332.0, &today, 14.0, theme::faint().with_alpha(170), Align::Center);
    }

    fn draw_profiles(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, scroll: &mut usize) {
        // Click outside closes.
        ui.regions.push((Rect::new(0.0, 0.0, WIDTH as f32, HEIGHT as f32), Action::CloseOverlay));
        let current = ui.snapshot.profile.clone();
        let item_h = 32.0;
        let max_items = 10usize;
        let n = ui.profiles.len();
        *scroll = (*scroll).min(n.saturating_sub(max_items));
        let shown = n.min(max_items);
        let panel = Rect::new(16.0, 50.0, 280.0, shown as f32 * item_h + item_h + 13.0);
        Self::rounded(c, panel, 6.0, Some(theme::popup_fill()), Some(Paint::color(theme::hairline())));
        ui.regions.push((panel, Action::None));
        let mut y = panel.y + 4.0;
        for name in ui.profiles.iter().skip(*scroll).take(shown).cloned().collect::<Vec<_>>() {
            let r = Rect::new(panel.x + 4.0, y, panel.w - 8.0, item_h);
            if r.contains(ui.mouse) {
                Self::rounded(c, r, 4.0, Some(theme::hover_fill()), None);
            }
            let selected = name == current;
            if selected {
                let mut p = Path::new();
                p.circle(r.x + 14.0, r.y + r.h / 2.0, 2.5);
                c.fill_path(&p, &theme::window_gradient(255));
            }
            let color = if selected { theme::text() } else { theme::text().with_alpha(170) };
            self.text(c, r.x + 28.0, r.y + r.h / 2.0, &name, 14.0, color, Align::Left);
            ui.regions.push((r, Action::SelectProfile(name)));
            y += item_h;
        }
        Self::hline(c, panel.x + 10.0, panel.x + panel.w - 10.0, y + 2.0, theme::hairline());
        let r = Rect::new(panel.x + 4.0, y + 5.0, panel.w - 8.0, item_h);
        let hovered = r.contains(ui.mouse);
        if hovered {
            Self::rounded(c, r, 4.0, Some(theme::hover_fill()), None);
        }
        self.text(c, r.x + 28.0, r.y + r.h / 2.0, "New profile…", 14.0, if hovered { theme::text() } else { theme::dim() }, Align::Left);
        let mut p = Path::new();
        p.move_to(r.x + 10.0, r.y + r.h / 2.0);
        p.line_to(r.x + 18.0, r.y + r.h / 2.0);
        p.move_to(r.x + 14.0, r.y + r.h / 2.0 - 4.0);
        p.line_to(r.x + 14.0, r.y + r.h / 2.0 + 4.0);
        c.stroke_path(&p, &Paint::color(theme::dim()).with_line_width(1.2));
        ui.regions.push((r, Action::NewProfile));
    }

    fn draw_new_profile(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, text: &str, error: Option<&str>) {
        let (w, h) = (WIDTH as f32, HEIGHT as f32);
        let mut p = Path::new();
        p.rect(0.0, 0.0, w, h);
        c.fill_path(&p, &Paint::color(Color::rgba(0, 0, 0, 150)));
        ui.regions.push((Rect::new(0.0, 0.0, w, h), Action::None));
        let panel = Rect::new(w / 2.0 - 190.0, h / 2.0 - 95.0, 380.0, 190.0);
        Self::rounded(c, panel, 8.0, Some(theme::popup_fill()), Some(theme::window_gradient(140)));
        self.label(c, panel.x + 24.0, panel.y + 30.0, "New profile", theme::dim(), Align::Left);

        let field = Rect::new(panel.x + 24.0, panel.y + 52.0, panel.w - 48.0, 38.0);
        Self::rounded(c, field, 4.0, Some(Color::rgba(0, 0, 0, 120)), Some(Paint::color(theme::hairline())));
        ui.regions.push((field, Action::Field));
        let tw = if text.is_empty() {
            self.text(c, field.x + 12.0, field.y + field.h / 2.0, "Drums, Keys, …", 15.0, theme::faint(), Align::Left);
            0.0
        } else {
            self.text(c, field.x + 12.0, field.y + field.h / 2.0, text, 15.0, theme::text(), Align::Left)
        };
        let mut caret = Path::new();
        caret.rect(field.x + 12.0 + tw + 1.0, field.y + 10.0, 1.0, field.h - 20.0);
        c.fill_path(&caret, &theme::window_gradient(255));

        if let Some(e) = error {
            self.text(c, panel.x + 24.0, panel.y + 108.0, e, 12.5, Color::rgba(240, 150, 170, 220), Align::Left);
        }
        let cancel = Rect::new(panel.x + panel.w - 210.0, panel.y + panel.h - 54.0, 88.0, 32.0);
        let create = Rect::new(panel.x + panel.w - 112.0, panel.y + panel.h - 54.0, 88.0, 32.0);
        for (r, s, a) in [(cancel, "Cancel", Action::CloseOverlay), (create, "Create", Action::CreateProfile)] {
            let hovered = r.contains(ui.mouse);
            let stroke = if a == Action::CreateProfile { theme::window_gradient(200) } else { Paint::color(theme::hairline()) };
            Self::rounded(c, r, 4.0, hovered.then(theme::hover_fill), Some(stroke));
            self.text(c, r.x + r.w / 2.0, r.y + r.h / 2.0, s, 13.5, if hovered { theme::text() } else { theme::dim() }, Align::Center);
            ui.regions.push((r, a));
        }
    }

    /// Bars in `area`, with hover tips. `labels` gives each bar's tooltip title.
    fn bars(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, area: Rect, values: &[u64], tip: &dyn Fn(usize) -> String) {
        self.bars_with(c, ui, area, values, &|i| (tip(i), commas(values[i])));
    }

    /// Bars in `area`; `tip` gives each bar's tooltip title and value.
    fn bars_with(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, area: Rect, values: &[u64], tip: &dyn Fn(usize) -> (String, String)) {
        let n = values.len().max(1);
        let max = values.iter().copied().max().unwrap_or(0).max(1) as f32;
        let slot = area.w / n as f32;
        let gap = if slot >= 6.0 { (slot * 0.18).clamp(1.0, 6.0) } else if slot >= 3.0 { 1.0 } else { 0.0 };
        let hovered = if area.contains(ui.mouse) && matches!(ui.overlay, Overlay::None) {
            Some((((ui.mouse.0 - area.x) / slot) as usize).min(n - 1))
        } else {
            None
        };
        // Wide slots get a narrower bar, centred.
        let bar_w = (slot - gap).max(0.6).min(44.0);
        let inset = (slot - bar_w) / 2.0;
        let mut path = Path::new();
        for (i, &v) in values.iter().enumerate() {
            if v == 0 {
                continue;
            }
            let bh = (v as f32 / max * area.h).max(1.0);
            path.rect(area.x + i as f32 * slot + inset, area.y + area.h - bh, bar_w, bh);
        }
        let bar_paint = theme::gradient(area.x, area.y, area.x + area.w, area.y + area.h, 210);
        c.fill_path(&path, &bar_paint);
        if let Some(i) = hovered {
            let v = values[i];
            let bh = (v as f32 / max * area.h).max(if v > 0 { 1.0 } else { 0.0 });
            let mut col = Path::new();
            col.rect(area.x + i as f32 * slot, area.y, slot.max(1.0), area.h);
            c.fill_path(&col, &Paint::color(theme::hover_fill()));
            if v > 0 {
                let mut p = Path::new();
                p.rect(area.x + i as f32 * slot + inset, area.y + area.h - bh, bar_w.max(1.0), bh);
                c.fill_path(&p, &Paint::color(Color::rgba(255, 255, 255, 70)));
            }
        }
        for i in 0..n.min(values.len()) {
            let (title, value) = tip(i);
            ui.tips.push((Rect::new(area.x + i as f32 * slot, area.y, slot, area.h), format!("{title}\n{value}")));
        }
    }

    fn draw_tooltip(&self, c: &mut Canvas<OpenGl>, ui: &Ui) {
        if !matches!(ui.overlay, Overlay::None) {
            return;
        }
        let Some(i) = ui.tips.iter().rposition(|(r, _)| r.contains(ui.mouse)) else { return };
        let text = &ui.tips[i].1;
        let (title, value) = text.split_once('\n').unwrap_or((text, ""));
        let w = self.measure(c, title, 12.5, 300.0).max(self.measure(c, value, 15.0, 300.0)) + 24.0;
        let h = 46.0;
        let mut x = ui.mouse.0 + 14.0;
        if x + w > WIDTH as f32 - 8.0 {
            x = ui.mouse.0 - 14.0 - w;
        }
        let y = (ui.mouse.1 - h - 10.0).max(8.0);
        let r = Rect::new(x, y, w, h);
        Self::rounded(c, r, 5.0, Some(theme::popup_fill()), Some(Paint::color(theme::hairline())));
        self.text(c, x + 12.0, y + 15.0, title, 12.5, theme::dim(), Align::Left);
        let paint = theme::gradient(x, y, x + w, y + h, 255)
            .with_font(&[self.font])
            .with_font_size(15.0)
            .with_font_weight(300.0)
            .with_text_baseline(Baseline::Middle);
        let _ = c.fill_text(x + 12.0, y + 32.0, value, &paint);
    }

    fn draw_stats(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui) {
        let w = WIDTH as f32;
        // Top bar: back, profile, spans.
        self.top_bar(c, ui);

        let mut x = w - 16.0;
        for span in stats::SPANS.iter().rev() {
            let bw = 58.0;
            x -= bw;
            let selected = ui.query.span == *span;
            self.button(c, ui, Rect::new(x, 12.0, bw, 32.0), span.label(), selected, Action::Span(*span));
        }
        Self::hline(c, 16.0, w - 16.0, 52.0, theme::hairline());

        // Range row: ‹ title › on the left, bucket choice on the right.
        let q = ui.query;
        let (title, total) = match &ui.stats {
            Some(s) => (s.title.clone(), s.total),
            None => (String::new(), 0),
        };
        let mut x = 16.0;
        if q.span != Span::All {
            let r = Rect::new(x, 62.0, 28.0, 30.0);
            let hovered = r.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
            if hovered {
                Self::rounded(c, r, 4.0, Some(theme::hover_fill()), None);
            }
            Self::chevron(c, r.x + 14.0, r.y + 15.0, 5.0, -1, if hovered { theme::text() } else { theme::dim() });
            ui.regions.push((r, Action::Prev));
            x += 32.0;
        }
        let tw = self.text(c, x + 4.0, 77.0, &title, 15.0, theme::text(), Align::Left);
        x += tw + 12.0;
        if q.span != Span::All {
            let enabled = q.offset < 0;
            let r = Rect::new(x, 62.0, 28.0, 30.0);
            let hovered = enabled && r.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
            if hovered {
                Self::rounded(c, r, 4.0, Some(theme::hover_fill()), None);
            }
            let col = if !enabled { theme::faint().with_alpha(40) } else if hovered { theme::text() } else { theme::dim() };
            Self::chevron(c, r.x + 14.0, r.y + 15.0, 5.0, 1, col);
            if enabled {
                ui.regions.push((r, Action::Next));
            }
        }
        if ui.stats.is_some() {
            let total_s = format!("{} notes", commas(total));
            let tx = x + if q.span != Span::All { 44.0 } else { 8.0 };
            let paint = theme::gradient(tx, 0.0, tx + 160.0, 0.0, 255)
                .with_font(&[self.font])
                .with_font_size(15.0)
                .with_font_weight(300.0)
                .with_text_baseline(Baseline::Middle);
            let _ = c.fill_text(tx, 77.0, &total_s, &paint);
        }
        let buckets = q.span.buckets();
        let mut bx = w - 16.0;
        for b in buckets.iter().rev() {
            let bw = 62.0;
            bx -= bw;
            self.button(c, ui, Rect::new(bx, 62.0, bw, 30.0), b.label(), q.bucket == *b, Action::Bucket(*b));
        }
        if buckets.len() > 1 {
            self.label(c, bx - 8.0, 77.0, "by", theme::faint(), Align::Right);
        }

        // Time played, pace and averages; what the timeline shows.
        if let Some(s) = &ui.stats {
            if s.total > 0 {
                let mut x = 16.0;
                let limit = w - 16.0 - 110.0;
                for (i, (text, tip)) in summary(s).into_iter().enumerate() {
                    let sep = if i > 0 { "  ·  " } else { "" };
                    let sep_w = self.measure(c, sep, 12.5, 300.0);
                    let tw = self.measure(c, &text, 12.5, 300.0);
                    if x + sep_w + tw > limit {
                        break;
                    }
                    self.text(c, x, 108.0, sep, 12.5, theme::faint(), Align::Left);
                    x += sep_w;
                    self.text(c, x, 108.0, &text, 12.5, theme::dim(), Align::Left);
                    if let Some((title, value)) = tip {
                        ui.tips.push((Rect::new(x, 98.0, tw, 20.0), format!("{title}\n{value}")));
                    }
                    x += tw;
                }
            }
        }
        let rt = Rect::new(w - 16.0 - 46.0, 96.0, 46.0, 24.0);
        let rn = Rect::new(rt.x - 52.0, 96.0, 50.0, 24.0);
        let mode = ui.chart_mode;
        self.small_toggle(c, ui, rn, "Notes", mode == ChartMode::Notes, Action::ChartMode(ChartMode::Notes));
        self.small_toggle(c, ui, rt, "Time", mode == ChartMode::Time, Action::ChartMode(ChartMode::Time));

        // Timeline.
        let chart = Rect::new(60.0, 130.0, w - 76.0, 126.0);
        let loading_alpha = if ui.loading { 0.5 } else { 1.0 };
        c.save();
        c.set_global_alpha(loading_alpha);

        if let Some(stats) = ui.stats.take() {
            let time = ui.chart_mode == ChartMode::Time;
            // In time mode the bars are seconds of (estimated) playing.
            let values: Vec<u64> = if time {
                stats.played.iter().map(|&m| (m * 60.0).round() as u64).collect()
            } else {
                stats.counts.clone()
            };
            let max = values.iter().copied().max().unwrap_or(0);
            let axis = |v: u64| if time { theme::short_duration(v as f64 / 60.0) } else { theme::short(v) };
            Self::hline(c, chart.x, chart.x + chart.w, chart.y + chart.h, theme::hairline());
            Self::hline(c, chart.x, chart.x + chart.w, chart.y, theme::hairline().with_alpha(10));
            Self::hline(c, chart.x, chart.x + chart.w, chart.y + chart.h / 2.0, theme::hairline().with_alpha(10));
            if max > 0 {
                self.text(c, chart.x - 8.0, chart.y, &axis(max), 11.0, theme::faint(), Align::Right);
                self.text(c, chart.x - 8.0, chart.y + chart.h / 2.0, &axis(max / 2), 11.0, theme::faint(), Align::Right);
            }
            self.text(c, chart.x - 8.0, chart.y + chart.h, "0", 11.0, theme::faint(), Align::Right);
            let n = stats.counts.len();
            let stats_ref = &stats;
            self.bars_with(c, ui, chart, &values, &|i| {
                let s = stats_ref;
                let played = played_text(s.played[i], s.logged[i], s.pace.is_some(), s.query.bucket == Bucket::Minute);
                let value = match (time, s.counts[i]) {
                    (_, 0) => "Not played".to_string(),
                    (true, _) => played,
                    (false, n) if s.query.bucket == Bucket::Minute => format!("{} notes", commas(n)),
                    (false, n) => format!("{} notes · {} · up to {} a minute", commas(n), played, commas(s.peak[i])),
                };
                (s.bucket_label(i), value)
            });
            // Axis labels: about six, evenly spread.
            let step = (n as f32 / 6.0).ceil().max(1.0) as usize;
            let slot = chart.w / n.max(1) as f32;
            for i in (0..n).step_by(step) {
                self.text(c, chart.x + i as f32 * slot + slot / 2.0, chart.y + chart.h + 14.0, &stats.axis_label(i), 11.0, theme::faint(), Align::Center);
            }
            if stats.total == 0 && !ui.loading {
                self.text(c, chart.x + chart.w / 2.0, chart.y + chart.h / 2.0, "No notes in this range", 14.0, theme::dim(), Align::Center);
            }

            self.draw_notes(c, ui, &stats, Rect::new(16.0, 296.0, 470.0, 140.0));
            self.draw_velocity(c, ui, &stats, Rect::new(500.0, 296.0, w - 516.0, 140.0));
            ui.stats = Some(stats);
        }
        c.restore();
    }

    fn draw_notes(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats, panel: Rect) {
        Self::rounded(c, panel, 6.0, Some(theme::panel_fill()), Some(Paint::color(theme::hairline())));
        self.label(c, panel.x + 14.0, panel.y + 18.0, "Notes", theme::dim(), Align::Left);
        let pc = stats::pitch_classes(&stats.notes);
        let scale = if ui.note_mode == NoteMode::Drums { None } else { stats::likely_scale(&pc) };
        if let Some(scale) = scale {
            let paint = theme::gradient(panel.x + 70.0, 0.0, panel.x + 300.0, 0.0, 255)
                .with_font(&[self.font])
                .with_font_size(13.0)
                .with_font_weight(300.0)
                .with_text_baseline(Baseline::Middle);
            let _ = c.fill_text(panel.x + 70.0, panel.y + 18.0, format!("mostly {scale}"), &paint);
        }
        let mode = ui.note_mode;
        let r3 = Rect::new(panel.x + panel.w - 60.0, panel.y + 6.0, 50.0, 24.0);
        let r2 = Rect::new(r3.x - 66.0, panel.y + 6.0, 64.0, 24.0);
        let r1 = Rect::new(r2.x - 86.0, panel.y + 6.0, 84.0, 24.0);
        self.small_toggle(c, ui, r1, "Pitch class", mode == NoteMode::PitchClass, Action::NoteMode(NoteMode::PitchClass));
        self.small_toggle(c, ui, r2, "All notes", mode == NoteMode::All, Action::NoteMode(NoteMode::All));
        self.small_toggle(c, ui, r3, "Drums", mode == NoteMode::Drums, Action::NoteMode(NoteMode::Drums));

        let area = Rect::new(panel.x + 14.0, panel.y + 40.0, panel.w - 28.0, panel.h - 66.0);
        if mode == NoteMode::Drums {
            self.draw_drum_bars(c, ui, &stats.notes, area);
        } else if mode == NoteMode::All {
            let lo = stats.notes.iter().position(|&n| n > 0).unwrap_or(36) as u8;
            let hi = stats.notes.iter().rposition(|&n| n > 0).unwrap_or(72) as u8;
            let (mut lo, mut hi) = (lo, hi.max(lo));
            while hi - lo < 23 {
                lo = lo.saturating_sub(1);
                hi = (hi + 1).min(127);
                if lo == 0 && hi == 127 {
                    break;
                }
            }
            let values: Vec<u64> = (lo..=hi).map(|n| stats.notes[n as usize]).collect();
            let kit = ui.kit.clone();
            self.bars(c, ui, area, &values, &|i| {
                let note = lo + i as u8;
                match kit.name(note).or_else(|| stats::gm_drum_name(note)) {
                    Some(d) => format!("{} · {} · {}", note, stats::note_name(note), d),
                    None => format!("{} · {}", note, stats::note_name(note)),
                }
            });
            let slot = area.w / values.len() as f32;
            for (i, n) in (lo..=hi).enumerate() {
                if n % 12 == 0 {
                    self.text(c, area.x + i as f32 * slot + slot / 2.0, area.y + area.h + 13.0, &stats::note_name(n), 10.5, theme::faint(), Align::Center);
                }
            }
        } else {
            self.bars(c, ui, area, &pc, &|i| stats::NOTE_NAMES[i].to_string());
            let slot = area.w / 12.0;
            for (i, name) in stats::NOTE_NAMES.iter().enumerate() {
                self.text(c, area.x + i as f32 * slot + slot / 2.0, area.y + area.h + 13.0, name, 11.0, theme::faint(), Align::Center);
            }
        }
    }

    fn small_toggle(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, r: Rect, s: &str, selected: bool, action: Action) {
        let hovered = r.contains(ui.mouse) && matches!(ui.overlay, Overlay::None);
        if hovered {
            Self::rounded(c, r, 4.0, Some(theme::hover_fill()), None);
        }
        let color = if selected { theme::text() } else if hovered { theme::text().with_alpha(170) } else { theme::faint() };
        self.text(c, r.x + r.w / 2.0, r.y + r.h / 2.0, s, 12.0, color, Align::Center);
        if selected {
            let mut p = Path::new();
            p.rect(r.x + r.w * 0.25, r.y + r.h - 2.0, r.w * 0.5, 1.0);
            c.fill_path(&p, &theme::window_gradient(255));
        }
        ui.regions.push((r, action));
    }

    fn draw_velocity(&self, c: &mut Canvas<OpenGl>, ui: &mut Ui, stats: &Stats, panel: Rect) {
        Self::rounded(c, panel, 6.0, Some(theme::panel_fill()), Some(Paint::color(theme::hairline())));
        self.label(c, panel.x + 14.0, panel.y + 18.0, "Velocity", theme::dim(), Align::Left);
        let count: u64 = stats.vels.iter().sum();
        if count > 0 {
            let avg = stats.vels.iter().enumerate().map(|(v, &n)| v as u64 * n).sum::<u64>() as f64 / count as f64;
            self.text(c, panel.x + panel.w - 14.0, panel.y + 18.0, &format!("average {:.0}", avg), 12.5, theme::dim(), Align::Right);
        }
        let area = Rect::new(panel.x + 14.0, panel.y + 40.0, panel.w - 28.0, panel.h - 66.0);
        let values: Vec<u64> = stats.vels[1..].to_vec();
        self.bars(c, ui, area, &values, &|i| format!("Velocity {}", i + 1));
        for (v, align) in [(1usize, Align::Left), (64, Align::Center), (127, Align::Right)] {
            let x = area.x + (v - 1) as f32 / 126.0 * area.w;
            self.text(c, x, area.y + area.h + 13.0, &v.to_string(), 10.5, theme::faint(), align);
        }
    }

    fn render(&self) -> Result<(), HandlerError> {
        unsafe { self.gl.make_current() }.map_err(|e| handler_error(e.to_string()))?;
        let size = self.ctx.size();
        let (pw, ph) = (size.physical.width, size.physical.height);
        let (scale, ox, oy) = self.layout();
        let mut canvas = self.canvas.borrow_mut();
        let c = &mut *canvas;
        c.set_size(pw, ph, 1.0);
        c.clear_rect(0, 0, pw, ph, theme::background());
        c.reset_transform();
        c.translate(ox, oy);
        c.scale(scale, scale);

        let mut ui = self.ui.borrow_mut();
        let ui = &mut *ui;
        if let Some(s) = ui.incoming.lock().unwrap().take() {
            ui.stats = Some(s);
            ui.loading = false;
        }
        ui.snapshot = self.shared.snapshot();
        ui.kit = self.shared.kit();
        ui.regions.clear();
        ui.tips.clear();
        match ui.view {
            View::Counter => self.draw_counter(c, ui),
            View::Stats => self.draw_stats(c, ui),
            View::Kit => self.draw_kit(c, ui),
        }
        let mut overlay = std::mem::replace(&mut ui.overlay, Overlay::None);
        // Draw overlays with the hover state they'd have (overlay set back first).
        match &mut overlay {
            Overlay::None => self.draw_tooltip(c, ui),
            Overlay::Profiles { scroll } => {
                let mut s = *scroll;
                ui.overlay = Overlay::Profiles { scroll: s };
                self.draw_profiles(c, ui, &mut s);
                *scroll = s;
            }
            Overlay::NewProfile { text, error } => {
                ui.overlay = Overlay::NewProfile { text: String::new(), error: None };
                self.draw_new_profile(c, ui, text, error.as_deref());
            }
            Overlay::Learn { .. } => {
                ui.overlay = Overlay::Learn { since: 0 };
                self.draw_learn(c, ui);
            }
            Overlay::NameDrum { note, text, error, .. } => {
                ui.overlay = Overlay::Learn { since: 0 };
                self.draw_name_drum(c, ui, *note, text, error.as_deref());
            }
        }
        ui.overlay = overlay;
        ui.drawn_hover = Self::hover_state(ui);
        self.update_cursor(ui);
        c.flush();
        drop(canvas);

        if let (Some(path), false) = (&self.options.screenshot, ui.screenshot_done) {
            let ready = ui.view != View::Stats || (ui.stats.is_some() && !ui.loading);
            if ready {
                ui.screenshot_done = true;
                self.save_screenshot(path);
            }
        }
        self.gl.swap_buffers().map_err(|e| handler_error(e.to_string()))?;
        Ok(())
    }

    fn save_screenshot(&self, path: &std::path::Path) {
        let Ok(img) = self.canvas.borrow_mut().screenshot() else { return };
        let (w, h) = (img.width() as u32, img.height() as u32);
        let mut bytes = Vec::with_capacity((w * h * 4) as usize);
        for px in img.pixels() {
            bytes.extend_from_slice(&[px.r, px.g, px.b, px.a]);
        }
        write_png(path, w, h, &bytes);
        eprintln!("screenshot saved to {}", path.display());
        std::process::exit(0);
    }

    fn on_key(&self, ui: &mut Ui, ev: &keyboard_types::KeyboardEvent) -> bool {
        if ev.state != KeyState::Down {
            return matches!(ui.overlay, Overlay::NewProfile { .. } | Overlay::NameDrum { .. });
        }
        match &mut ui.overlay {
            Overlay::NewProfile { text, error } => {
                match &ev.key {
                    Key::Named(NamedKey::Enter) => {
                        self.create_profile(ui);
                        return true;
                    }
                    Key::Named(NamedKey::Escape) => ui.overlay = Overlay::None,
                    Key::Named(NamedKey::Backspace) => {
                        text.pop();
                        *error = None;
                    }
                    Key::Character(s) if !ev.modifiers.ctrl() && !ev.modifiers.meta() => {
                        if text.chars().count() < crate::store::MAX_PROFILE_NAME {
                            text.extend(s.chars().filter(|c| !c.is_control()));
                        }
                        *error = None;
                    }
                    _ => {}
                }
                true
            }
            Overlay::NameDrum { text, edited, error, .. } => {
                match &ev.key {
                    Key::Named(NamedKey::Enter) => {
                        self.save_drum(ui);
                        return true;
                    }
                    Key::Named(NamedKey::Escape) => ui.overlay = Overlay::None,
                    Key::Named(NamedKey::Backspace) => {
                        text.pop();
                        *edited = true;
                        *error = None;
                    }
                    Key::Character(s) if !ev.modifiers.ctrl() && !ev.modifiers.meta() => {
                        if text.chars().count() < crate::kit::MAX_DRUM_NAME {
                            text.extend(s.chars().filter(|c| !c.is_control()));
                        }
                        *edited = true;
                        *error = None;
                    }
                    _ => {}
                }
                true
            }
            Overlay::Profiles { .. } | Overlay::Learn { .. } => {
                if ev.key == Key::Named(NamedKey::Escape) {
                    ui.overlay = Overlay::None;
                    return true;
                }
                false
            }
            Overlay::None => false,
        }
    }
}

impl WindowHandler for Editor {
    fn draw(&self) -> Result<(), HandlerError> {
        self.render()
    }

    fn resized(&self, _new_size: WindowSize) -> Result<(), HandlerError> {
        self.ctx.request_redraw();
        Ok(())
    }

    fn on_timer(&self, timer: &TimerHandle) {
        if self.timer.borrow().as_ref() != Some(timer) {
            return;
        }
        let mut ui = self.ui.borrow_mut();
        let hits = self.shared.hits_seen();
        let generation = self.shared.generation.load(std::sync::atomic::Ordering::Relaxed);
        let stats_ready = ui.incoming.lock().map(|s| s.is_some()).unwrap_or(false);
        let recent = self.shared.recent_seq();
        let recent_changed = recent != ui.seen_recent;
        if recent_changed {
            ui.seen_recent = recent;
            self.on_recent_hit(&mut ui);
        }
        // Ignored hits matter only where the monitor or a learn dialog shows them.
        let recent_shown = recent_changed && (ui.view == View::Kit || !matches!(ui.overlay, Overlay::None));
        if hits != ui.seen_hits || generation != ui.seen_generation || stats_ready || recent_shown {
            ui.seen_hits = hits;
            ui.seen_generation = generation;
            self.ctx.request_redraw();
        }
    }

    fn on_event(&self, event: Event) -> EventStatus {
        let (scale, ox, oy) = self.layout();
        let mut ui = self.ui.borrow_mut();
        let ui = &mut *ui;
        match event {
            Event::Mouse(MouseEvent::CursorMoved { position, .. }) => {
                ui.mouse = ((position.x as f32 - ox) / scale, (position.y as f32 - oy) / scale);
                self.update_cursor(ui);
                if Self::hover_state(ui) != ui.drawn_hover {
                    self.ctx.request_redraw();
                }
                EventStatus::Captured
            }
            Event::Mouse(MouseEvent::CursorLeft) => {
                ui.mouse = (-1.0, -1.0);
                self.ctx.request_redraw();
                EventStatus::Captured
            }
            Event::Mouse(MouseEvent::ButtonPressed { button: MouseButton::Left, .. }) => {
                if let Some((_, action)) = ui.regions.iter().rev().find(|(r, _)| r.contains(ui.mouse)).cloned() {
                    self.act(ui, action);
                    self.ctx.request_redraw();
                }
                EventStatus::Captured
            }
            Event::Mouse(MouseEvent::WheelScrolled { delta, .. }) => {
                if matches!(ui.overlay, Overlay::None) && ui.view == View::Kit {
                    let dy = match delta {
                        ScrollDelta::Lines { y, .. } => y,
                        ScrollDelta::Pixels { y, .. } => y / 30.0,
                    };
                    if dy < 0.0 {
                        ui.kit_scroll += 1;
                    } else if dy > 0.0 {
                        ui.kit_scroll = ui.kit_scroll.saturating_sub(1);
                    }
                    self.ctx.request_redraw();
                }
                if let Overlay::Profiles { scroll } = &mut ui.overlay {
                    let dy = match delta {
                        ScrollDelta::Lines { y, .. } => y,
                        ScrollDelta::Pixels { y, .. } => y / 30.0,
                    };
                    if dy < 0.0 {
                        *scroll += 1;
                    } else if dy > 0.0 {
                        *scroll = scroll.saturating_sub(1);
                    }
                    self.ctx.request_redraw();
                }
                EventStatus::Captured
            }
            Event::Keyboard(ev) => {
                if self.on_key(ui, &ev) {
                    self.ctx.request_redraw();
                    EventStatus::Captured
                } else {
                    EventStatus::Ignored
                }
            }
            _ => EventStatus::Ignored,
        }
    }
}

/// Fits the 800 × 450 layout into a `w` × `h` window: (scale, x offset, y offset).
/// The time played in a bucket, for its tooltip: "about 35 min · 41 logged", or in seconds for a
/// minute. Without a pace the logged minutes are all there is.
fn played_text(played: f64, logged: u32, estimated: bool, minute: bool) -> String {
    if minute {
        return if estimated { format!("about {:.0} s", played * 60.0) } else { "played".to_string() };
    }
    if !estimated {
        return format!("{} played", theme::duration(logged as f64));
    }
    format!("about {} · {} logged", theme::duration(played), theme::duration(logged as f64))
}

/// The line under the range, in parts: time played, notes a minute (average and most), and averages
/// per bucket. Each part has an optional tooltip (title, value).
fn summary(s: &Stats) -> Vec<(String, Option<(String, String)>)> {
    let mut parts = Vec::new();
    if s.pace.is_some() {
        parts.push((
            format!("About {} played", theme::duration(s.played_total)),
            Some((
                format!("{} logged; part-played minutes count in part", theme::duration(s.logged_total as f64)),
                format!("a full minute is {}+ notes", commas(s.pace.unwrap_or(0.0).round() as u64)),
            )),
        ));
    } else {
        parts.push((format!("{} played", theme::duration(s.played_total)), None));
    }
    if let Some(rate) = s.per_minute() {
        let most = s.busiest.map(|(_, n)| n).unwrap_or(0);
        let tip = s.busiest_label().map(|when| (format!("Busiest minute: {when}"), format!("{} notes", commas(most))));
        parts.push((format!("{} notes a minute, {} at most", commas(rate.round() as u64), commas(most)), tip));
    }
    let per = match s.query.bucket {
        Bucket::Minute => None,
        Bucket::Hour => Some("an hour"),
        Bucket::Day => Some("a day"),
        Bucket::Week => Some("a week"),
        Bucket::Month => Some("a month"),
        Bucket::Year => Some("a year"),
    };
    if let Some(per) = per.filter(|_| s.elapsed > 1) {
        let n = s.elapsed as f64;
        parts.push((
            format!("{} notes and {} {per}", commas((s.total as f64 / n).round() as u64), theme::duration(s.played_total / n)),
            Some((format!("On average, over the {} so far", plural(s)), "including the ones you didn't play".to_string())),
        ));
    }
    parts
}

/// "5 days", "1 week": how many buckets have started.
fn plural(s: &Stats) -> String {
    let unit = s.query.bucket.label().to_lowercase();
    if s.elapsed == 1 { format!("1 {unit}") } else { format!("{} {unit}s", s.elapsed) }
}

fn fit(w: f32, h: f32) -> (f32, f32, f32) {
    let scale = (w / WIDTH as f32).min(h / HEIGHT as f32).max(0.01);
    let ox = ((w - WIDTH as f32 * scale) / 2.0).max(0.0).floor();
    let oy = ((h - HEIGHT as f32 * scale) / 2.0).max(0.0).floor();
    (scale, ox, oy)
}

fn write_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) {
    // Minimal PNG writer (stored zlib blocks), so the plugin needs no image crate.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
            }
        }
        !crc
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend_from_slice(data);
        out.extend_from_slice(&c);
        out.extend_from_slice(&crc32(&c).to_be_bytes());
    }
    let mut raw = Vec::with_capacity(((w * 4 + 1) * h) as usize);
    for row in rgba.chunks((w * 4) as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push((i == blocks.len() - 1) as u8);
        z.extend_from_slice(&(b.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
        z.extend_from_slice(b);
    }
    let (mut a, mut bsum) = (1u32, 0u32);
    for &x in &raw {
        a = (a + x as u32) % 65521;
        bsum = (bsum + a) % 65521;
    }
    z.extend_from_slice(&((bsum << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    let _ = std::fs::write(path, out);
}

#[cfg(test)]
mod tests {
    #[test]
    fn fit_keeps_the_shape() {
        assert_eq!(super::fit(800.0, 450.0), (1.0, 0.0, 0.0));
        assert_eq!(super::fit(1600.0, 900.0), (2.0, 0.0, 0.0));
        assert_eq!(super::fit(1000.0, 450.0), (1.0, 100.0, 0.0));
        assert_eq!(super::fit(800.0, 650.0), (1.0, 0.0, 100.0));
    }
}
