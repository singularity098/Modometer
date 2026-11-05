// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! Opens the plugin window on its own, with made-up data, for working on the UI.
//!
//! ```text
//! cargo run --example preview                       # counter view, fake playing
//! cargo run --example preview -- --stats            # stats view
//! cargo run --example preview -- --screenshot a.png # save the first frame and exit
//! ```
//! Data goes to a temporary folder unless MIDI_COUNTER_DATA_DIR is set.

use std::collections::BTreeMap;
use std::sync::Arc;

use midi_counter::session::Shared;
use midi_counter::store::{self, MinuteCounts, Store};
use midi_counter::ui::{self, Editor, Options};

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
    let root = std::env::var_os("MIDI_COUNTER_DATA_DIR")
        .map(Into::into)
        .unwrap_or_else(|| std::env::temp_dir().join("midi-counter-preview"));
    let store = Store::new(root);
    store.create_profile("Guitar").unwrap();
    store.create_profile("Drums").unwrap();
    store.create_profile("Default").unwrap();
    fake_history(&store, "Default");
    let shared = Shared::new(store);
    shared.start();

    // Pretend someone's playing.
    let player = Arc::clone(&shared);
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(180));
        player.record_hit(store::now_minute() as u32, 52, 96);
    });

    let options = Options { screenshot, start_in_stats };
    let window = ui::window_settings(1.0).into_iter().find_map(|settings| {
        let (s, options) = (Arc::clone(&shared), options.clone());
        baseview::Window::create(settings, move |ctx| Editor::new(ctx, s, options)).ok()
    });
    window.expect("couldn't open a window").run_until_closed().unwrap();
    shared.shutdown();
}
