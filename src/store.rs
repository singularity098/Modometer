// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! On-disk storage: one folder per profile, one append-only text log per UTC month.
//!
//! Each line is one minute that had hits:
//!
//! ```text
//! 2026-11-05T15:04Z 212 n36:40,38:22,42:150 v64:12,100:200
//! ```
//!
//! UTC minute, total hits, then `n` note:count pairs and `v` velocity:count pairs (both sparse,
//! ascending). A minute can appear more than once (several saves or instances); readers add them.
//! A line without its trailing newline (torn by a crash) is ignored.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const MAX_PROFILE_NAME: usize = 48;
pub const DEFAULT_PROFILE: &str = "Default";
const CACHE_FILE: &str = "totals.cache";

/// Hits in one minute.
#[derive(Clone)]
pub struct MinuteCounts {
    pub total: u32,
    pub notes: [u32; 128],
    pub vels: [u32; 128],
}

impl Default for MinuteCounts {
    fn default() -> Self {
        Self { total: 0, notes: [0; 128], vels: [0; 128] }
    }
}

impl MinuteCounts {
    pub fn add_hit(&mut self, note: u8, vel: u8) {
        self.total += 1;
        self.notes[(note & 127) as usize] += 1;
        self.vels[(vel & 127) as usize] += 1;
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
    pub fn parse_file_name(name: &str) -> Option<Month> {
        let stem = name.strip_suffix(".log")?;
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

pub fn format_line(minute: i64, c: &MinuteCounts, out: &mut String) {
    let (y, mo, d) = civil_from_days(minute.div_euclid(1440));
    let mins = minute.rem_euclid(1440);
    let _ = write!(out, "{:04}-{:02}-{:02}T{:02}:{:02}Z {}", y, mo, d, mins / 60, mins % 60, c.total);
    for (prefix, arr) in [(" n", &c.notes), (" v", &c.vels)] {
        let mut first = true;
        for (i, &n) in arr.iter().enumerate() {
            if n == 0 {
                continue;
            }
            out.push_str(if first { prefix } else { "," });
            first = false;
            let _ = write!(out, "{}:{}", i, n);
        }
    }
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

fn parse_pairs(s: &str, out: &mut Vec<(u8, u32)>) -> Option<()> {
    for pair in s.split(',') {
        let (k, v) = pair.split_once(':')?;
        let k: u8 = k.parse().ok()?;
        if k > 127 {
            return None;
        }
        out.push((k, v.parse().ok()?));
    }
    Some(())
}

/// One parsed log line. The vectors are reused between lines to avoid allocation.
#[derive(Default)]
pub struct Line {
    pub minute: i64,
    pub total: u64,
    pub notes: Vec<(u8, u32)>,
    pub vels: Vec<(u8, u32)>,
}

/// Parses a line (without its newline). With `detail` false, note and velocity pairs are skipped.
pub fn parse_line(s: &str, detail: bool, line: &mut Line) -> bool {
    line.notes.clear();
    line.vels.clear();
    let mut parts = s.split(' ');
    let Some(minute) = parts.next().and_then(parse_minute) else { return false };
    let Some(total) = parts.next().and_then(|t| t.parse().ok()) else { return false };
    line.minute = minute;
    line.total = total;
    if detail {
        for p in parts {
            let ok = if let Some(rest) = p.strip_prefix('n') {
                parse_pairs(rest, &mut line.notes)
            } else if let Some(rest) = p.strip_prefix('v') {
                parse_pairs(rest, &mut line.vels)
            } else {
                Some(())
            };
            if ok.is_none() {
                return false;
            }
        }
    }
    true
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
    let text = String::from_utf8_lossy(&buf[..complete]);
    let mut line = Line::default();
    for l in text.lines() {
        if parse_line(l, detail, &mut line) {
            f(&line);
        }
    }
    Ok(from + complete as u64)
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

    /// The months that have a log file, oldest first.
    pub fn months(&self, profile: &str) -> Vec<Month> {
        let mut months: Vec<Month> = fs::read_dir(self.profile_dir(profile))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().to_str().and_then(Month::parse_file_name))
            .collect();
        months.sort();
        months
    }

    pub fn month_path(&self, profile: &str, month: Month) -> PathBuf {
        self.profile_dir(profile).join(month.file_name())
    }

    /// Appends minutes to their month logs. Each month's lines go out in one `O_APPEND` write, so
    /// several writers (instances, processes) never interleave inside a line.
    /// Returns the months written.
    pub fn append(&self, profile: &str, minutes: &BTreeMap<i64, MinuteCounts>) -> io::Result<Vec<Month>> {
        let mut by_month: BTreeMap<Month, String> = BTreeMap::new();
        for (&minute, counts) in minutes {
            if counts.total > 0 {
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

    /// Reads the per-month totals cache: month -> (bytes covered, total).
    pub fn read_cache(&self, profile: &str) -> BTreeMap<Month, (u64, u64)> {
        let mut map = BTreeMap::new();
        if let Ok(text) = fs::read_to_string(self.profile_dir(profile).join(CACHE_FILE)) {
            for l in text.lines() {
                let mut it = l.split(' ');
                if let (Some(m), Some(b), Some(t)) = (it.next(), it.next(), it.next()) {
                    if let (Some(m), Ok(b), Ok(t)) =
                        (Month::parse_file_name(&format!("{m}.log")), b.parse(), t.parse())
                    {
                        map.insert(m, (b, t));
                    }
                }
            }
        }
        map
    }

    /// Writes the totals cache atomically (temp file + rename). Skipped if unchanged.
    pub fn write_cache(&self, profile: &str, cache: &BTreeMap<Month, (u64, u64)>) -> io::Result<()> {
        if self.read_cache(profile) == *cache {
            return Ok(());
        }
        let mut text = String::new();
        for (m, (b, t)) in cache {
            let _ = writeln!(text, "{:04}-{:02} {} {}", m.year, m.month, b, t);
        }
        let dir = self.profile_dir(profile);
        fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!("{CACHE_FILE}.tmp{}", std::process::id()));
        fs::write(&tmp, text)?;
        fs::rename(tmp, dir.join(CACHE_FILE))
    }
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
        assert_eq!(line.notes, vec![(36, 2), (42, 1)]);
        assert_eq!(line.vels, vec![(90, 1), (100, 2)]);
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
