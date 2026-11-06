// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! On-disk storage: one folder per profile, one append-only text log per UTC month.
//!
//! Each line is one minute that had hits:
//!
//! ```text
//! 2026-11-05T15:04Z 212 n36:40,38:22,42:150 v64:12,100:200 b118:212 j190:1520
//! ```
//!
//! UTC minute, total hits, then `n` note:count pairs and `v` velocity:count pairs (both sparse,
//! ascending), then optional analysis fields (see [`Detail`]). A minute can appear more than once
//! (several saves or instances); readers add them. A line without its trailing newline (torn by a
//! crash) is ignored. Fields a reader doesn't know are skipped, so older versions still read the
//! counts.
//!
//! Months that are over (older than last month) are compacted: their lines are merged (one per
//! minute, in order) and gzipped into `YYYY-MM.log.gz`, which `zcat` or `zless` can read. If a line
//! for such a month is ever appended again, it goes to a fresh `YYYY-MM.log` next to the `.gz`, and
//! readers read both until the next compaction merges them.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const MAX_PROFILE_NAME: usize = 48;
pub const DEFAULT_PROFILE: &str = "Default";
const CACHE_FILE: &str = "totals.cache";

/// Velocity bins of [`Detail::drum_vels`] and of the hi-hat pedal: 16 values each.
pub const BINS: usize = 8;
/// Melodic intervals are counted up to two octaves.
pub const MAX_INTERVAL: usize = 24;

/// What the analysis adds to a minute (all optional; see `analyze`). Times are in tenths of a
/// millisecond, so sums stay whole numbers and lines from several saves add up exactly.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detail {
    /// `t` The host's tempo while its transport was playing: BPM -> hits.
    pub host_tempo: BTreeMap<u16, u32>,
    /// `b` The tempo you played at, as estimated from the hits: BPM -> hits.
    pub tempo: BTreeMap<u16, u32>,
    /// `j` Hits measured against the grid of your own tempo, and the sum of their distances from
    /// it (0.1 ms).
    pub timing: (u32, u64),
    /// `o` Per drum (its lowest note): hits measured, and the sum of their signed offsets from the
    /// grid (0.1 ms; negative is early).
    pub offsets: BTreeMap<u8, (u32, i64)>,
    /// `s` Swung off-beats: how many, and the sum of their positions in the beat (0.1 %; 500 is
    /// straight, 667 triplet swing).
    pub swing: (u32, u64),
    /// `d` Tempo drift: minutes measured, and the sum of their BPM change per minute (0.01 BPM).
    pub drift: (u32, i64),
    /// `w` Per drum (its lowest note): hits per velocity bin (1-15, 16-31, ... 112-127).
    pub drum_vels: BTreeMap<u8, [u32; BINS]>,
    /// `f` The fastest run of one drum (or of notes, on keys): (note, hits a second × 10).
    pub speed: Option<(u8, u16)>,
    /// `r` Rolls: fast runs on a snare or tom.
    pub rolls: u32,
    /// `l` Fills: runs across the toms.
    pub fills: u32,
    /// `p` Hi-hat hits per pedal position (CC 4 in bins of 16; 0 is pedal up).
    pub pedal: [u32; BINS],
    /// `c` Chords (three or more pitch classes at once): pitch-class mask (bit 0 = C) -> count.
    pub chords: BTreeMap<u16, u32>,
    /// `i` Melodic intervals between successive single notes: semitones (0..=24) -> count.
    pub intervals: [u32; MAX_INTERVAL + 1],
}

impl Detail {
    pub fn is_empty(&self) -> bool {
        *self == Detail::default()
    }

    pub fn merge(&mut self, o: &Detail) {
        for (dst, src) in [(&mut self.host_tempo, &o.host_tempo), (&mut self.tempo, &o.tempo), (&mut self.chords, &o.chords)] {
            for (&k, &v) in src {
                *dst.entry(k).or_default() += v;
            }
        }
        add2(&mut self.timing, o.timing);
        add2(&mut self.swing, o.swing);
        self.drift = (self.drift.0 + o.drift.0, self.drift.1 + o.drift.1);
        for (&k, &(n, s)) in &o.offsets {
            let e = self.offsets.entry(k).or_default();
            *e = (e.0 + n, e.1 + s);
        }
        for (&k, bins) in &o.drum_vels {
            let e = self.drum_vels.entry(k).or_default();
            for (a, b) in e.iter_mut().zip(bins) {
                *a += b;
            }
        }
        if let Some((n, r)) = o.speed {
            if self.speed.is_none_or(|(_, best)| r > best) {
                self.speed = Some((n, r));
            }
        }
        self.rolls += o.rolls;
        self.fills += o.fills;
        for (a, b) in self.pedal.iter_mut().zip(&o.pedal) {
            *a += b;
        }
        for (a, b) in self.intervals.iter_mut().zip(&o.intervals) {
            *a += b;
        }
    }
}

fn add2(a: &mut (u32, u64), b: (u32, u64)) {
    *a = (a.0 + b.0, a.1 + b.1);
}

/// Hits in one minute.
#[derive(Clone, Debug, PartialEq)]
pub struct MinuteCounts {
    pub total: u32,
    pub notes: [u32; 128],
    pub vels: [u32; 128],
    pub detail: Detail,
}

impl Default for MinuteCounts {
    fn default() -> Self {
        Self { total: 0, notes: [0; 128], vels: [0; 128], detail: Detail::default() }
    }
}

impl MinuteCounts {
    pub fn add_hit(&mut self, note: u8, vel: u8) {
        self.total += 1;
        self.notes[(note & 127) as usize] += 1;
        self.vels[(vel & 127) as usize] += 1;
    }

    /// Nothing worth a line: no hits and no analysis.
    pub fn is_empty(&self) -> bool {
        self.total == 0 && self.detail.is_empty()
    }

    pub fn clear(&mut self) {
        self.total = 0;
        self.notes = [0; 128];
        self.vels = [0; 128];
        if !self.detail.is_empty() {
            self.detail = Detail::default();
        }
    }

    pub fn merge(&mut self, o: &MinuteCounts) {
        self.total += o.total;
        for (a, b) in self.notes.iter_mut().zip(&o.notes) {
            *a += b;
        }
        for (a, b) in self.vels.iter_mut().zip(&o.vels) {
            *a += b;
        }
        self.detail.merge(&o.detail);
    }
}

// ---- Time helpers (UTC civil dates without a time zone library) ----

/// Days since 1970-01-01 for a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// (year, month, day) for days since 1970-01-01.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A UTC month, as used for log file names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Month {
    pub year: i32,
    pub month: u32,
}

impl Month {
    pub fn of_minute(minute: i64) -> Month {
        let (y, m, _) = civil_from_days(minute.div_euclid(1440));
        Month { year: y as i32, month: m }
    }
    pub fn file_name(self) -> String {
        format!("{:04}-{:02}.log", self.year, self.month)
    }
    pub fn gz_file_name(self) -> String {
        format!("{:04}-{:02}.log.gz", self.year, self.month)
    }
    /// The month of `YYYY-MM.log` or `YYYY-MM.log.gz`.
    pub fn parse_file_name(name: &str) -> Option<Month> {
        let stem = name.strip_suffix(".gz").unwrap_or(name).strip_suffix(".log")?;
        let (y, m) = stem.split_once('-')?;
        if y.len() != 4 || m.len() != 2 {
            return None;
        }
        let month = Month { year: y.parse().ok()?, month: m.parse().ok()? };
        (1..=12).contains(&month.month).then_some(month)
    }
    pub fn start_minute(self) -> i64 {
        days_from_civil(self.year as i64, self.month, 1) * 1440
    }
    pub fn prev(self) -> Month {
        if self.month == 1 {
            Month { year: self.year - 1, month: 12 }
        } else {
            Month { year: self.year, month: self.month - 1 }
        }
    }
    pub fn next(self) -> Month {
        if self.month == 12 {
            Month { year: self.year + 1, month: 1 }
        } else {
            Month { year: self.year, month: self.month + 1 }
        }
    }
}

pub fn now_minute() -> i64 {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    secs.div_euclid(60)
}

// ---- Line format ----

/// Writes `prefix` then `k:v` pairs joined by commas, if there are any.
fn pairs<K: std::fmt::Display, V: std::fmt::Display>(out: &mut String, prefix: &str, it: impl Iterator<Item = (K, V)>) {
    let mut first = true;
    for (k, v) in it {
        out.push_str(if first { prefix } else { "," });
        first = false;
        let _ = write!(out, "{k}:{v}");
    }
}

fn nonzero<T: Copy + PartialEq + Default>(arr: &[T]) -> impl Iterator<Item = (usize, T)> + '_ {
    arr.iter().copied().enumerate().filter(|&(_, n)| n != T::default())
}

pub fn format_line(minute: i64, c: &MinuteCounts, out: &mut String) {
    let (y, mo, d) = civil_from_days(minute.div_euclid(1440));
    let mins = minute.rem_euclid(1440);
    let _ = write!(out, "{:04}-{:02}-{:02}T{:02}:{:02}Z {}", y, mo, d, mins / 60, mins % 60, c.total);
    pairs(out, " n", nonzero(&c.notes));
    pairs(out, " v", nonzero(&c.vels));
    let d = &c.detail;
    pairs(out, " t", d.host_tempo.iter());
    pairs(out, " b", d.tempo.iter());
    if d.timing.0 > 0 {
        let _ = write!(out, " j{}:{}", d.timing.0, d.timing.1);
    }
    let mut first = true;
    for (k, (n, sum)) in &d.offsets {
        out.push_str(if first { " o" } else { "," });
        first = false;
        let _ = write!(out, "{k}:{n}:{sum}");
    }
    if d.swing.0 > 0 {
        let _ = write!(out, " s{}:{}", d.swing.0, d.swing.1);
    }
    if d.drift.0 > 0 {
        let _ = write!(out, " d{}:{}", d.drift.0, d.drift.1);
    }
    let mut first = true;
    for (k, bins) in &d.drum_vels {
        let used = bins.iter().rposition(|&n| n > 0).map(|i| i + 1).unwrap_or(1);
        out.push_str(if first { " w" } else { "," });
        first = false;
        let _ = write!(out, "{k}:");
        for (i, n) in bins[..used].iter().enumerate() {
            let _ = write!(out, "{}{n}", if i > 0 { "." } else { "" });
        }
    }
    if let Some((note, rate)) = d.speed {
        let _ = write!(out, " f{note}:{rate}");
    }
    if d.rolls > 0 {
        let _ = write!(out, " r{}", d.rolls);
    }
    if d.fills > 0 {
        let _ = write!(out, " l{}", d.fills);
    }
    pairs(out, " p", nonzero(&d.pedal));
    pairs(out, " c", d.chords.iter().map(|(m, n)| (format!("{m:x}"), n)));
    pairs(out, " i", nonzero(&d.intervals));
    out.push('\n');
}

fn parse_minute(s: &str) -> Option<i64> {
    // 2026-11-05T15:04Z
    let b = s.as_bytes();
    if b.len() != 17 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b'Z' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let mut v = 0i64;
        for &c in &b[r] {
            if !c.is_ascii_digit() {
                return None;
            }
            v = v * 10 + (c - b'0') as i64;
        }
        Some(v)
    };
    let (y, mo, d, h, mi) = (num(0..4)?, num(5..7)?, num(8..10)?, num(11..13)?, num(14..16)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 {
        return None;
    }
    Some(days_from_civil(y, mo as u32, d as u32) * 1440 + h * 60 + mi)
}

/// `k:v,k:v` into a 128-slot array (keys must be < 128).
fn parse_into(s: &str, out: &mut [u32; 128]) -> Option<()> {
    for pair in s.split(',') {
        let (k, v) = pair.split_once(':')?;
        let k: u8 = k.parse().ok()?;
        if k > 127 {
            return None;
        }
        out[k as usize] += v.parse::<u32>().ok()?;
    }
    Some(())
}

fn parse_map<K: Ord + Copy>(s: &str, key: impl Fn(&str) -> Option<K>, out: &mut BTreeMap<K, u32>) -> Option<()> {
    for pair in s.split(',') {
        let (k, v) = pair.split_once(':')?;
        *out.entry(key(k)?).or_default() += v.parse::<u32>().ok()?;
    }
    Some(())
}

fn parse_array(s: &str, out: &mut [u32]) -> Option<()> {
    for pair in s.split(',') {
        let (k, v) = pair.split_once(':')?;
        *out.get_mut(k.parse::<usize>().ok()?)? += v.parse::<u32>().ok()?;
    }
    Some(())
}

fn parse_two<A: std::str::FromStr, B: std::str::FromStr>(s: &str) -> Option<(A, B)> {
    let (a, b) = s.split_once(':')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// Parses one analysis field into `d`. Malformed fields are skipped (they never cost the counts).
fn parse_detail(p: &str, d: &mut Detail) -> Option<()> {
    let mut chars = p.chars();
    let tag = chars.next()?;
    let rest = chars.as_str();
    match tag {
        't' => parse_map(rest, |k| k.parse().ok(), &mut d.host_tempo)?,
        'b' => parse_map(rest, |k| k.parse().ok(), &mut d.tempo)?,
        'c' => parse_map(rest, |k| u16::from_str_radix(k, 16).ok().filter(|&m| m < 1 << 12), &mut d.chords)?,
        'j' => add2(&mut d.timing, parse_two(rest)?),
        's' => add2(&mut d.swing, parse_two(rest)?),
        'd' => {
            let (n, s): (u32, i64) = parse_two(rest)?;
            d.drift = (d.drift.0 + n, d.drift.1 + s);
        }
        'o' => {
            for item in rest.split(',') {
                let mut it = item.split(':');
                let (k, n, s) = (it.next()?.parse::<u8>().ok()?, it.next()?.parse::<u32>().ok()?, it.next()?.parse::<i64>().ok()?);
                let e = d.offsets.entry(k & 127).or_default();
                *e = (e.0 + n, e.1 + s);
            }
        }
        'w' => {
            for item in rest.split(',') {
                let (k, bins) = item.split_once(':')?;
                let k = k.parse::<u8>().ok()? & 127;
                let mut parsed = [0u32; BINS];
                for (i, n) in bins.split('.').enumerate() {
                    *parsed.get_mut(i)? = n.parse().ok()?;
                }
                let e = d.drum_vels.entry(k).or_default();
                for (a, b) in e.iter_mut().zip(parsed) {
                    *a += b;
                }
            }
        }
        'f' => {
            let (note, rate): (u8, u16) = parse_two(rest)?;
            if d.speed.is_none_or(|(_, best)| rate > best) {
                d.speed = Some((note & 127, rate));
            }
        }
        'r' => d.rolls += rest.parse::<u32>().ok()?,
        'l' => d.fills += rest.parse::<u32>().ok()?,
        'p' => parse_array(rest, &mut d.pedal)?,
        'i' => parse_array(rest, &mut d.intervals)?,
        _ => {}
    }
    Some(())
}

/// One parsed log line. Reused between lines to avoid allocation.
#[derive(Default)]
pub struct Line {
    pub minute: i64,
    pub total: u64,
    /// The line's notes, velocities and analysis (with `detail`; `total` is in `total`).
    pub counts: MinuteCounts,
}

/// Parses a line (without its newline). With `detail` false, only the minute and total are read.
pub fn parse_line(s: &str, detail: bool, line: &mut Line) -> bool {
    let mut parts = s.split(' ');
    let Some(minute) = parts.next().and_then(parse_minute) else { return false };
    let Some(total) = parts.next().and_then(|t| t.parse::<u32>().ok()) else { return false };
    line.minute = minute;
    line.total = total as u64;
    if detail {
        let c = &mut line.counts;
        c.clear();
        c.total = total;
        for p in parts {
            if let Some(rest) = p.strip_prefix('n') {
                if parse_into(rest, &mut c.notes).is_none() {
                    return false;
                }
            } else if let Some(rest) = p.strip_prefix('v') {
                if parse_into(rest, &mut c.vels).is_none() {
                    return false;
                }
            } else {
                // Into a scratch copy, so a malformed field leaves nothing half-read.
                let mut d = Detail::default();
                if parse_detail(p, &mut d).is_some() {
                    c.detail.merge(&d);
                }
            }
        }
    }
    true
}

/// Calls `f` for each valid complete line of `text`.
fn scan_text(text: &[u8], detail: bool, f: &mut impl FnMut(&Line)) {
    let text = String::from_utf8_lossy(text);
    let mut line = Line::default();
    for l in text.lines() {
        if parse_line(l, detail, &mut line) {
            f(&line);
        }
    }
}

/// Reads complete lines of `path` from byte `from`, calling `f` for each valid one.
/// Returns the offset just past the last complete line.
pub fn scan_file(path: &Path, from: u64, detail: bool, mut f: impl FnMut(&Line)) -> io::Result<u64> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    if from >= len {
        return Ok(from.min(len));
    }
    file.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::with_capacity((len - from) as usize);
    file.take(len - from).read_to_end(&mut buf)?;
    let complete = match buf.iter().rposition(|&b| b == b'\n') {
        Some(i) => i + 1,
        None => return Ok(from),
    };
    scan_text(&buf[..complete], detail, &mut f);
    Ok(from + complete as u64)
}

/// Reads a compacted month (`.log.gz`), calling `f` for each line.
pub fn scan_gz(path: &Path, detail: bool, mut f: impl FnMut(&Line)) -> io::Result<()> {
    let mut buf = Vec::new();
    flate2::read::GzDecoder::new(File::open(path)?).read_to_end(&mut buf)?;
    let complete = buf.iter().rposition(|&b| b == b'\n').map(|i| i + 1).unwrap_or(0);
    scan_text(&buf[..complete], detail, &mut f);
    Ok(())
}

// ---- Store and profiles ----

/// The data folder: `$XDG_DATA_HOME/modometer`, else `~/.local/share/modometer`.
pub fn default_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("MODOMETER_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("modometer")
}

#[derive(Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf) -> Store {
        Store { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn profiles_dir(&self) -> PathBuf {
        self.root.join("profiles")
    }

    pub fn profile_dir(&self, name: &str) -> PathBuf {
        self.profiles_dir().join(name)
    }

    pub fn list_profiles(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(self.profiles_dir())
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.'))
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        names
    }

    /// Creates the profile's folder if needed and returns its (cleaned) name.
    pub fn create_profile(&self, name: &str) -> Result<String, String> {
        let name = clean_profile_name(name)?;
        fs::create_dir_all(self.profile_dir(&name)).map_err(|e| e.to_string())?;
        Ok(name)
    }

    /// The months that have a log (plain or compacted), oldest first.
    pub fn months(&self, profile: &str) -> Vec<Month> {
        let mut months: Vec<Month> = fs::read_dir(self.profile_dir(profile))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str().and_then(Month::parse_file_name))
            .collect();
        months.sort();
        months.dedup();
        months
    }

    /// The month's plain log, appended to.
    pub fn month_path(&self, profile: &str, month: Month) -> PathBuf {
        self.profile_dir(profile).join(month.file_name())
    }

    /// The month's compacted log.
    pub fn gz_path(&self, profile: &str, month: Month) -> PathBuf {
        self.profile_dir(profile).join(month.gz_file_name())
    }

    /// Calls `f` for every line of a month: the compacted part, then the plain log.
    pub fn scan_month(&self, profile: &str, month: Month, detail: bool, mut f: impl FnMut(&Line)) {
        let gz = self.gz_path(profile, month);
        if gz.exists() {
            if let Err(e) = scan_gz(&gz, detail, &mut f) {
                eprintln!("Modometer: couldn't read {}: {e}", gz.display());
            }
        }
        let _ = scan_file(&self.month_path(profile, month), 0, detail, f);
    }

    /// Appends minutes to their month logs. Each month's lines go out in one `O_APPEND` write, so
    /// several writers (instances, processes) never interleave inside a line.
    /// Returns the months written.
    pub fn append(&self, profile: &str, minutes: &BTreeMap<i64, MinuteCounts>) -> io::Result<Vec<Month>> {
        let mut by_month: BTreeMap<Month, String> = BTreeMap::new();
        for (&minute, counts) in minutes {
            if !counts.is_empty() {
                format_line(minute, counts, by_month.entry(Month::of_minute(minute)).or_default());
            }
        }
        fs::create_dir_all(self.profile_dir(profile))?;
        for (&month, text) in &by_month {
            let path = self.month_path(profile, month);
            let mut file = OpenOptions::new().create(true).append(true).read(true).open(&path)?;
            let mut text = std::borrow::Cow::Borrowed(text.as_str());
            // A torn last line (crash mid-write) must not swallow our first line.
            let len = file.metadata()?.len();
            if len > 0 {
                let mut last = [0u8];
                file.seek(SeekFrom::Start(len - 1))?;
                file.read_exact(&mut last)?;
                if last[0] != b'\n' {
                    text = format!("\n{text}").into();
                }
            }
            file.write_all(text.as_bytes())?;
        }
        Ok(by_month.into_keys().collect())
    }

    /// Compacts the plain logs of months before `before`: merges each with its `.gz` (if any) into
    /// one line per minute, in order, gzipped. Returns the months compacted with their hit totals
    /// and new `.gz` sizes.
    ///
    /// Safe with other writers because nothing appends to a month that's over; still, the log is
    /// only replaced if it didn't change while being read.
    pub fn compact(&self, profile: &str, before: Month) -> io::Result<Vec<(Month, u64, u64)>> {
        let mut done = Vec::new();
        for month in self.months(profile).into_iter().filter(|&m| m < before) {
            let log = self.month_path(profile, month);
            let Ok(meta) = fs::metadata(&log) else { continue };
            let len = meta.len();
            let mut minutes: BTreeMap<i64, MinuteCounts> = BTreeMap::new();
            let gz = self.gz_path(profile, month);
            if gz.exists() {
                scan_gz(&gz, true, |l| minutes.entry(l.minute).or_default().merge(&l.counts))?;
            }
            let read = scan_file(&log, 0, true, |l| minutes.entry(l.minute).or_default().merge(&l.counts))?;
            let mut text = String::new();
            let mut total = 0u64;
            for (&m, c) in &minutes {
                if !c.is_empty() {
                    total += c.total as u64;
                    format_line(m, c, &mut text);
                }
            }
            let tmp = self.profile_dir(profile).join(format!("{}.tmp{}", month.gz_file_name(), std::process::id()));
            {
                let mut enc = flate2::write::GzEncoder::new(File::create(&tmp)?, flate2::Compression::best());
                enc.write_all(text.as_bytes())?;
                enc.finish()?.sync_all()?;
            }
            // Skip it if the log changed meanwhile (or a torn line was being completed).
            if fs::metadata(&log).map(|m| m.len()).unwrap_or(0) != len || read != len {
                let _ = fs::remove_file(&tmp);
                continue;
            }
            fs::rename(&tmp, &gz)?;
            let _ = fs::remove_file(&log);
            done.push((month, total, fs::metadata(&gz)?.len()));
        }
        Ok(done)
    }

    /// Reads the totals cache.
    pub fn read_cache(&self, profile: &str) -> Cache {
        let mut cache = Cache::default();
        if let Ok(text) = fs::read_to_string(self.profile_dir(profile).join(CACHE_FILE)) {
            for l in text.lines() {
                let parts: Vec<&str> = l.split(' ').collect();
                let (m, gz, b, t) = match parts[..] {
                    [m, b, t] => (m, false, b, t),
                    [m, "gz", b, t] => (m, true, b, t),
                    _ => continue,
                };
                if let (Some(m), Ok(b), Ok(t)) = (Month::parse_file_name(&format!("{m}.log")), b.parse(), t.parse()) {
                    if gz { &mut cache.gz } else { &mut cache.logs }.insert(m, (b, t));
                }
            }
        }
        cache
    }

    /// Writes the totals cache atomically (temp file + rename). Skipped if unchanged.
    pub fn write_cache(&self, profile: &str, cache: &Cache) -> io::Result<()> {
        if self.read_cache(profile) == *cache {
            return Ok(());
        }
        let mut text = String::new();
        for (m, (b, t)) in &cache.logs {
            let _ = writeln!(text, "{:04}-{:02} {} {}", m.year, m.month, b, t);
        }
        // Older versions skip these lines (they don't read compacted months).
        for (m, (b, t)) in &cache.gz {
            let _ = writeln!(text, "{:04}-{:02} gz {} {}", m.year, m.month, b, t);
        }
        let dir = self.profile_dir(profile);
        fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!("{CACHE_FILE}.tmp{}", std::process::id()));
        fs::write(&tmp, text)?;
        fs::rename(tmp, dir.join(CACHE_FILE))
    }
}

/// Per-month hit totals, so loading a profile doesn't read years of logs: for each plain log the
/// bytes read and the hits in them, for each compacted month its `.gz` size and hits.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cache {
    pub logs: BTreeMap<Month, (u64, u64)>,
    pub gz: BTreeMap<Month, (u64, u64)>,
}

pub fn clean_profile_name(name: &str) -> Result<String, String> {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| if c == '/' || c == '\\' || c.is_control() { '-' } else { c })
        .take(MAX_PROFILE_NAME)
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        return Err("Enter a name".into());
    }
    if cleaned.starts_with('.') {
        return Err("A name can't start with a dot".into());
    }
    Ok(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("modometer-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    #[test]
    fn civil_round_trip() {
        for z in [-800_000i64, -1, 0, 1, 20_000, 20_397, 2_000_000] {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(days_from_civil(2026, 11, 5), 20762);
    }

    #[test]
    fn line_round_trip() {
        let mut c = MinuteCounts::default();
        c.add_hit(36, 100);
        c.add_hit(36, 90);
        c.add_hit(42, 100);
        let minute = days_from_civil(2026, 11, 5) * 1440 + 15 * 60 + 4;
        let mut s = String::new();
        format_line(minute, &c, &mut s);
        assert_eq!(s, "2026-11-05T15:04Z 3 n36:2,42:1 v90:1,100:2\n");
        let mut line = Line::default();
        assert!(parse_line(s.trim_end(), true, &mut line));
        assert_eq!(line.minute, minute);
        assert_eq!(line.total, 3);
        assert_eq!(line.counts, c);
        // Old lines have no analysis; unknown fields are skipped.
        assert!(parse_line("2026-11-05T15:04Z 3 n36:3 v90:3 zfuture:1", true, &mut line));
        assert_eq!(line.counts.notes[36], 3);
        assert!(line.counts.detail.is_empty());
    }

    fn detailed() -> MinuteCounts {
        let mut c = MinuteCounts::default();
        c.add_hit(36, 100);
        c.add_hit(38, 20);
        let d = &mut c.detail;
        d.host_tempo.insert(120, 2);
        d.tempo.insert(118, 2);
        d.timing = (2, 153);
        d.offsets.insert(36, (1, -40));
        d.offsets.insert(38, (1, 113));
        d.swing = (1, 580);
        d.drift = (1, -35);
        d.drum_vels.insert(36, [0, 0, 0, 0, 0, 0, 1, 0]);
        d.drum_vels.insert(38, [0, 1, 0, 0, 0, 0, 0, 0]);
        d.speed = Some((38, 142));
        d.rolls = 1;
        d.fills = 2;
        d.pedal[7] = 1;
        d.chords.insert(0b1001_0001, 3);
        d.intervals[2] = 4;
        c
    }

    #[test]
    fn detail_round_trip_and_merge() {
        let c = detailed();
        let mut s = String::new();
        format_line(0, &c, &mut s);
        assert_eq!(
            s,
            "1970-01-01T00:00Z 2 n36:1,38:1 v20:1,100:1 t120:2 b118:2 j2:153 o36:1:-40,38:1:113 s1:580 \
             d1:-35 w36:0.0.0.0.0.0.1,38:0.1 f38:142 r1 l2 p7:1 c91:3 i2:4\n"
        );
        let mut line = Line::default();
        assert!(parse_line(s.trim_end(), true, &mut line));
        assert_eq!(line.counts, c);

        let mut sum = c.clone();
        sum.merge(&c);
        assert_eq!(sum.total, 4);
        assert_eq!(sum.detail.timing, (4, 306));
        assert_eq!(sum.detail.offsets[&38], (2, 226));
        assert_eq!(sum.detail.speed, Some((38, 142)));
        assert_eq!(sum.detail.chords[&0x91], 6);

        // A malformed analysis field is skipped; the counts survive.
        assert!(parse_line("1970-01-01T00:00Z 2 n36:2 jx:1 r3", true, &mut line));
        assert_eq!(line.counts.notes[36], 2);
        assert_eq!(line.counts.detail.timing, (0, 0));
        assert_eq!(line.counts.detail.rolls, 3);
    }

    #[test]
    fn analysis_only_lines() {
        // The analysis can finish after a minute's hits were saved: such a line has no hits.
        let mut c = MinuteCounts::default();
        assert!(c.is_empty());
        c.detail.rolls = 1;
        let mut s = String::new();
        format_line(0, &c, &mut s);
        assert_eq!(s, "1970-01-01T00:00Z 0 r1\n");
    }

    #[test]
    fn compaction() {
        let store = temp_store("compact");
        let profile = store.create_profile("Drums").unwrap();
        let sep = Month { year: 2026, month: 9 };
        let m0 = sep.start_minute() + 600;
        let c = detailed();
        let mut minutes = BTreeMap::new();
        minutes.insert(m0, c.clone());
        minutes.insert(m0 + 1, c.clone());
        store.append(&profile, &minutes).unwrap();
        store.append(&profile, &minutes).unwrap(); // the same minutes again, as a second instance would
        let mut oct = BTreeMap::new();
        oct.insert(Month { year: 2026, month: 10 }.start_minute(), c.clone());
        store.append(&profile, &oct).unwrap();

        let done = store.compact(&profile, Month { year: 2026, month: 10 }).unwrap();
        assert_eq!(done.len(), 1);
        assert_eq!((done[0].0, done[0].1), (sep, 8));
        assert!(!store.month_path(&profile, sep).exists());
        assert!(store.gz_path(&profile, sep).exists());
        assert!(store.month_path(&profile, Month { year: 2026, month: 10 }).exists());
        assert_eq!(store.months(&profile).len(), 2);

        // One merged line per minute.
        let mut lines = Vec::new();
        store.scan_month(&profile, sep, true, |l| lines.push((l.minute, l.counts.clone())));
        assert_eq!(lines.len(), 2);
        let mut twice = c.clone();
        twice.merge(&c);
        assert_eq!(lines[0], (m0, twice));

        // A late line for a compacted month lands in a new plain log; readers see both.
        let mut late = BTreeMap::new();
        late.insert(m0 + 5, c.clone());
        store.append(&profile, &late).unwrap();
        let mut total = 0;
        store.scan_month(&profile, sep, false, |l| total += l.total);
        assert_eq!(total, 10);
        // And the next compaction merges it in.
        store.compact(&profile, Month { year: 2026, month: 10 }).unwrap();
        let mut n = 0;
        store.scan_month(&profile, sep, false, |_| n += 1);
        assert_eq!(n, 3);
        assert!(!store.month_path(&profile, sep).exists());

        let mut cache = Cache::default();
        cache.logs.insert(Month { year: 2026, month: 10 }, (100, 2));
        cache.gz.insert(sep, (55, 10));
        store.write_cache(&profile, &cache).unwrap();
        assert_eq!(store.read_cache(&profile), cache);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn rejects_garbage() {
        let mut line = Line::default();
        assert!(!parse_line("2026-11-05T15:0", true, &mut line));
        assert!(!parse_line("2026-13-05T15:04Z 3", true, &mut line));
        assert!(!parse_line("2026-11-05T15:04Z x", true, &mut line));
        assert!(!parse_line("2026-11-05T15:04Z 3 n200:1", true, &mut line));
    }

    #[test]
    fn append_and_scan_with_torn_line() {
        let store = temp_store("append");
        let profile = store.create_profile("Drums").unwrap();
        let base = days_from_civil(2026, 11, 30) * 1440 + 23 * 60 + 59;
        let mut minutes = BTreeMap::new();
        let mut c = MinuteCounts::default();
        c.add_hit(38, 64);
        minutes.insert(base, c.clone()); // November
        minutes.insert(base + 1, c.clone()); // December
        let written = store.append(&profile, &minutes).unwrap();
        assert_eq!(written, vec![Month { year: 2026, month: 11 }, Month { year: 2026, month: 12 }]);

        // Simulate a crash mid-line, then another append.
        let nov = store.month_path(&profile, Month { year: 2026, month: 11 });
        OpenOptions::new().append(true).open(&nov).unwrap().write_all(b"2026-11-30T23:5").unwrap();
        let mut one = BTreeMap::new();
        one.insert(base, c);
        store.append(&profile, &one).unwrap();

        let mut total = 0;
        let end = scan_file(&nov, 0, true, |l| total += l.total).unwrap();
        assert_eq!(total, 2);
        assert_eq!(end, fs::metadata(&nov).unwrap().len());
        assert_eq!(store.months(&profile).len(), 2);
        assert_eq!(store.list_profiles(), vec!["Drums".to_string()]);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn profile_names() {
        assert_eq!(clean_profile_name("  Keys/Synth ").unwrap(), "Keys-Synth");
        assert!(clean_profile_name("   ").is_err());
        assert!(clean_profile_name(".hidden").is_err());
    }
}
