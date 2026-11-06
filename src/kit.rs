// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! Per-profile settings: the drum map (MIDI note -> drum name) and the retrigger guard.
//!
//! Stored as `settings.txt` in the profile's folder:
//!
//! ```text
//! retrigger-ms 30
//! note 36 Kick
//! note 38 Snare
//! note 40 Snare
//! ```
//!
//! Notes that share a name are one drum: their hits are guarded together and add up in the stats.
//!
//! `hihat-pedal-cc 4` (off unless present) reads the hi-hat pedal's position from that MIDI CC, to
//! show how open the hi-hat was when you hit it.
//!
//! A profile with at least one named drum is a drum kit: its hits get the drum analysis (rolls,
//! fills, per-drum dynamics). Others get the keys analysis (chords, intervals).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

pub const SETTINGS_FILE: &str = "settings.txt";
pub const DEFAULT_RETRIGGER_MS: u32 = 30;
/// The guard settings offered in the UI, in milliseconds (0 = off).
pub const RETRIGGER_CHOICES: [u32; 9] = [0, 10, 20, 30, 40, 50, 75, 100, 150];
pub const MAX_DRUM_NAME: usize = 32;

/// Names offered when learning a drum.
pub const PRESETS: [&str; 16] = [
    "Kick", "Snare", "Snare rim", "Cross stick", "Hi-hat", "Hi-hat edge", "Hi-hat pedal", "Tom 1", "Tom 2",
    "Tom 3", "Floor tom", "Crash 1", "Crash 2", "Ride", "Ride bell", "Splash",
];

/// The usual hi-hat pedal controller (MIDI CC 4, "foot controller").
pub const PEDAL_CC: u8 = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Kit {
    /// Hits of the same drum closer together than this count once (0 = off).
    pub retrigger_ms: u32,
    pub names: BTreeMap<u8, String>,
    /// The MIDI CC of the hi-hat pedal, or 0 to not read it.
    pub pedal_cc: u8,
}

impl Default for Kit {
    fn default() -> Self {
        Kit { retrigger_ms: DEFAULT_RETRIGGER_MS, names: BTreeMap::new(), pedal_cc: 0 }
    }
}

/// What part of the kit a drum is, from its name (or its General MIDI note if it has none).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Kick,
    Snare,
    Tom,
    HiHat,
    HatPedal,
    Cymbal,
    Other,
}

impl Role {
    pub fn of_name(name: &str) -> Role {
        let n = name.to_lowercase();
        let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
        if has(&["hat", "hh"]) {
            if has(&["pedal", "foot", "chick"]) { Role::HatPedal } else { Role::HiHat }
        } else if has(&["kick", "bass drum", "bd"]) {
            Role::Kick
        } else if has(&["snare", "rim", "stick", "sd"]) {
            Role::Snare
        } else if has(&["tom"]) {
            Role::Tom
        } else if has(&["crash", "ride", "china", "splash", "cymbal", "bell", "stack"]) {
            Role::Cymbal
        } else {
            Role::Other
        }
    }

    pub fn of_gm(note: u8) -> Role {
        match note {
            35 | 36 => Role::Kick,
            37 | 38 | 40 => Role::Snare,
            41 | 43 | 45 | 47 | 48 | 50 => Role::Tom,
            42 | 46 => Role::HiHat,
            44 => Role::HatPedal,
            49 | 51 | 52 | 53 | 55 | 57 | 59 => Role::Cymbal,
            _ => Role::Other,
        }
    }
}

pub fn clean_drum_name(name: &str) -> Result<String, String> {
    let cleaned: String = name.trim().chars().filter(|c| !c.is_control()).take(MAX_DRUM_NAME).collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        return Err("Enter a name".into());
    }
    Ok(cleaned)
}

impl Kit {
    pub fn parse(text: &str) -> Kit {
        let mut kit = Kit::default();
        for line in text.lines() {
            let line = line.trim();
            if let Some(ms) = line.strip_prefix("retrigger-ms ") {
                if let Ok(ms) = ms.trim().parse::<u32>() {
                    kit.retrigger_ms = ms.min(1000);
                }
            } else if let Some(cc) = line.strip_prefix("hihat-pedal-cc ") {
                if let Ok(cc) = cc.trim().parse::<u8>() {
                    kit.pedal_cc = cc.min(127);
                }
            } else if let Some(rest) = line.strip_prefix("note ") {
                let (note, name) = rest.trim().split_once(' ').unwrap_or((rest, ""));
                if let (Ok(note), Ok(name)) = (note.parse::<u8>(), clean_drum_name(name)) {
                    if note < 128 {
                        kit.names.insert(note, name);
                    }
                }
            }
        }
        kit
    }

    pub fn format(&self) -> String {
        let mut s = String::from(
            "# Modometer settings for this profile. The plugin rewrites this file when you change them.\n",
        );
        let _ = writeln!(s, "retrigger-ms {}", self.retrigger_ms);
        if self.pedal_cc > 0 {
            let _ = writeln!(s, "hihat-pedal-cc {}", self.pedal_cc);
        }
        for (note, name) in &self.names {
            let _ = writeln!(s, "note {note} {name}");
        }
        s
    }

    /// The profile's settings, or the defaults if it has none.
    pub fn load(profile_dir: &Path) -> Kit {
        fs::read_to_string(profile_dir.join(SETTINGS_FILE)).map(|t| Kit::parse(&t)).unwrap_or_default()
    }

    /// Writes the settings atomically (temp file + rename).
    pub fn save(&self, profile_dir: &Path) -> io::Result<()> {
        fs::create_dir_all(profile_dir)?;
        let tmp = profile_dir.join(format!("{SETTINGS_FILE}.tmp{}", std::process::id()));
        fs::write(&tmp, self.format())?;
        fs::rename(tmp, profile_dir.join(SETTINGS_FILE))
    }

    pub fn name(&self, note: u8) -> Option<&str> {
        self.names.get(&note).map(String::as_str)
    }

    /// A drum kit (some drums are named), as opposed to keys.
    pub fn is_drums(&self) -> bool {
        !self.names.is_empty()
    }

    /// Each note's role: from its name, else from General MIDI.
    pub fn roles(&self) -> [Role; 128] {
        std::array::from_fn(|n| match self.names.get(&(n as u8)) {
            Some(name) => Role::of_name(name),
            None => Role::of_gm(n as u8),
        })
    }

    /// The name of the drum `note` leads (the lowest note of a named drum), else the GM name, else
    /// the note's name.
    pub fn drum_label(&self, note: u8) -> String {
        match self.names.get(&note) {
            Some(n) => n.clone(),
            None => crate::stats::gm_drum_name(note).map(str::to_string).unwrap_or_else(|| crate::stats::note_name(note)),
        }
    }

    /// For each note, the drum it belongs to: the lowest note with the same name (names compared
    /// without case), or the note itself if it has no name.
    pub fn groups(&self) -> [u8; 128] {
        let mut groups: [u8; 128] = std::array::from_fn(|i| i as u8);
        let mut first: BTreeMap<String, u8> = BTreeMap::new();
        for (&note, name) in &self.names {
            groups[note as usize] = *first.entry(name.to_lowercase()).or_insert(note);
        }
        groups
    }

    /// Named drums, ordered by their lowest note, with their notes.
    pub fn drums(&self) -> Vec<(String, Vec<u8>)> {
        let mut drums: Vec<(String, Vec<u8>)> = Vec::new();
        for (&note, name) in &self.names {
            match drums.iter_mut().find(|(n, _)| n.to_lowercase() == name.to_lowercase()) {
                Some((_, notes)) => notes.push(note),
                None => drums.push((name.clone(), vec![note])),
            }
        }
        drums
    }

    /// Hits per named drum, then "Other" for notes without a name (if any were played).
    pub fn drum_counts(&self, notes: &[u64; 128]) -> Vec<(String, u64)> {
        let mut out: Vec<(String, u64)> =
            self.drums().into_iter().map(|(name, ns)| (name, ns.iter().map(|&n| notes[n as usize]).sum())).collect();
        let other: u64 = (0..128u8).filter(|n| !self.names.contains_key(n)).map(|n| notes[n as usize]).sum();
        if other > 0 {
            out.push(("Other".into(), other));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut kit = Kit { retrigger_ms: 40, ..Kit::default() };
        kit.names.insert(36, "Kick".into());
        kit.names.insert(38, "Snare".into());
        kit.names.insert(49, "Crash 2".into());
        assert_eq!(Kit::parse(&kit.format()), kit);
        kit.pedal_cc = PEDAL_CC;
        assert_eq!(Kit::parse(&kit.format()), kit);
    }

    #[test]
    fn lenient_parse() {
        let kit = Kit::parse("# hi\nretrigger-ms x\nnote 200 Nope\nnote 38   My  snare  \nnote 39\nrandom\n");
        assert_eq!(kit.retrigger_ms, DEFAULT_RETRIGGER_MS);
        assert_eq!(kit.names.len(), 1);
        assert_eq!(kit.name(38), Some("My  snare"));
        assert_eq!(Kit::parse("").retrigger_ms, DEFAULT_RETRIGGER_MS);
    }

    #[test]
    fn shared_names_are_one_drum() {
        let kit = Kit::parse("note 38 Snare\nnote 40 snare\nnote 36 Kick\n");
        let g = kit.groups();
        assert_eq!(g[38], 38);
        assert_eq!(g[40], 38);
        assert_eq!(g[36], 36);
        assert_eq!(g[42], 42);
        assert_eq!(kit.drums(), vec![("Kick".to_string(), vec![36]), ("Snare".to_string(), vec![38, 40])]);
        let mut notes = [0u64; 128];
        notes[36] = 5;
        notes[38] = 3;
        notes[40] = 2;
        notes[42] = 7;
        assert_eq!(
            kit.drum_counts(&notes),
            vec![("Kick".to_string(), 5), ("Snare".to_string(), 5), ("Other".to_string(), 7)]
        );
    }

    #[test]
    fn roles() {
        assert_eq!(Role::of_name("Hi-hat pedal"), Role::HatPedal);
        assert_eq!(Role::of_name("Hi-hat edge"), Role::HiHat);
        assert_eq!(Role::of_name("Kick"), Role::Kick);
        assert_eq!(Role::of_name("Cross stick"), Role::Snare);
        assert_eq!(Role::of_name("Floor tom"), Role::Tom);
        assert_eq!(Role::of_name("Ride bell"), Role::Cymbal);
        assert_eq!(Role::of_name("Cowbell"), Role::Cymbal);
        assert_eq!(Role::of_name("Clap"), Role::Other);
        let kit = Kit::parse("note 50 Crash 1\n");
        let r = kit.roles();
        assert_eq!(r[50], Role::Cymbal);
        assert_eq!(r[36], Role::Kick);
        assert!(kit.is_drums());
        assert!(!Kit::default().is_drums());
    }
}
