// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! Opens the plugin window on its own, with made-up data, for working on the UI.
//!
//! ```text
//! cargo run --example preview                       # counter view, fake playing
//! cargo run --example preview -- --stats            # stats view
//! cargo run --example preview -- --kit              # drums view
//! cargo run --example preview -- --screenshot a.png # save the first frame and exit
//! ```
//! Data goes to a temporary folder unless MODOMETER_DATA_DIR is set.

use std::collections::BTreeMap;
use std::sync::Arc;

use modometer::session::Shared;
use modometer::store::{self, MinuteCounts, Store};
use modometer::ui::{self, Editor, Options};

fn fake_history(store: &Store, profile: &str) {
    if !store.months(profile).is_empty() {
        return;
    }
    // About 120 days of practice: an E minor pentatonic player, most days, mostly evenings.
    let scale = [40u8, 43, 45, 47, 50, 52, 55, 57, 59, 62, 64, 67];
    let now = store::now_minute();
    let mut seed = 0x2545F491u64;
    let mut rand = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut minutes = BTreeMap::new();
    for day in 0..120i64 {
        if rand() % 7 == 0 {
            continue;
        }
        let session_start = now - day * 1440 - (rand() % 300) as i64 - 60;
        let len = 20 + (rand() % 70) as i64;
        for m in 0..len {
            let mut c = MinuteCounts::default();
            let hits = 40 + rand() % 160;
            for _ in 0..hits {
                let note = scale[(rand() % scale.len() as u64) as usize];
                let vel = (70 + (rand() % 40) as i32 - (rand() % 30) as i32).clamp(1, 127) as u8;
                c.add_hit(note, vel);
            }
            minutes.insert(session_start - m, c);
        }
    }
    store.append(profile, &minutes).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let screenshot = args.iter().position(|a| a == "--screenshot").and_then(|i| args.get(i + 1)).map(Into::into);
    let start_in_stats = args.iter().any(|a| a == "--stats");
    let start_in_kit = args.iter().any(|a| a == "--kit");
    let root = std::env::var_os("MODOMETER_DATA_DIR")
        .map(Into::into)
        .unwrap_or_else(|| std::env::temp_dir().join("modometer-preview"));
    let store = Store::new(root);
    store.create_profile("Guitar").unwrap();
    store.create_profile("Drums").unwrap();
    store.create_profile("Default").unwrap();
    fake_history(&store, "Default");
    let shared = Shared::new(store);
    shared.start();

    if start_in_kit && shared.kit().names.is_empty() {
        for (note, name) in [(36, "Kick"), (38, "Snare"), (40, "Snare"), (42, "Hi-hat"), (49, "Crash 1"), (57, "Crash 2")] {
            shared.set_drum_name(note, Some(name)).unwrap();
        }
    }

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

    let options = Options { screenshot, start_in_stats, start_in_kit };
    let window = ui::window_settings(1.0).into_iter().find_map(|settings| {
        let (s, options) = (Arc::clone(&shared), options.clone());
        baseview::Window::create(settings, move |ctx| Editor::new(ctx, s, options)).ok()
    });
    window.expect("couldn't open a window").run_until_closed().unwrap();
    shared.shutdown();
}
