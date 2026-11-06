// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! Opens the plugin window on its own, with made-up data, for working on the UI.
//!
//! ```text
//! cargo run --example preview                       # counter view, fake playing
//! cargo run --example preview -- --stats            # stats view
//! cargo run --example preview -- --stats --page rhythm   # a stats page: overview, rhythm, kit,
//!                                                        # habits or review
//! cargo run --example preview -- --stats --chart tempo  # the timeline: notes, time, velocity,
//!                                                        # tempo or timing
//! cargo run --example preview -- --profile Drums    # start on the drum kit profile
//! cargo run --example preview -- --kit              # drums view
//! cargo run --example preview -- --screenshot a.png # save the first frame and exit
//! ```
//! Data goes to a temporary folder unless MODOMETER_DATA_DIR is set.

use std::collections::BTreeMap;
use std::sync::Arc;

use modometer::session::Shared;
use modometer::store::{self, MinuteCounts, Store};
use modometer::ui::{self, Editor, Options};

struct Rand(u64);

impl Rand {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// About 120 days of practice, most days, mostly evenings: `minute` fills in one minute.
fn fake_days(store: &Store, profile: &str, seed: u64, mut minute: impl FnMut(&mut Rand, f64, &mut MinuteCounts)) {
    if !store.months(profile).is_empty() {
        return;
    }
    let now = store::now_minute();
    let mut rand = Rand(seed);
    let mut minutes = BTreeMap::new();
    for day in 0..120i64 {
        if rand.below(7) == 0 {
            continue;
        }
        let session_start = now - day * 1440 - rand.below(300) as i64 - 60;
        let len = 20 + rand.below(70) as i64;
        // Practice pays off: older days are a little looser.
        let progress = 1.0 - day as f64 / 120.0;
        for m in 0..len {
            let mut c = MinuteCounts::default();
            minute(&mut rand, progress, &mut c);
            minutes.insert(session_start - m, c);
        }
    }
    store.append(profile, &minutes).unwrap();
}

/// An E minor pentatonic keys player, with chords.
fn fake_keys(store: &Store) {
    let scale = [40u8, 43, 45, 47, 50, 52, 55, 57, 59, 62, 64, 67, 69, 71, 74, 76];
    // Em, G, Am, C, D (bit 0 = C).
    let chords: [(u16, u32); 5] = [(0b1000_1001_0000, 6), (0b1000_1000_0100, 4), (0b0010_0001_0001, 3), (0b0000_1001_0001, 2), (0b0010_0100_0100, 1)];
    fake_days(store, "Default", 0x2545F491, |r, progress, c| {
        let hits = 40 + r.below(160);
        for _ in 0..hits {
            let note = scale[r.below(scale.len() as u64) as usize];
            let vel = (70 + r.below(40) as i32 - r.below(30) as i32).clamp(1, 127) as u8;
            c.add_hit(note, vel);
        }
        let d = &mut c.detail;
        let bpm = [92u16, 96, 100, 100, 104, 120][r.below(6) as usize];
        if r.below(3) > 0 {
            *d.tempo.entry(bpm + r.below(3) as u16).or_default() += hits as u32 * 2 / 3;
            let ms = 9.0 + 10.0 * (1.0 - progress) + r.below(40) as f64 / 10.0;
            d.timing = (hits as u32 / 2, (hits as f64 / 2.0 * ms * 10.0) as u64);
        }
        for (mask, w) in chords {
            if r.below(10) < w as u64 {
                *d.chords.entry(mask).or_default() += 1 + r.below(3) as u32;
            }
        }
        for (iv, w) in [(1usize, 3u64), (2, 9), (3, 7), (4, 3), (5, 6), (7, 4), (12, 1), (0, 2)] {
            d.intervals[iv] += r.below(w + 1) as u32;
        }
        d.speed = Some((64, 40 + r.below(70) as u16));
    });
}

/// A rock drummer with a named kit.
fn fake_drums(store: &Store) {
    let kit = [(36u8, 30u64), (38, 24), (42, 40), (44, 4), (48, 3), (45, 3), (43, 4), (49, 4), (51, 8), (57, 1)];
    fake_days(store, "Drums", 0x9E3779B9, |r, progress, c| {
        let hits = 120 + r.below(200);
        let d_total: u64 = kit.iter().map(|k| k.1).sum();
        for _ in 0..hits {
            let mut x = r.below(d_total);
            let mut note = 36;
            for &(n, w) in &kit {
                if x < w {
                    note = n;
                    break;
                }
                x -= w;
            }
            let vel = match note {
                38 if r.below(5) == 0 => 20 + r.below(25) as u8, // ghost notes
                38 | 49 | 57 => 95 + r.below(32) as u8,
                _ => (60 + r.below(50)) as u8,
            };
            c.add_hit(note, vel);
            let lead = if note == 44 { 42 } else { note };
            c.detail.drum_vels.entry(lead).or_default()[(vel as usize / 16).min(7)] += 1;
            if note == 42 {
                c.detail.pedal[[0usize, 6, 7, 7, 7, 3][r.below(6) as usize]] += 1;
            }
        }
        let d = &mut c.detail;
        let bpm = [96u16, 110, 120, 120, 128, 140][r.below(6) as usize];
        *d.tempo.entry(bpm + r.below(2) as u16).or_default() += hits as u32 * 4 / 5;
        *d.host_tempo.entry(120).or_default() += if r.below(4) == 0 { hits as u32 } else { 0 };
        let ms = 5.0 + 8.0 * (1.0 - progress) + r.below(30) as f64 / 10.0;
        d.timing = (hits as u32 * 3 / 4, (hits as f64 * 0.75 * ms * 10.0) as u64);
        for (note, off) in [(36u8, -1.5), (38, 3.5), (42, -2.0), (51, 0.5)] {
            let n = 20 + r.below(20) as u32;
            d.offsets.insert(note, (n, (n as f64 * off * 10.0) as i64));
        }
        d.swing = (10 + r.below(10) as u32, 0);
        d.swing.1 = d.swing.0 as u64 * (540 + r.below(40));
        if r.below(3) == 0 {
            d.drift = (1, r.below(160) as i64 - 50);
        }
        d.speed = Some((38, 90 + r.below(60) as u16));
        d.rolls = r.below(3) as u32;
        d.fills = r.below(4) as u32;
        d.tempo.retain(|_, n| *n > 0);
        d.host_tempo.retain(|_, n| *n > 0);
    });
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    let screenshot = arg("--screenshot").map(Into::into);
    let start_in_stats = args.iter().any(|a| a == "--stats") || arg("--page").is_some() || arg("--chart").is_some();
    let start_in_kit = args.iter().any(|a| a == "--kit");
    let root = std::env::var_os("MODOMETER_DATA_DIR")
        .map(Into::into)
        .unwrap_or_else(|| std::env::temp_dir().join("modometer-preview"));
    let store = Store::new(root);
    store.create_profile("Guitar").unwrap();
    store.create_profile("Drums").unwrap();
    store.create_profile("Default").unwrap();
    fake_keys(&store);
    fake_drums(&store);
    // "Milestone": 990 notes yesterday and 20 today, so today passed 1,000.
    if store.create_profile("Milestone").is_ok() && store.months("Milestone").is_empty() {
        let mut minutes = BTreeMap::new();
        for (minute, n) in [(store::now_minute() - 1440, 990), (store::now_minute() - 1, 20)] {
            let mut c = MinuteCounts::default();
            (0..n).for_each(|_| c.add_hit(38, 90));
            minutes.insert(minute, c);
        }
        store.append("Milestone", &minutes).unwrap();
    }
    let shared = Shared::new(store);
    shared.select_profile("Drums");
    if shared.kit().names.is_empty() {
        for (note, name) in [(36, "Kick"), (38, "Snare"), (40, "Snare"), (42, "Hi-hat"), (44, "Hi-hat pedal"), (48, "Tom 1"), (45, "Tom 2"), (43, "Floor tom"), (49, "Crash 1"), (51, "Ride"), (57, "Crash 2")] {
            shared.set_drum_name(note, Some(name)).unwrap();
        }
        shared.set_pedal_cc(4);
    }
    shared.select_profile(&arg("--profile").unwrap_or_else(|| if start_in_kit { "Drums".into() } else { "Default".into() }));
    shared.start();

    // Pretend someone's playing: a little groove, with the odd double trigger.
    let player = Arc::clone(&shared);
    std::thread::spawn(move || {
        let mut guard = modometer::session::Guard::default();
        let start = std::time::Instant::now();
        let pattern = [36u8, 42, 38, 42, 36, 36, 38, 49];
        for i in 0.. {
            // A quick burst first, so a screenshot of the drums view has hits in it.
            std::thread::sleep(std::time::Duration::from_millis(if i < 8 { 5 } else { 180 }));
            let note = pattern[i % pattern.len()];
            let at = start.elapsed().as_micros() as u64 * 48 / 1000;
            let minute = store::now_minute() as u32;
            player.note_on(&mut guard, at, 48000.0, minute, note, 70 + (i % 5) as u8 * 10);
            if i % 3 == 0 {
                player.note_on(&mut guard, at + 48 * 9, 48000.0, minute, note, 20);
            }
        }
    });

    let options = Options { screenshot, start_in_stats, start_in_kit, page: arg("--page"), chart: arg("--chart") };
    let window = ui::window_settings(1.0).into_iter().find_map(|settings| {
        let (s, options) = (Arc::clone(&shared), options.clone());
        baseview::Window::create(settings, move |ctx| Editor::new(ctx, s, options)).ok()
    });
    window.expect("couldn't open a window").run_until_closed().unwrap();
    shared.shutdown();
}
