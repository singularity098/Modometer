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

use crate::store::{self, Detail, MinuteCounts, Month, Store};

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
    /// The analysis over the range, added up.
    pub detail: Detail,
    /// Per bucket: velocities, the tempo you played at, and timing (hits measured, sum of their
    /// distances from the grid in 0.1 ms).
    pub bucket_vels: Vec<[u32; 128]>,
    pub bucket_tempo: Vec<BTreeMap<u16, u32>>,
    pub bucket_timing: Vec<(u32, u64)>,
    /// Hits per weekday (Monday first) and hour, local time.
    pub week_hours: [[u64; 24]; 7],
    /// Hits per local day.
    pub days: BTreeMap<NaiveDate, u64>,
    /// Stretches of playing, oldest first.
    pub sessions: Vec<Session>,
}

/// A stretch of playing: minutes with hits, no more than [`SESSION_GAP`] minutes apart.
#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    /// First minute, and one past the last (unix minutes).
    pub start: i64,
    pub end: i64,
    pub hits: u64,
    /// Estimated minutes of playing.
    pub played: f64,
    pub pitch_classes: [u64; 12],
}

/// Minutes without a hit that end a session.
pub const SESSION_GAP: i64 = 10;

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
        detail: Detail::default(),
        bucket_vels: vec![[0; 128]; n],
        bucket_tempo: vec![BTreeMap::new(); n],
        bucket_timing: vec![(0, 0); n],
        week_hours: [[0; 24]; 7],
        days: BTreeMap::new(),
        sessions: Vec::new(),
    };
    let bucket_of = |minute: i64| edge_minutes.partition_point(|&e| e <= minute).saturating_sub(1);

    // (minute, hits, pitch classes)
    let mut minutes: Vec<(i64, u64, [u32; 12])> = Vec::new();
    scan(store, profile, (start_m, end_m), pending, true, |minute, total, line, unsaved| {
        let i = bucket_of(minute);
        if let Some(c) = stats.counts.get_mut(i) {
            *c += total;
        }
        stats.total += total;
        let mut pc = [0u32; 12];
        if let Some(c) = line.map(|l| &l.counts).or(unsaved) {
            for (k, &n) in c.notes.iter().enumerate() {
                stats.notes[k] += n as u64;
                pc[k % 12] += n;
            }
            for (k, &n) in c.vels.iter().enumerate() {
                stats.vels[k] += n as u64;
            }
            stats.detail.merge(&c.detail);
            if i < n {
                for (a, b) in stats.bucket_vels[i].iter_mut().zip(&c.vels) {
                    *a += b;
                }
                for (&bpm, &hits) in &c.detail.tempo {
                    *stats.bucket_tempo[i].entry(bpm).or_default() += hits;
                }
                let t = &mut stats.bucket_timing[i];
                *t = (t.0 + c.detail.timing.0, t.1 + c.detail.timing.1);
            }
        }
        minutes.push((minute, total, pc));
    });

    // Merge repeated minutes.
    minutes.sort_unstable_by_key(|m| m.0);
    let mut merged: Vec<(i64, u64, [u32; 12])> = Vec::with_capacity(minutes.len());
    for (m, t, pc) in minutes {
        match merged.last_mut() {
            Some(last) if last.0 == m => {
                last.1 += t;
                for (a, b) in last.2.iter_mut().zip(pc) {
                    *a += b;
                }
            }
            _ => merged.push((m, t, pc)),
        }
    }

    for &(minute, hits, pc) in &merged {
        if hits == 0 {
            continue; // analysis that finished after the minute's hits were saved
        }
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
        if let Some(t) = Local.timestamp_opt(minute * 60, 0).single() {
            let t = t.naive_local();
            stats.week_hours[t.weekday().num_days_from_monday() as usize][t.hour() as usize] += hits;
            *stats.days.entry(t.date()).or_default() += hits;
        }
        match stats.sessions.last_mut() {
            Some(s) if minute - s.end < SESSION_GAP => {
                s.end = minute + 1;
                s.hits += hits;
                s.played += played;
                for (a, b) in s.pitch_classes.iter_mut().zip(pc) {
                    *a += b as u64;
                }
            }
            _ => stats.sessions.push(Session {
                start: minute,
                end: minute + 1,
                hits,
                played,
                pitch_classes: pc.map(|n| n as u64),
            }),
        }
    }
    stats
}

/// Mean, median and most common of a weighted histogram (value -> weight).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub mean: f64,
    pub median: f64,
    pub mode: f64,
    pub count: u64,
}

pub fn summarize<K: Copy + Into<f64>>(hist: &BTreeMap<K, u32>) -> Option<Summary> {
    let count: u64 = hist.values().map(|&n| n as u64).sum();
    if count == 0 {
        return None;
    }
    let mean = hist.iter().map(|(&k, &n)| k.into() * n as f64).sum::<f64>() / count as f64;
    let mut acc = 0;
    let mut median = 0.0;
    for (&k, &n) in hist {
        acc += n as u64;
        if acc * 2 >= count {
            median = k.into();
            break;
        }
    }
    // The mode, smoothed over neighbours (1 BPM apart is the same tempo, give or take).
    let smoothed = |k: f64| hist.iter().filter(|(&j, _)| (j.into() - k).abs() <= 1.0).map(|(_, &n)| n as u64).sum::<u64>();
    let mode = hist.keys().map(|&k| k.into()).max_by_key(|&k| (smoothed(k), hist.iter().find(|(&j, _)| j.into() == k).map(|(_, &n)| n).unwrap_or(0))).unwrap_or(0.0);
    Some(Summary { mean, median, mode, count })
}

/// The value at `q` (0..1) of a velocity histogram.
pub fn quantile(vels: &[u32; 128], q: f64) -> Option<u8> {
    let count: u64 = vels.iter().map(|&n| n as u64).sum();
    if count == 0 {
        return None;
    }
    let target = (q * count as f64).ceil().max(1.0) as u64;
    let mut acc = 0;
    for (v, &n) in vels.iter().enumerate() {
        acc += n as u64;
        if acc >= target {
            return Some(v as u8);
        }
    }
    Some(127)
}

/// Average of a velocity histogram.
pub fn average_velocity(vels: &[u32; 128]) -> Option<f64> {
    let count: u64 = vels.iter().map(|&n| n as u64).sum();
    (count > 0).then(|| vels.iter().enumerate().map(|(v, &n)| v as f64 * n as f64).sum::<f64>() / count as f64)
}

// ---- Everything played: calendar, streaks, personal bests ----

/// A profile's whole history, by local day.
#[derive(Clone, Debug, Default)]
pub struct History {
    pub days: BTreeMap<NaiveDate, u64>,
    /// The busiest minute ever: (unix minute, hits).
    pub best_minute: Option<(i64, u64)>,
}

/// A run of days in a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Streak {
    pub first: NaiveDate,
    pub last: NaiveDate,
}

impl Streak {
    pub fn days(&self) -> i64 {
        (self.last - self.first).num_days() + 1
    }
}

impl History {
    pub fn compute(store: &Store, profile: &str, pending: &BTreeMap<i64, MinuteCounts>) -> History {
        let mut minutes: BTreeMap<i64, u64> = BTreeMap::new();
        for month in store.months(profile) {
            store.scan_month(profile, month, false, |l| *minutes.entry(l.minute).or_default() += l.total);
        }
        for (&m, c) in pending {
            *minutes.entry(m).or_default() += c.total as u64;
        }
        let mut h = History::default();
        for (&m, &n) in &minutes {
            if n == 0 {
                continue;
            }
            *h.days.entry(local_date(m)).or_default() += n;
            if h.best_minute.is_none_or(|(_, b)| n > b) {
                h.best_minute = Some((m, n));
            }
        }
        h
    }

    /// Runs of days played, oldest first.
    pub fn streaks(&self) -> Vec<Streak> {
        let mut out: Vec<Streak> = Vec::new();
        for &d in self.days.keys() {
            match out.last_mut() {
                Some(s) if s.last.succ_opt() == Some(d) => s.last = d,
                _ => out.push(Streak { first: d, last: d }),
            }
        }
        out
    }

    /// The streak still going on `today` (played today or yesterday), if any.
    pub fn current_streak(&self, today: NaiveDate) -> Option<Streak> {
        self.streaks().into_iter().last().filter(|s| s.last >= today.pred_opt().unwrap_or(today))
    }

    pub fn longest_streak(&self) -> Option<Streak> {
        // The earliest of equally long ones.
        self.streaks().into_iter().rev().max_by_key(|s| s.days())
    }

    pub fn best_day(&self) -> Option<(NaiveDate, u64)> {
        self.days.iter().rev().max_by_key(|(_, &n)| n).map(|(&d, &n)| (d, n))
    }

    /// The best week (Monday it starts on, hits).
    pub fn best_week(&self) -> Option<(NaiveDate, u64)> {
        let mut weeks: BTreeMap<NaiveDate, u64> = BTreeMap::new();
        for (&d, &n) in &self.days {
            *weeks.entry(d - Duration::days(d.weekday().num_days_from_monday() as i64)).or_default() += n;
        }
        weeks.into_iter().rev().max_by_key(|&(_, n)| n)
    }
}

fn local_date(minute: i64) -> NaiveDate {
    Local.timestamp_opt(minute * 60, 0).single().map(|t| t.date_naive()).unwrap_or_default()
}

/// The longest run of days in a row within `days`.
pub fn longest_run(days: &BTreeMap<NaiveDate, u64>) -> i64 {
    let h = History { days: days.clone(), best_minute: None };
    h.longest_streak().map(|s| s.days()).unwrap_or(0)
}

// ---- Harmony ----

/// Chord shapes: (suffix, intervals from the root). Earlier ones win when a set of notes fits
/// several (C E G A is Am7 rather than C6).
const CHORDS: [(&str, &[u8]); 20] = [
    ("", &[0, 4, 7]),
    ("m", &[0, 3, 7]),
    ("7", &[0, 4, 7, 10]),
    ("m7", &[0, 3, 7, 10]),
    ("maj7", &[0, 4, 7, 11]),
    ("sus4", &[0, 5, 7]),
    ("sus2", &[0, 2, 7]),
    ("dim", &[0, 3, 6]),
    ("aug", &[0, 4, 8]),
    ("m7b5", &[0, 3, 6, 10]),
    ("dim7", &[0, 3, 6, 9]),
    ("6", &[0, 4, 7, 9]),
    ("m6", &[0, 3, 7, 9]),
    ("add9", &[0, 2, 4, 7]),
    ("madd9", &[0, 2, 3, 7]),
    ("9", &[0, 2, 4, 7, 10]),
    ("maj9", &[0, 2, 4, 7, 11]),
    ("m9", &[0, 2, 3, 7, 10]),
    ("7sus4", &[0, 5, 7, 10]),
    ("mMaj7", &[0, 3, 7, 11]),
];

/// The name of a set of pitch classes (bit 0 = C): "Am7", or its notes if it's no common chord.
pub fn chord_name(mask: u16) -> String {
    for (suffix, shape) in CHORDS {
        for root in 0..12u16 {
            let m: u16 = shape.iter().fold(0, |m, &i| m | 1 << ((root + i as u16) % 12));
            if m == mask {
                return format!("{}{}", NOTE_NAMES[root as usize], suffix);
            }
        }
    }
    (0..12).filter(|i| mask >> i & 1 == 1).map(|i| NOTE_NAMES[i]).collect::<Vec<_>>().join(" ")
}

/// Short names of intervals 0..=12 semitones.
pub const INTERVALS: [&str; 13] = ["Unison", "m2", "M2", "m3", "M3", "P4", "Tritone", "P5", "m6", "M6", "m7", "M7", "Octave"];

pub fn interval_name(semitones: usize) -> String {
    match INTERVALS.get(semitones) {
        Some(n) => n.to_string(),
        None => format!("{semitones} semitones"),
    }
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

    #[test]
    fn summaries() {
        let mut h: BTreeMap<u16, u32> = BTreeMap::new();
        h.insert(100, 1);
        h.insert(120, 10);
        h.insert(121, 4);
        h.insert(140, 5);
        let s = summarize(&h).unwrap();
        assert_eq!(s.count, 20);
        assert_eq!(s.median, 120.0);
        assert_eq!(s.mode, 120.0);
        assert!((s.mean - 124.2).abs() < 1e-9);
        assert_eq!(summarize::<u16>(&BTreeMap::new()), None);

        let mut v = [0u32; 128];
        v[20] = 10;
        v[100] = 90;
        assert_eq!(quantile(&v, 0.1), Some(20));
        assert_eq!(quantile(&v, 0.5), Some(100));
        assert_eq!(average_velocity(&v), Some(92.0));
    }

    #[test]
    fn chords_and_intervals() {
        assert_eq!(chord_name(0b0000_1001_0001), "C");
        assert_eq!(chord_name(0b0010_0001_0001), "Am");
        assert_eq!(chord_name(0b0010_1001_0001), "Am7");
        assert_eq!(chord_name(0b1000_1001_0001), "Cmaj7");
        assert_eq!(chord_name(0b0000_1010_0001), "Csus4");
        assert_eq!(chord_name(0b0000_0000_0111), "C C# D");
        assert_eq!(interval_name(7), "P5");
        assert_eq!(interval_name(14), "14 semitones");
    }

    #[test]
    fn streaks_and_bests() {
        let d = |day: u32| NaiveDate::from_ymd_opt(2026, 11, day).unwrap();
        let mut h = History::default();
        for (day, n) in [(1, 10), (2, 20), (3, 5), (5, 50), (6, 5), (9, 1), (10, 1)] {
            h.days.insert(d(day), n);
        }
        assert_eq!(h.streaks().len(), 3);
        assert_eq!(h.longest_streak(), Some(Streak { first: d(1), last: d(3) }));
        assert_eq!(h.current_streak(d(11)), Some(Streak { first: d(9), last: d(10) }));
        assert_eq!(h.current_streak(d(12)), None);
        assert_eq!(h.best_day(), Some((d(5), 50)));
        // Mon 2 Nov – Sun 8 Nov 2026: 20 + 5 + 50 + 5.
        assert_eq!(h.best_week(), Some((d(2), 80)));
        assert_eq!(longest_run(&h.days), 3);
    }

    #[test]
    fn sessions_split_on_gaps() {
        let dir = std::env::temp_dir().join(format!("modometer-sessions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(dir.clone());
        store.create_profile("P").unwrap();
        let start = local_to_minute(Local::now().naive_local().date().and_hms_opt(8, 0, 0).unwrap());
        let mut minutes = BTreeMap::new();
        for m in [0, 1, 2, 5, 30, 31] {
            let mut c = MinuteCounts::default();
            c.add_hit(60, 100);
            c.add_hit(64, 50);
            if m == 30 {
                c.detail.tempo.insert(120, 2);
                c.detail.timing = (2, 300);
            }
            minutes.insert(start + m, c);
        }
        store.append("P", &minutes).unwrap();
        let s = compute(&store, "P", Query { span: Span::Day, offset: 0, bucket: Bucket::Hour }, &BTreeMap::new(), None);
        assert_eq!(s.sessions.len(), 2);
        assert_eq!((s.sessions[0].start, s.sessions[0].end, s.sessions[0].hits), (start, start + 6, 8));
        assert_eq!(s.sessions[1].pitch_classes[4], 2);
        assert_eq!(s.days.values().sum::<u64>(), 12);
        assert_eq!(s.week_hours.iter().flatten().sum::<u64>(), 12);
        assert_eq!(s.bucket_timing[8], (2, 300));
        assert_eq!(s.bucket_tempo[8].get(&120), Some(&2));
        assert_eq!(s.bucket_vels[8][100], 6);
        assert_eq!(s.detail.tempo.get(&120), Some(&2));
        let _ = std::fs::remove_dir_all(dir);
    }
}
