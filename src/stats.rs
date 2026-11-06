// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! Stats over a date range: hits per time bucket, per note and per velocity.
//! Ranges and buckets follow local time (days start at local midnight, weeks on Monday).
//!
//! Time played: the logs have one line per minute with hits, so the minutes logged are an upper
//! bound on the time played. A minute at the start or end of a session, or with a pause in it, has
//! fewer hits than a minute of steady playing, so each minute also gets an estimate: its hits over
//! the profile's *pace* (hits in a full minute), at most one minute. See [`pace`].

use std::collections::BTreeMap;

use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, TimeZone, Timelike};

use crate::store::{self, MinuteCounts, Month, Store};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Span {
    Hour,
    Day,
    Week,
    Month,
    Year,
    All,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bucket {
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

pub const SPANS: [Span; 6] = [Span::Hour, Span::Day, Span::Week, Span::Month, Span::Year, Span::All];
pub const BUCKETS: [Bucket; 6] = [Bucket::Minute, Bucket::Hour, Bucket::Day, Bucket::Week, Bucket::Month, Bucket::Year];

impl Span {
    pub fn label(self) -> &'static str {
        match self {
            Span::Hour => "Hour",
            Span::Day => "Day",
            Span::Week => "Week",
            Span::Month => "Month",
            Span::Year => "Year",
            Span::All => "All",
        }
    }

    /// The bucket sizes that make sense for this span (not too few bars, not thousands).
    pub fn buckets(self) -> &'static [Bucket] {
        match self {
            Span::Hour => &[Bucket::Minute],
            Span::Day => &[Bucket::Minute, Bucket::Hour],
            Span::Week => &[Bucket::Hour, Bucket::Day],
            Span::Month => &[Bucket::Day, Bucket::Week],
            Span::Year => &[Bucket::Day, Bucket::Week, Bucket::Month],
            Span::All => &[Bucket::Day, Bucket::Week, Bucket::Month, Bucket::Year],
        }
    }

    pub fn default_bucket(self) -> Bucket {
        match self {
            Span::Hour => Bucket::Minute,
            Span::Day => Bucket::Hour,
            Span::Week | Span::Month => Bucket::Day,
            Span::Year | Span::All => Bucket::Month,
        }
    }
}

impl Bucket {
    pub fn label(self) -> &'static str {
        match self {
            Bucket::Minute => "Minute",
            Bucket::Hour => "Hour",
            Bucket::Day => "Day",
            Bucket::Week => "Week",
            Bucket::Month => "Month",
            Bucket::Year => "Year",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Query {
    pub span: Span,
    /// 0 = the current hour/day/week/..., -1 = the one before, and so on.
    pub offset: i32,
    pub bucket: Bucket,
}

pub struct Stats {
    pub query: Query,
    pub title: String,
    /// Bucket start times (local), and one past the last bucket's end.
    pub edges: Vec<NaiveDateTime>,
    pub counts: Vec<u64>,
    /// Minutes with hits, per bucket.
    pub logged: Vec<u32>,
    /// Estimated minutes of playing, per bucket (see [`pace`]).
    pub played: Vec<f64>,
    /// The most hits in one minute, per bucket.
    pub peak: Vec<u64>,
    /// The busiest minute in the range: (unix minute, hits).
    pub busiest: Option<(i64, u64)>,
    pub notes: [u64; 128],
    pub vels: [u64; 128],
    pub total: u64,
    pub logged_total: u64,
    pub played_total: f64,
    /// The pace the estimate used: hits that make a full minute. None: too little history, and
    /// every logged minute counts as a full one.
    pub pace: Option<f64>,
    /// Buckets that have started by now, for averages per bucket.
    pub elapsed: usize,
}

impl Stats {
    pub fn bucket_label(&self, i: usize) -> String {
        let t = self.edges[i];
        match self.query.bucket {
            Bucket::Minute => t.format("%a %-d %b, %H:%M").to_string(),
            Bucket::Hour => t.format("%a %-d %b, %H:00").to_string(),
            Bucket::Day => t.format("%a %-d %b %Y").to_string(),
            Bucket::Week => format!("Week of {}", t.format("%-d %b %Y")),
            Bucket::Month => t.format("%B %Y").to_string(),
            Bucket::Year => t.format("%Y").to_string(),
        }
    }

    /// The busiest minute's local time, e.g. "Tue 3 Nov, 20:14".
    pub fn busiest_label(&self) -> Option<String> {
        let (m, _) = self.busiest?;
        let t = Local.timestamp_opt(m * 60, 0).single()?;
        Some(t.format("%a %-d %b, %H:%M").to_string())
    }

    /// Hits per minute of playing over the range, if anything was played.
    pub fn per_minute(&self) -> Option<f64> {
        (self.played_total > 0.0).then(|| self.total as f64 / self.played_total)
    }

    /// A short axis label for bucket `i`.
    pub fn axis_label(&self, i: usize) -> String {
        let t = self.edges[i];
        match self.query.bucket {
            Bucket::Minute => t.format("%H:%M").to_string(),
            Bucket::Hour => t.format("%H:00").to_string(),
            Bucket::Day => t.format("%-d %b").to_string(),
            Bucket::Week => t.format("%-d %b").to_string(),
            Bucket::Month => t.format("%b %Y").to_string(),
            Bucket::Year => t.format("%Y").to_string(),
        }
    }
}

fn midnight(d: NaiveDate) -> NaiveDateTime {
    d.and_hms_opt(0, 0, 0).unwrap()
}

fn add_months(d: NaiveDate, months: i32) -> NaiveDate {
    let total = d.year() * 12 + d.month0() as i32 + months;
    NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, 1).unwrap()
}

/// The next bucket boundary after local time `t`.
fn next_edge(t: NaiveDateTime, bucket: Bucket) -> NaiveDateTime {
    let d = t.date();
    match bucket {
        Bucket::Minute => t.with_second(0).unwrap() + Duration::minutes(1),
        Bucket::Hour => t.with_minute(0).unwrap().with_second(0).unwrap() + Duration::hours(1),
        Bucket::Day => midnight(d + Duration::days(1)),
        Bucket::Week => midnight(d + Duration::days(7 - d.weekday().num_days_from_monday() as i64)),
        Bucket::Month => midnight(add_months(d.with_day(1).unwrap(), 1)),
        Bucket::Year => midnight(NaiveDate::from_ymd_opt(d.year() + 1, 1, 1).unwrap()),
    }
}

/// Local time to unix minutes. For a time skipped by a DST change, the next valid hour.
fn local_to_minute(t: NaiveDateTime) -> i64 {
    match Local.from_local_datetime(&t).earliest() {
        Some(dt) => dt.timestamp().div_euclid(60),
        None => Local
            .from_local_datetime(&(t + Duration::hours(1)))
            .earliest()
            .map(|dt| dt.timestamp().div_euclid(60))
            .unwrap_or(0),
    }
}

/// The local [start, end) of a query, and its title.
pub fn range(q: Query, store: &Store, profile: &str, now: NaiveDateTime) -> (NaiveDateTime, NaiveDateTime, String) {
    let today = now.date();
    let o = q.offset;
    match q.span {
        Span::Hour => {
            let start = now.with_minute(0).unwrap().with_second(0).unwrap().with_nanosecond(0).unwrap()
                + Duration::hours(o as i64);
            let end = start + Duration::hours(1);
            (start, end, format!("{} – {}", start.format("%a %-d %b %Y, %H:00"), end.format("%H:00")))
        }
        Span::Day => {
            let d = today + Duration::days(o as i64);
            (midnight(d), midnight(d + Duration::days(1)), d.format("%A %-d %B %Y").to_string())
        }
        Span::Week => {
            let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64) + Duration::weeks(o as i64);
            let sunday = monday + Duration::days(6);
            let title = if monday.year() != sunday.year() {
                format!("{} – {}", monday.format("%-d %b %Y"), sunday.format("%-d %b %Y"))
            } else if monday.month() != sunday.month() {
                format!("{} – {}", monday.format("%-d %b"), sunday.format("%-d %b %Y"))
            } else {
                format!("{} – {}", monday.format("%-d"), sunday.format("%-d %b %Y"))
            };
            (midnight(monday), midnight(monday + Duration::days(7)), title)
        }
        Span::Month => {
            let first = add_months(today.with_day(1).unwrap(), o);
            (midnight(first), midnight(add_months(first, 1)), first.format("%B %Y").to_string())
        }
        Span::Year => {
            let y = today.year() + o;
            let start = NaiveDate::from_ymd_opt(y, 1, 1).unwrap();
            (midnight(start), midnight(NaiveDate::from_ymd_opt(y + 1, 1, 1).unwrap()), y.to_string())
        }
        Span::All => {
            let first = store
                .months(profile)
                .first()
                .map(|m| {
                    // Logs are per UTC month; start at the local date of its first minute.
                    let t = Local.timestamp_opt(m.start_minute() * 60, 0).single().map(|t| t.naive_local());
                    t.map(|t| t.date()).unwrap_or(today)
                })
                .unwrap_or(today)
                .min(today);
            let start = match q.bucket {
                Bucket::Year => NaiveDate::from_ymd_opt(first.year(), 1, 1).unwrap(),
                Bucket::Month => first.with_day(1).unwrap(),
                Bucket::Week => first - Duration::days(first.weekday().num_days_from_monday() as i64),
                _ => first,
            };
            let end = next_edge(midnight(today), q.bucket.max(Bucket::Day));
            (midnight(start), end, "All time".to_string())
        }
    }
}

/// Calls `f(minute, total, line)` for every log line in [start_m, end_m), then for every unsaved
/// minute in it (with `None`). A minute can come more than once (two instances, or saved and
/// unsaved parts of it); callers add them up.
fn scan(
    store: &Store,
    profile: &str,
    (start_m, end_m): (i64, i64),
    pending: &BTreeMap<i64, MinuteCounts>,
    detail: bool,
    mut f: impl FnMut(i64, u64, Option<&store::Line>, Option<&MinuteCounts>),
) {
    let mut month = Month::of_minute(start_m);
    let last = Month::of_minute(end_m - 1);
    let available = store.months(profile);
    while month <= last {
        if available.binary_search(&month).is_ok() {
            store.scan_month(profile, month, detail, |l| {
                if l.minute >= start_m && l.minute < end_m {
                    f(l.minute, l.total, Some(l), None);
                }
            });
        }
        month = month.next();
    }
    for (&minute, c) in pending.range(start_m..end_m) {
        f(minute, c.total as u64, None, Some(c));
    }
}

/// Sorts (minute, hits) pairs and adds up repeated minutes.
fn merge_minutes(mut minutes: Vec<(i64, u64)>) -> Vec<(i64, u64)> {
    minutes.sort_unstable_by_key(|&(m, _)| m);
    let mut out: Vec<(i64, u64)> = Vec::with_capacity(minutes.len());
    for (m, t) in minutes {
        match out.last_mut() {
            Some(last) if last.0 == m => last.1 += t,
            _ => out.push((m, t)),
        }
    }
    out
}

/// Days of history the pace is learned from.
pub const PACE_DAYS: i64 = 90;
/// Mid-session minutes needed before the pace is trusted.
const PACE_MIN_SAMPLES: usize = 10;

/// The pace from sorted, merged (minute, hits) pairs: hits in a minute that counts as a full minute
/// of playing. Learned from minutes in the middle of a session (the minutes before and after it
/// have hits too), whose hits are typical of steady playing, unlike a session's first and last
/// minutes. It's half their median, so the usual ups and downs of steady playing (a slower song,
/// a fill) still count as full minutes, while a minute with a handful of hits counts as seconds.
pub fn pace_of(minutes: &[(i64, u64)]) -> Option<f64> {
    let mut mid: Vec<u64> = minutes
        .windows(3)
        .filter(|w| w[0].0 + 1 == w[1].0 && w[1].0 + 1 == w[2].0)
        .map(|w| w[1].1)
        .collect();
    if mid.len() < PACE_MIN_SAMPLES {
        return None;
    }
    let half = mid.len() / 2;
    let (_, &mut median, _) = mid.select_nth_unstable(half);
    Some((median as f64 / 2.0).max(1.0))
}

/// The profile's pace over the last [`PACE_DAYS`] days (see [`pace_of`]).
pub fn pace(store: &Store, profile: &str, pending: &BTreeMap<i64, MinuteCounts>) -> Option<f64> {
    let now = store::now_minute();
    let mut minutes = Vec::new();
    scan(store, profile, (now - PACE_DAYS * 1440, now + 1), pending, false, |m, t, _, _| minutes.push((m, t)));
    pace_of(&merge_minutes(minutes))
}

/// The estimated minutes of playing in a minute with `hits` hits.
pub fn minute_played(hits: u64, pace: Option<f64>) -> f64 {
    match pace {
        Some(p) => (hits as f64 / p).min(1.0),
        None => 1.0,
    }
}

/// Computes stats from the profile's logs plus its unsaved minutes. `pace` is from [`pace`].
pub fn compute(store: &Store, profile: &str, q: Query, pending: &BTreeMap<i64, MinuteCounts>, pace: Option<f64>) -> Stats {
    let now = Local::now().naive_local();
    let (start, end, title) = range(q, store, profile, now);

    let mut edges = vec![start];
    let mut t = start;
    while t < end && edges.len() < 20_000 {
        t = next_edge(t, q.bucket).min(end);
        edges.push(t);
    }
    let edge_minutes: Vec<i64> = edges.iter().map(|&t| local_to_minute(t)).collect();
    let (start_m, end_m) = (edge_minutes[0], *edge_minutes.last().unwrap());
    let n = edges.len() - 1;
    let elapsed = edges[..n].partition_point(|&e| e <= now).max(1);

    let mut stats = Stats {
        query: q,
        title,
        counts: vec![0; n],
        logged: vec![0; n],
        played: vec![0.0; n],
        peak: vec![0; n],
        busiest: None,
        edges,
        notes: [0; 128],
        vels: [0; 128],
        total: 0,
        logged_total: 0,
        played_total: 0.0,
        pace,
        elapsed,
    };
    let bucket_of = |minute: i64| edge_minutes.partition_point(|&e| e <= minute).saturating_sub(1);

    let mut minutes = Vec::new();
    scan(store, profile, (start_m, end_m), pending, true, |minute, total, line, unsaved| {
        minutes.push((minute, total));
        if let Some(c) = stats.counts.get_mut(bucket_of(minute)) {
            *c += total;
        }
        stats.total += total;
        if let Some(c) = line.map(|l| &l.counts).or(unsaved) {
            for (i, &n) in c.notes.iter().enumerate() {
                stats.notes[i] += n as u64;
            }
            for (i, &n) in c.vels.iter().enumerate() {
                stats.vels[i] += n as u64;
            }
        }
    });
    for (minute, hits) in merge_minutes(minutes) {
        let i = bucket_of(minute);
        let played = minute_played(hits, pace);
        if let (Some(l), Some(p), Some(k)) = (stats.logged.get_mut(i), stats.played.get_mut(i), stats.peak.get_mut(i)) {
            *l += 1;
            *p += played;
            *k = (*k).max(hits);
        }
        if stats.busiest.is_none_or(|(_, b)| hits > b) {
            stats.busiest = Some((minute, hits));
        }
        stats.logged_total += 1;
        stats.played_total += played;
    }
    stats
}

pub const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

/// "C4" for 60 (the convention where middle C is C4).
pub fn note_name(note: u8) -> String {
    format!("{}{}", NOTE_NAMES[(note % 12) as usize], note as i32 / 12 - 1)
}

/// General MIDI drum names for notes 35-81, for profiles of drum kits.
pub fn gm_drum_name(note: u8) -> Option<&'static str> {
    const NAMES: [&str; 47] = [
        "Acoustic Bass Drum", "Bass Drum", "Side Stick", "Snare", "Hand Clap", "Electric Snare",
        "Low Floor Tom", "Closed Hi-Hat", "High Floor Tom", "Pedal Hi-Hat", "Low Tom", "Open Hi-Hat",
        "Low-Mid Tom", "Hi-Mid Tom", "Crash 1", "High Tom", "Ride 1", "China", "Ride Bell",
        "Tambourine", "Splash", "Cowbell", "Crash 2", "Vibraslap", "Ride 2", "Hi Bongo", "Low Bongo",
        "Mute Hi Conga", "Open Hi Conga", "Low Conga", "High Timbale", "Low Timbale", "High Agogo",
        "Low Agogo", "Cabasa", "Maracas", "Short Whistle", "Long Whistle", "Short Guiro", "Long Guiro",
        "Claves", "Hi Wood Block", "Low Wood Block", "Mute Cuica", "Open Cuica", "Mute Triangle",
        "Open Triangle",
    ];
    (35..=81).contains(&note).then(|| NAMES[(note - 35) as usize])
}

/// Hits per pitch class (C..B).
pub fn pitch_classes(notes: &[u64; 128]) -> [u64; 12] {
    let mut pc = [0; 12];
    for (i, &n) in notes.iter().enumerate() {
        pc[i % 12] += n;
    }
    pc
}

/// The major or minor pentatonic / diatonic scale that best covers the played pitch classes,
/// as e.g. "E minor pentatonic". None if there's too little to go on.
pub fn likely_scale(pc: &[u64; 12]) -> Option<String> {
    let total: u64 = pc.iter().sum();
    if total < 50 {
        return None;
    }
    // (name, intervals, root offset for display)
    const SCALES: [(&str, [bool; 12]); 4] = [
        ("major", [true, false, true, false, true, true, false, true, false, true, false, true]),
        ("minor", [true, false, true, true, false, true, false, true, true, false, true, false]),
        ("major pentatonic", [true, false, true, false, true, false, false, true, false, true, false, false]),
        ("minor pentatonic", [true, false, false, true, false, true, false, true, false, false, true, false]),
    ];
    let mut best: Option<(f64, String)> = None;
    for root in 0..12 {
        for (name, mask) in SCALES.iter() {
            let notes_in = mask.iter().filter(|&&b| b).count() as f64;
            let covered: u64 = (0..12).filter(|&i| mask[i]).map(|i| pc[(i + root) % 12]).sum();
            let share = covered as f64 / total as f64;
            // Prefer smaller scales when they cover nearly as much; weight the root a little.
            let score = share - notes_in * 0.012 + pc[root] as f64 / total as f64 * 0.05;
            if share >= 0.85 && best.as_ref().map(|b| score > b.0).unwrap_or(true) {
                best = Some((score, format!("{} {}", NOTE_NAMES[root], name)));
            }
        }
    }
    best.map(|b| b.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_cover_the_range() {
        let store = Store::new(std::env::temp_dir().join("modometer-stats-none"));
        for span in SPANS {
            for &bucket in span.buckets() {
                let s = compute(&store, "nobody", Query { span, offset: -1, bucket }, &BTreeMap::new(), None);
                assert!(!s.counts.is_empty(), "{span:?} {bucket:?}");
                assert!(s.counts.len() <= 1500, "{span:?} {bucket:?} {}", s.counts.len());
                assert!(s.edges.windows(2).all(|w| w[0] < w[1]));
            }
        }
    }

    #[test]
    fn counts_land_in_the_right_bucket() {
        let dir = std::env::temp_dir().join(format!("modometer-stats-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(dir.clone());
        store.create_profile("P").unwrap();
        let now = Local::now().naive_local();
        let today9 = now.date().and_hms_opt(9, 30, 0).unwrap();
        let m = local_to_minute(today9);
        let mut c = MinuteCounts::default();
        c.add_hit(64, 100);
        c.add_hit(64, 100);
        c.add_hit(67, 80);
        let mut minutes = BTreeMap::new();
        minutes.insert(m, c.clone());
        store.append("P", &minutes).unwrap();
        let mut pending = BTreeMap::new();
        pending.insert(m + 60, c);

        let s = compute(&store, "P", Query { span: Span::Day, offset: 0, bucket: Bucket::Hour }, &pending, None);
        assert_eq!(s.total, 6);
        assert_eq!(s.logged[9], 1);
        assert_eq!(s.peak[10], 3);
        assert_eq!(s.busiest, Some((m, 3)));
        assert_eq!(s.logged_total, 2);
        assert_eq!(s.played_total, 2.0);
        // A pace of 6 hits a minute: each 3-hit minute is half a minute of playing.
        let s = compute(&store, "P", Query { span: Span::Day, offset: 0, bucket: Bucket::Hour }, &pending, Some(6.0));
        assert_eq!(s.played[9], 0.5);
        assert_eq!(s.played_total, 1.0);
        assert_eq!(s.per_minute(), Some(6.0));
        assert_eq!(s.counts[9], 3);
        assert_eq!(s.counts[10], 3);
        assert_eq!(s.notes[64], 4);
        assert_eq!(s.vels[80], 2);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_minute_saved_twice_is_one_minute() {
        assert_eq!(merge_minutes(vec![(5, 2), (3, 1), (5, 4)]), vec![(3, 1), (5, 6)]);
    }

    #[test]
    fn pace_comes_from_mid_session_minutes() {
        // Sessions of 12 minutes: 2 hits in the first and last, 110 to 200 in between.
        let mut minutes = Vec::new();
        for session in 0..3 {
            let start = session * 1000;
            minutes.push((start, 2));
            for m in 1..11 {
                minutes.push((start + m, 100 + m as u64 * 10));
            }
            minutes.push((start + 11, 2));
        }
        // Mid-session minutes are 1..=10 of each session (the 2-hit edges have a gap on one side);
        // their median is 160, and the pace half that.
        let pace = pace_of(&minutes).unwrap();
        assert_eq!(pace, 80.0);
        assert_eq!(minute_played(2, Some(pace)), 2.0 / 80.0);
        assert_eq!(minute_played(120, Some(pace)), 1.0);
        // Too little to learn from.
        assert_eq!(pace_of(&minutes[..8]), None);
    }

    #[test]
    fn scale_guess() {
        let mut pc = [0u64; 12];
        for (i, n) in [(4, 40), (7, 30), (9, 25), (11, 20), (2, 20)] {
            pc[i] = n;
        }
        assert_eq!(likely_scale(&pc).as_deref(), Some("E minor pentatonic"));
        assert_eq!(note_name(60), "C4");
        assert_eq!(gm_drum_name(42), Some("Closed Hi-Hat"));
    }
}
