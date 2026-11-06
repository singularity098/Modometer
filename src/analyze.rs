// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! Rhythm and harmony analysis of the hits, run on the saver thread (never the audio thread).
//!
//! **Tempo without a click.** Hits are analysed in one-second chunks, each with three seconds of
//! context on both sides. For each chunk the tempo comes from the intervals between hits: every pair
//! of hits up to two seconds apart votes for its distance, kick and snare (and loud notes) count
//! more than hi-hats, and the beat is the interval with the most votes, nudged towards 115 BPM so
//! eighth notes and half-time feel don't usually win (the usual double/half-tempo ambiguity of beat
//! tracking remains). The tempo of the chunk before is preferred when it's nearly as good, so the
//! estimate doesn't flip between octaves.
//!
//! **The imagined grid.** A grid of sixteenths (or triplets, if they fit clearly better) is then
//! fitted to the hits by least squares, so its tempo and phase follow your own playing, not a
//! click. Each hit's distance from the nearest grid line is how far off the grid it was. Because the
//! grid follows you, this measures steadiness (are the hits evenly spaced?), not whether you kept a
//! tempo you never set. Per drum the signed offsets show which drums sit ahead or behind the others.
//! The beat's position in the grid is where kick and snare land most. Notes clearly between grid
//! lines (grace notes, flams, 32nds) are left out of the timing.
//!
//! **Swing**: in beats with exactly one hit between the beat and the next, its position (50 % is
//! straight, 67 % triplet swing). **Drift**: within a minute, how much the tempo rose or fell.
//!
//! Also, as hits come: rolls (six or more hits of a snare or tom, each within 100 ms), fills (four
//! or more tom hits across at least two toms, each within 350 ms), top speed (eight hits of one
//! drum, or eight notes on keys, in the shortest time), and on keys chords (three or more pitch
//! classes within 35 ms) and the intervals between successive single notes.

use std::collections::{BTreeMap, VecDeque};

use crate::kit::{Kit, Role};
use crate::store::{MinuteCounts, MAX_INTERVAL};

/// Seconds of context each side of a chunk.
const CONTEXT: f64 = 3.0;
/// Seconds per chunk.
const CHUNK: f64 = 1.0;
/// A gap this long (seconds) ends a stretch of playing: the analysis starts afresh.
pub const GAP: f64 = 2.5;
/// Hits closer than this (seconds) are one onset for the tempo.
const MERGE: f64 = 0.03;
const CHORD_WINDOW: f64 = 0.035;
const ROLL_GAP: f64 = 0.1;
const ROLL_MIN: u16 = 6;
const FILL_GAP: f64 = 0.35;
const FILL_MIN: u16 = 4;
const SPEED_RUN: usize = 8;
const MIN_BPM: f64 = 60.0;
const MAX_BPM: f64 = 200.0;

/// A hit as the analysis sees it.
#[derive(Clone, Copy, Debug)]
pub struct AHit {
    /// Seconds, on the instance's sample clock.
    pub t: f64,
    /// Its UTC minute, where its results are counted.
    pub minute: i64,
    pub note: u8,
    pub vel: u8,
}

/// A grid fitted to the hits: lines at `phase + k * step`, `sub` lines per beat, beats starting at
/// line `beat_line` (mod `sub`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub phase: f64,
    pub step: f64,
    pub sub: u32,
    pub beat_line: u32,
}

impl Grid {
    pub fn beat(&self) -> f64 {
        self.step * self.sub as f64
    }
    pub fn bpm(&self) -> f64 {
        60.0 / self.beat()
    }
    /// Signed distance (seconds) from the nearest grid line.
    pub fn offset(&self, t: f64) -> f64 {
        let k = ((t - self.phase) / self.step).round();
        t - (self.phase + k * self.step)
    }
    fn beat_start(&self) -> f64 {
        self.phase + self.beat_line as f64 * self.step
    }
    /// Which beat `t` is in, and where in it (0..1).
    pub fn beat_pos(&self, t: f64) -> (i64, f64) {
        let x = (t - self.beat_start()) / self.beat();
        (x.floor() as i64, x - x.floor())
    }
}

/// How much a hit counts towards finding the beat.
fn weight(role: Role, vel: u8) -> f64 {
    let r = match role {
        Role::Kick | Role::Snare => 1.0,
        Role::Tom => 0.7,
        Role::Cymbal => 0.5,
        Role::HiHat | Role::HatPedal => 0.35,
        Role::Other => 0.6,
    };
    r * (0.4 + 0.6 * vel as f64 / 127.0)
}

/// Merges hits closer than [`MERGE`] into onsets: (time of the first, combined weight).
fn merge_onsets(hits: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(hits.len());
    let mut start = f64::NEG_INFINITY;
    for &(t, w) in hits {
        match out.last_mut() {
            Some(last) if t - start < MERGE => {
                let (hi, lo) = if w > last.1 { (w, last.1) } else { (last.1, w) };
                last.1 = hi + 0.3 * lo;
            }
            _ => {
                out.push((t, w));
                start = t;
            }
        }
    }
    out
}

/// A preference for tempos near 115 BPM, falling off by octave.
fn prior(bpm: f64) -> f64 {
    let x = (bpm / 115.0).log2() / 0.8;
    (-0.5 * x * x).exp()
}

const BIN: f64 = 0.002;
const MAX_LAG: f64 = 2.0;

/// The beat period (seconds) the intervals between onsets vote for, if any. `prefer` is the
/// previous chunk's period.
fn beat_period(onsets: &[(f64, f64)], prefer: Option<f64>) -> Option<f64> {
    let nbins = (MAX_LAG / BIN) as usize + 16;
    let mut hist = vec![0.0f64; nbins];
    // Each interval votes with a Gaussian of 8 ms around it.
    const SIGMA_BINS: f64 = 4.0;
    let kernel: Vec<f64> = (-12..=12).map(|i| (-0.5 * (i as f64 / SIGMA_BINS).powi(2)).exp()).collect();
    for (i, &(ti, wi)) in onsets.iter().enumerate() {
        for &(tj, wj) in &onsets[i + 1..] {
            let d = tj - ti;
            if d > MAX_LAG {
                break;
            }
            if d < 0.09 {
                continue;
            }
            let c = (d / BIN).round() as isize;
            for (k, kw) in kernel.iter().enumerate() {
                let b = c + k as isize - 12;
                if b >= 0 && (b as usize) < nbins {
                    hist[b as usize] += wi * wj * kw;
                }
            }
        }
    }
    let lo = (60.0 / MAX_BPM / BIN).round() as usize;
    let hi = (60.0 / MIN_BPM / BIN).round() as usize;
    let mut best: Option<(f64, usize)> = None;
    for b in lo..=hi {
        let p = b as f64 * BIN;
        // A beat also shows at two beats' distance.
        let mut score = hist[b] + if 2 * b < nbins { 0.5 * hist[2 * b] } else { 0.0 };
        score *= prior(60.0 / p);
        if let Some(q) = prefer {
            if (p / q - 1.0).abs() < 0.08 {
                score *= 1.25;
            }
        }
        if score > 0.0 && best.is_none_or(|(s, _)| score > s) {
            best = Some((score, b));
        }
    }
    best.map(|(_, b)| b as f64 * BIN)
}

/// Least-squares fit of a grid with `sub` lines per beat of about `period` seconds:
/// (phase, step, rms distance from the grid).
fn fit_lines(onsets: &[(f64, f64)], period: f64, sub: u32) -> Option<(f64, f64, f64)> {
    let step0 = period / sub as f64;
    let mut step = step0;
    let mut phase = onsets.iter().max_by(|a, b| a.1.total_cmp(&b.1))?.0;
    for _ in 0..6 {
        let (mut sw, mut sk, mut st, mut skk, mut skt) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for &(t, w) in onsets {
            let k = ((t - phase) / step).round();
            sw += w;
            sk += w * k;
            st += w * t;
            skk += w * k * k;
            skt += w * k * t;
        }
        let det = sw * skk - sk * sk;
        if det.abs() < 1e-9 {
            return None;
        }
        let b = (sw * skt - sk * st) / det;
        let a = (st - b * sk) / sw;
        if !(step0 * 0.92..=step0 * 1.08).contains(&b) {
            return None;
        }
        phase = a;
        step = b;
    }
    let (mut sw, mut se) = (0.0, 0.0);
    for &(t, w) in onsets {
        let k = ((t - phase) / step).round();
        let e = t - (phase + k * step);
        sw += w;
        se += w * e * e;
    }
    Some((phase, step, (se / sw).sqrt()))
}

/// Fits a grid to weighted hits (time order). None if they're too few, or not rhythmic enough.
pub fn fit_grid(hits: &[(f64, f64)], prefer: Option<f64>) -> Option<Grid> {
    let onsets = merge_onsets(hits);
    if onsets.len() < 8 || onsets.last()?.0 - onsets[0].0 < 2.5 {
        return None;
    }
    let period = beat_period(&onsets, prefer)?;
    // Sixteenths, else triplets, else (fast music counted in half time) 32nds or sextuplets, each
    // only if it fits clearly better: a finer grid always fits a little better.
    let (sub, (phase, step, rms)) = [(4, 1.0), (3, 1.33), (8, 2.0), (6, 2.2)]
        .into_iter()
        .filter_map(|(sub, penalty)| fit_lines(&onsets, period, sub).map(|f| (sub, f, f.2 * penalty)))
        .min_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(sub, f, _)| (sub, f))?;
    // Random hits sit on average 0.29 steps from a grid; steady playing far closer.
    if rms / step > 0.2 {
        return None;
    }
    // The beat starts on the line where the weight (kick and snare) lands most.
    let mut by_line = vec![0.0; sub as usize];
    for &(t, w) in &onsets {
        let k = ((t - phase) / step).round() as i64;
        by_line[k.rem_euclid(sub as i64) as usize] += w;
    }
    let beat_line = by_line.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i as u32).unwrap_or(0);
    Some(Grid { phase, step, sub, beat_line })
}

/// The swing positions (0..1 in the beat) of beats that start in [from, to): beats with a hit on
/// the beat and exactly one between it and the next beat, near the middle.
pub fn swing_samples(grid: &Grid, onsets: &[(f64, f64)], from: f64, to: f64) -> Vec<f64> {
    let mut beats: BTreeMap<i64, Vec<f64>> = BTreeMap::new();
    for &(t, _) in onsets {
        let (b, p) = grid.beat_pos(t);
        // A hit just before the beat belongs to it.
        let (b, p) = if p > 0.92 { (b + 1, p - 1.0) } else { (b, p) };
        beats.entry(b).or_default().push(p);
    }
    let mut out = Vec::new();
    for (b, ps) in beats {
        let start = grid.beat_start() + b as f64 * grid.beat();
        if start < from || start >= to {
            continue;
        }
        let on = ps.iter().any(|&p| p.abs() < 0.1);
        let mid: Vec<f64> = ps.iter().copied().filter(|&p| (0.42..0.78).contains(&p)).collect();
        let other = ps.iter().any(|&p| (0.1..0.42).contains(&p) || (0.78..0.92).contains(&p));
        if on && mid.len() == 1 && !other {
            out.push(mid[0]);
        }
    }
    out
}

/// Sequential state, carried between chunks.
struct Runs {
    /// Per drum: length of the current fast run.
    roll: [u16; 128],
    fill_hits: u16,
    fill_toms: u128,
    fill_last: f64,
    fill_counted: bool,
    /// Keys: the chord being gathered (start time, minute, notes as a 128-bit set).
    group: Option<(f64, i64, u128)>,
    /// Keys: the last single note (note, time), for intervals.
    prev_single: Option<(u8, f64)>,
    /// Keys: start times of recent onsets, for speed.
    onsets: VecDeque<f64>,
}

impl Default for Runs {
    fn default() -> Self {
        Runs {
            roll: [0; 128],
            fill_hits: 0,
            fill_toms: 0,
            fill_last: f64::NEG_INFINITY,
            fill_counted: false,
            group: None,
            prev_single: None,
            onsets: VecDeque::new(),
        }
    }
}

pub struct Analyzer {
    drums: bool,
    roles: [Role; 128],
    groups: [u8; 128],
    buf: VecDeque<AHit>,
    /// Index in `buf` of the first hit not analysed yet.
    done: usize,
    last_t: f64,
    prev_period: Option<f64>,
    runs: Runs,
    /// Per minute: (time, BPM) of its chunks, for the drift.
    drift: BTreeMap<i64, Vec<(f64, f64)>>,
}

impl Analyzer {
    pub fn new(kit: &Kit) -> Analyzer {
        let mut a = Analyzer {
            drums: false,
            roles: [Role::Other; 128],
            groups: std::array::from_fn(|i| i as u8),
            buf: VecDeque::new(),
            done: 0,
            last_t: f64::NEG_INFINITY,
            prev_period: None,
            runs: Runs::default(),
            drift: BTreeMap::new(),
        };
        a.set_kit(kit);
        a
    }

    pub fn set_kit(&mut self, kit: &Kit) {
        self.drums = kit.is_drums();
        self.roles = kit.roles();
        self.groups = kit.groups();
    }

    /// Adds a hit (in time order). A long gap, or time going backwards (the host restarted
    /// processing), first finishes the stretch before it.
    pub fn push(&mut self, hit: AHit, out: &mut BTreeMap<i64, MinuteCounts>) {
        if !self.buf.is_empty() && (hit.t < self.last_t - 0.001 || hit.t - self.last_t > GAP) {
            self.flush(out);
        }
        self.last_t = hit.t;
        self.buf.push_back(hit);
    }

    /// Analyses the hits that have enough context after them.
    pub fn process(&mut self, out: &mut BTreeMap<i64, MinuteCounts>) {
        self.run_until(self.last_t - CONTEXT, out);
    }

    /// Whether there are hits waiting to be analysed.
    pub fn pending(&self) -> bool {
        self.done < self.buf.len()
    }

    /// Analyses everything left and starts afresh (end of playing, profile switch, shutdown).
    pub fn flush(&mut self, out: &mut BTreeMap<i64, MinuteCounts>) {
        self.run_until(f64::INFINITY, out);
        if let Some((_, minute, notes)) = self.runs.group.take() {
            self.end_group(minute, notes, out);
        }
        self.finish_drift(i64::MAX, out);
        self.buf.clear();
        self.done = 0;
        self.prev_period = None;
        self.runs = Runs::default();
        self.last_t = f64::NEG_INFINITY;
    }

    fn run_until(&mut self, limit: f64, out: &mut BTreeMap<i64, MinuteCounts>) {
        while self.done < self.buf.len() && self.buf[self.done].t <= limit {
            let c0 = self.buf[self.done].t;
            let c1 = c0 + CHUNK;
            let end = (self.done..self.buf.len()).find(|&i| self.buf[i].t >= c1 || self.buf[i].t > limit).unwrap_or(self.buf.len());

            let window: Vec<(f64, f64)> = self
                .buf
                .iter()
                .filter(|h| h.t >= c0 - CONTEXT && h.t < c1 + CONTEXT)
                .map(|h| (h.t, weight(self.roles[h.note as usize], h.vel)))
                .collect();
            let grid = fit_grid(&window, self.prev_period);
            self.prev_period = grid.map(|g| g.beat()).or(self.prev_period);
            let minute = self.buf[self.done].minute;
            self.finish_drift(minute, out);

            if let Some(g) = grid {
                let bpm = g.bpm().round().clamp(1.0, 999.0) as u16;
                for i in self.done..end {
                    let h = self.buf[i];
                    let d = &mut out.entry(h.minute).or_default().detail;
                    *d.tempo.entry(bpm).or_default() += 1;
                    let e = g.offset(h.t);
                    if e.abs() <= 0.4 * g.step {
                        let tenths = (e * 1e4).round() as i64;
                        d.timing = (d.timing.0 + 1, d.timing.1 + tenths.unsigned_abs());
                        let o = d.offsets.entry(self.groups[h.note as usize]).or_default();
                        *o = (o.0 + 1, o.1 + tenths);
                    }
                }
                let onsets = merge_onsets(&window);
                let samples = swing_samples(&g, &onsets, c0, self.buf[end - 1].t.max(c0) + 1e-9);
                if !samples.is_empty() {
                    let d = &mut out.entry(minute).or_default().detail;
                    let sum: f64 = samples.iter().map(|p| p * 1000.0).sum();
                    d.swing = (d.swing.0 + samples.len() as u32, d.swing.1 + sum.round() as u64);
                }
                let mid = (c0 + self.buf[end - 1].t) / 2.0;
                self.drift.entry(minute).or_default().push((mid, g.bpm()));
            }

            for i in self.done..end {
                self.sequential(i, out);
            }
            self.done = end;
        }
        // Keep only the context the next chunk needs.
        let keep_from = self.buf.get(self.done).map(|h| h.t).unwrap_or(self.last_t) - CONTEXT - 0.5;
        while self.done > 0 && self.buf.front().is_some_and(|h| h.t < keep_from) {
            self.buf.pop_front();
            self.done -= 1;
        }
    }

    /// Rolls, fills, speed, chords and intervals, hit by hit.
    fn sequential(&mut self, i: usize, out: &mut BTreeMap<i64, MinuteCounts>) {
        let h = self.buf[i];
        let role = self.roles[h.note as usize];
        let drum = self.groups[h.note as usize];
        if self.drums {
            // The same drum's previous hits, newest first.
            let prev: Vec<f64> = (0..i)
                .rev()
                .map(|j| self.buf[j])
                .take_while(|p| h.t - p.t < 2.0)
                .filter(|p| self.groups[p.note as usize] == drum)
                .map(|p| p.t)
                .take(SPEED_RUN - 1)
                .collect();
            if prev.len() == SPEED_RUN - 1 {
                let span = h.t - prev[SPEED_RUN - 2];
                if span > 0.0 {
                    record_speed(out, h.minute, drum, (SPEED_RUN - 1) as f64 / span);
                }
            }
            if matches!(role, Role::Snare | Role::Tom) {
                let r = &mut self.runs.roll[drum as usize];
                *r = if prev.first().is_some_and(|&p| h.t - p <= ROLL_GAP) { r.saturating_add(1) } else { 1 };
                if *r == ROLL_MIN {
                    out.entry(h.minute).or_default().detail.rolls += 1;
                }
            }
            if role == Role::Tom {
                let f = &mut self.runs;
                if h.t - f.fill_last > FILL_GAP {
                    f.fill_hits = 0;
                    f.fill_toms = 0;
                    f.fill_counted = false;
                }
                f.fill_hits += 1;
                f.fill_toms |= 1u128 << drum;
                f.fill_last = h.t;
                if f.fill_hits >= FILL_MIN && f.fill_toms.count_ones() >= 2 && !f.fill_counted {
                    f.fill_counted = true;
                    out.entry(h.minute).or_default().detail.fills += 1;
                }
            }
        } else {
            match self.runs.group {
                Some((start, _, ref mut notes)) if h.t - start < CHORD_WINDOW => *notes |= 1u128 << h.note,
                _ => {
                    if let Some((_, minute, notes)) = self.runs.group.take() {
                        self.end_group(minute, notes, out);
                    }
                    self.runs.group = Some((h.t, h.minute, 1u128 << h.note));
                    let on = &mut self.runs.onsets;
                    on.push_back(h.t);
                    if on.len() > SPEED_RUN {
                        on.pop_front();
                    }
                    if on.len() == SPEED_RUN {
                        let span = h.t - on[0];
                        if span > 0.0 && span < 2.0 {
                            record_speed(out, h.minute, h.note, (SPEED_RUN - 1) as f64 / span);
                        }
                    }
                }
            }
        }
    }

    /// A finished group of notes on keys: a chord, or a single note for the intervals.
    fn end_group(&mut self, minute: i64, notes: u128, out: &mut BTreeMap<i64, MinuteCounts>) {
        let start = self.runs.onsets.back().copied().unwrap_or(0.0);
        let mut mask = 0u16;
        for n in 0..128u32 {
            if notes >> n & 1 == 1 {
                mask |= 1 << (n % 12);
            }
        }
        if mask.count_ones() >= 3 {
            *out.entry(minute).or_default().detail.chords.entry(mask).or_default() += 1;
            self.runs.prev_single = None;
        } else if notes.count_ones() == 1 {
            let note = notes.trailing_zeros() as u8;
            if let Some((p, t)) = self.runs.prev_single {
                if start - t <= 1.0 {
                    let d = (note as i32 - p as i32).unsigned_abs().min(MAX_INTERVAL as u32) as usize;
                    out.entry(minute).or_default().detail.intervals[d] += 1;
                }
            }
            self.runs.prev_single = Some((note, start));
        } else {
            self.runs.prev_single = None;
        }
    }

    /// The drift of minutes before `before`: a straight line through their chunks' tempos, if
    /// there are enough of them, over at least 20 seconds, all within 6 % of their median.
    fn finish_drift(&mut self, before: i64, out: &mut BTreeMap<i64, MinuteCounts>) {
        let done: Vec<i64> = self.drift.range(..before).map(|(&m, _)| m).collect();
        for m in done {
            let pts = self.drift.remove(&m).unwrap_or_default();
            if let Some(slope) = drift_of(&pts) {
                let d = &mut out.entry(m).or_default().detail;
                d.drift = (d.drift.0 + 1, d.drift.1 + (slope * 100.0).round() as i64);
            }
        }
    }
}

fn record_speed(out: &mut BTreeMap<i64, MinuteCounts>, minute: i64, note: u8, rate: f64) {
    let r = (rate * 10.0).round().clamp(0.0, u16::MAX as f64) as u16;
    let d = &mut out.entry(minute).or_default().detail;
    if d.speed.is_none_or(|(_, best)| r > best) {
        d.speed = Some((note, r));
    }
}

/// BPM change per minute through (seconds, BPM) points, if they're steady enough to tell.
pub fn drift_of(pts: &[(f64, f64)]) -> Option<f64> {
    if pts.len() < 4 || pts.last()?.0 - pts[0].0 < 20.0 {
        return None;
    }
    let mut bpms: Vec<f64> = pts.iter().map(|p| p.1).collect();
    bpms.sort_by(f64::total_cmp);
    let median = bpms[bpms.len() / 2];
    if pts.iter().any(|p| (p.1 / median - 1.0).abs() > 0.06) {
        return None;
    }
    let n = pts.len() as f64;
    let mt = pts.iter().map(|p| p.0).sum::<f64>() / n;
    let mb = pts.iter().map(|p| p.1).sum::<f64>() / n;
    let num: f64 = pts.iter().map(|p| (p.0 - mt) * (p.1 - mb)).sum();
    let den: f64 = pts.iter().map(|p| (p.0 - mt).powi(2)).sum();
    (den > 0.0).then(|| num / den * 60.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny deterministic random source.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            (self.0 % 10_000) as f64 / 10_000.0
        }
        /// Roughly normal, mean 0, standard deviation `sd`.
        fn jitter(&mut self, sd: f64) -> f64 {
            (self.next() + self.next() + self.next() - 1.5) * 2.0 * sd
        }
    }

    fn drum_kit() -> Kit {
        Kit::parse("note 36 Kick\nnote 38 Snare\nnote 42 Hi-hat\nnote 48 Tom 1\nnote 45 Tom 2\nnote 43 Floor tom\n")
    }

    /// A rock beat: kick on 1 and 3, snare on 2 and 4, hi-hat eighths (the off-beat eighths at
    /// `swing` of the beat), `secs` long.
    fn groove(bpm: f64, swing: f64, jitter_ms: f64, secs: f64, rng: &mut Rng) -> Vec<AHit> {
        let beat = 60.0 / bpm;
        let mut hits = Vec::new();
        let mut b = 0;
        while b as f64 * beat < secs {
            let t = b as f64 * beat;
            let j = jitter_ms / 1000.0;
            hits.push(AHit { t: t + rng.jitter(j), minute: 0, note: if b % 2 == 0 { 36 } else { 38 }, vel: 110 });
            hits.push(AHit { t: t + rng.jitter(j), minute: 0, note: 42, vel: 80 });
            hits.push(AHit { t: t + swing * beat + rng.jitter(j), minute: 0, note: 42, vel: 60 });
            b += 1;
        }
        hits.sort_by(|a, b| a.t.total_cmp(&b.t));
        hits
    }

    fn analyse(kit: &Kit, hits: &[AHit]) -> MinuteCounts {
        let mut a = Analyzer::new(kit);
        let mut out = BTreeMap::new();
        for &h in hits {
            a.push(h, &mut out);
            a.process(&mut out);
        }
        a.flush(&mut out);
        let mut total = MinuteCounts::default();
        for c in out.values() {
            total.merge(c);
        }
        total
    }

    fn mode(m: &BTreeMap<u16, u32>) -> u16 {
        m.iter().max_by_key(|(_, &n)| n).map(|(&b, _)| b).unwrap_or(0)
    }

    #[test]
    fn finds_the_tempo_and_timing_of_a_groove() {
        let mut rng = Rng(7);
        let hits = groove(120.0, 0.5, 6.0, 40.0, &mut rng);
        let c = analyse(&drum_kit(), &hits);
        let d = &c.detail;
        assert_eq!(mode(&d.tempo), 120, "{:?}", d.tempo);
        // Nearly every hit was measured.
        let measured: u32 = d.tempo.values().sum();
        assert!(measured as usize > hits.len() * 9 / 10, "{measured} of {}", hits.len());
        // About 6 ms of jitter: the average distance from the grid is a few ms.
        let avg_ms = d.timing.1 as f64 / d.timing.0 as f64 / 10.0;
        assert!((2.0..8.0).contains(&avg_ms), "{avg_ms}");
        let swing = d.swing.1 as f64 / d.swing.0 as f64 / 10.0;
        assert!((47.0..53.0).contains(&swing), "{swing}");
    }

    #[test]
    fn sixteenth_hats_at_90_are_90() {
        let mut rng = Rng(3);
        let beat = 60.0 / 90.0;
        let mut hits = Vec::new();
        for b in 0..60 {
            let t = b as f64 * beat;
            hits.push(AHit { t, minute: 0, note: if b % 2 == 0 { 36 } else { 38 }, vel: 110 });
            for s in 0..4 {
                hits.push(AHit { t: t + s as f64 * beat / 4.0 + rng.jitter(0.004), minute: 0, note: 42, vel: 70 });
            }
        }
        hits.sort_by(|a, b| a.t.total_cmp(&b.t));
        let c = analyse(&drum_kit(), &hits);
        assert_eq!(mode(&c.detail.tempo), 90, "{:?}", c.detail.tempo);
    }

    #[test]
    fn hears_swing() {
        let mut rng = Rng(11);
        let hits = groove(100.0, 2.0 / 3.0, 4.0, 30.0, &mut rng);
        let c = analyse(&drum_kit(), &hits);
        assert_eq!(mode(&c.detail.tempo), 100, "{:?}", c.detail.tempo);
        let swing = c.detail.swing.1 as f64 / c.detail.swing.0 as f64 / 10.0;
        assert!((63.0..70.0).contains(&swing), "{swing}");
    }

    #[test]
    fn a_laid_back_snare_shows() {
        let mut rng = Rng(5);
        let mut hits = groove(110.0, 0.5, 3.0, 30.0, &mut rng);
        for h in hits.iter_mut().filter(|h| h.note == 38) {
            h.t += 0.008;
        }
        hits.sort_by(|a, b| a.t.total_cmp(&b.t));
        let c = analyse(&drum_kit(), &hits);
        let o = &c.detail.offsets;
        let avg = |n: u8| o[&n].1 as f64 / o[&n].0 as f64 / 10.0;
        assert!(avg(38) - avg(36) > 5.0, "snare {} kick {}", avg(38), avg(36));
    }

    #[test]
    fn noise_has_no_tempo() {
        let mut rng = Rng(9);
        let mut t = 0.0;
        let mut hits = Vec::new();
        while t < 30.0 {
            t += 0.05 + rng.next() * 0.5;
            hits.push(AHit { t, minute: 0, note: 60 + (rng.next() * 12.0) as u8, vel: 80 });
        }
        let c = analyse(&Kit::default(), &hits);
        let measured: u32 = c.detail.tempo.values().sum();
        assert!(measured < hits.len() as u32 / 5, "{measured} of {}", hits.len());
    }

    #[test]
    fn hears_speeding_up() {
        // 100 BPM rising by 6 BPM over the minute.
        let mut hits = Vec::new();
        let mut t = 0.0;
        while t < 60.0 {
            let bpm = 100.0 + 6.0 * t / 60.0;
            for s in 0..2 {
                hits.push(AHit { t: t + s as f64 * 30.0 / bpm, minute: 0, note: if s == 0 { 36 } else { 42 }, vel: 100 });
            }
            t += 60.0 / bpm;
        }
        let c = analyse(&drum_kit(), &hits);
        let (n, sum) = c.detail.drift;
        assert_eq!(n, 1);
        let per_min = sum as f64 / 100.0;
        assert!((3.0..9.0).contains(&per_min), "{per_min}");
    }

    #[test]
    fn rolls_fills_and_speed() {
        let kit = drum_kit();
        let mut hits = Vec::new();
        // A snare roll: 12 hits at 15 a second.
        for i in 0..12 {
            hits.push(AHit { t: i as f64 / 15.0, minute: 0, note: 38, vel: 90 });
        }
        // A fill across three toms, 2 seconds later.
        for (i, n) in [48u8, 48, 45, 45, 43, 43].iter().enumerate() {
            hits.push(AHit { t: 3.0 + i as f64 * 0.15, minute: 1, note: *n, vel: 100 });
        }
        let c = analyse(&kit, &hits);
        assert_eq!(c.detail.rolls, 1);
        assert_eq!(c.detail.fills, 1);
        assert_eq!(c.detail.speed, Some((38, 150)));
    }

    #[test]
    fn chords_and_intervals_on_keys() {
        let mut hits = Vec::new();
        // C major (C4 E4 G4), then A minor, then a melody C D E G.
        for (t, notes) in [(0.0, [60u8, 64, 67]), (0.5, [57, 60, 64])] {
            for (i, n) in notes.iter().enumerate() {
                hits.push(AHit { t: t + i as f64 * 0.01, minute: 0, note: *n, vel: 80 });
            }
        }
        for (i, n) in [60u8, 62, 64, 67].iter().enumerate() {
            hits.push(AHit { t: 1.2 + i as f64 * 0.25, minute: 0, note: *n, vel: 80 });
        }
        let c = analyse(&Kit::default(), &hits);
        let d = &c.detail;
        assert_eq!(d.chords.get(&0b0000_1001_0001), Some(&1)); // C E G
        assert_eq!(d.chords.get(&0b0010_0001_0001), Some(&1)); // C E A
        assert_eq!(d.intervals[2], 2);
        assert_eq!(d.intervals[3], 1);
    }

    #[test]
    fn drift_needs_steady_points() {
        assert_eq!(drift_of(&[(0.0, 100.0), (10.0, 101.0)]), None);
        let pts: Vec<(f64, f64)> = (0..6).map(|i| (i as f64 * 10.0, 100.0 + i as f64 * 0.5)).collect();
        assert!((drift_of(&pts).unwrap() - 3.0).abs() < 1e-9);
        let jumpy = [(0.0, 100.0), (10.0, 200.0), (20.0, 100.0), (30.0, 100.0)];
        assert_eq!(drift_of(&jumpy), None);
    }
}

#[cfg(test)]
mod sweep {
    use super::*;

    /// Kick and snare on the beats, hi-hats in `subdiv` (2 with `swing`), 5 ms of jitter.
    fn beat(bpm: f64, subdiv: usize, swing: f64) -> Vec<AHit> {
        let beat = 60.0 / bpm;
        let mut hits = Vec::new();
        let mut seed = 99u64;
        let mut j = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            ((seed % 1000) as f64 / 1000.0 - 0.5) * 0.01
        };
        for b in 0..(30.0 / beat) as usize {
            let t = b as f64 * beat;
            hits.push(AHit { t: t + j(), minute: 0, note: if b % 2 == 0 { 36 } else { 38 }, vel: 110 });
            for s in 0..subdiv {
                let off = if subdiv == 2 { [0.0, swing][s] } else { s as f64 / subdiv as f64 };
                hits.push(AHit { t: t + off * beat + j(), minute: 0, note: 42, vel: 70 });
            }
        }
        hits.sort_by(|a, b| a.t.total_cmp(&b.t));
        hits
    }

    /// Across tempos and grooves: the tempo is right from 80 to 160 BPM (outside that it may come
    /// out doubled or halved), and nearly every hit gets measured, a few ms from the grid.
    #[test]
    fn tempos_and_grooves() {
        let kit = Kit::parse("note 36 Kick\nnote 38 Snare\nnote 42 Hi-hat\n");
        for bpm in (70..=190).step_by(20) {
            for (subdiv, swing) in [(2, 0.5), (4, 0.25), (1, 0.0), (2, 0.667)] {
                let hits = beat(bpm as f64, subdiv, swing);
                let mut a = Analyzer::new(&kit);
                let mut out = BTreeMap::new();
                for &h in &hits {
                    a.push(h, &mut out);
                    a.process(&mut out);
                }
                a.flush(&mut out);
                let mut all = MinuteCounts::default();
                out.values().for_each(|c| all.merge(c));
                let d = &all.detail;
                let found = d.tempo.iter().max_by_key(|(_, &n)| n).map(|(&b, _)| b as f64).unwrap_or(0.0);
                let what = format!("{bpm} BPM, {subdiv} a beat, swing {swing}: found {found}");
                if (80..=160).contains(&bpm) {
                    assert_eq!(found, bpm as f64, "{what}");
                } else {
                    assert!([0.5, 1.0, 2.0].iter().any(|k| found == bpm as f64 * k), "{what}");
                }
                assert!(d.timing.0 as usize >= hits.len() * 3 / 4, "{what}: {} of {}", d.timing.0, hits.len());
                let avg_ms = d.timing.1 as f64 / d.timing.0 as f64 / 10.0;
                assert!(avg_ms < 4.0, "{what}: {avg_ms} ms");
            }
        }
    }
}
