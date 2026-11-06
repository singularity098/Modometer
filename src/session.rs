// SPDX-License-Identifier: MIT
// Copyright (c) 2026 singularity098

//! One plugin instance's counting state.
//!
//! The audio thread only pushes `Hit`s into a lock-free ring ([`Shared::record_hit`]). A saver
//! thread drains the ring into per-minute counts every couple of seconds, appends them to the
//! profile's log every [`AUTOSAVE`] (skipped when nothing new), and picks up lines other instances
//! appended to the same profile, so their counts add up.
//!
//! Before a hit is counted it passes the retrigger guard ([`Shared::note_on`]): another hit of the
//! same drum within the profile's `retrigger_ms` is dropped, so one strike of an e-drum pad that
//! sends a burst of notes counts once. The audio thread also keeps a short list of recent hits,
//! counted or not, for the UI's monitor and Learn.
//!
//! Each hit carries its time on the instance's sample clock, the host's tempo (when its transport
//! plays) and the hi-hat pedal's position (when read). The saver thread feeds them to the
//! [`Analyzer`](crate::analyze::Analyzer), whose results go into the same per-minute counts. It
//! also compacts months that are over, and keeps the records the counter screen celebrates
//! (streak, best day, best minute).

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::analyze::{self, AHit, Analyzer};
use crate::kit::{self, Kit, Role};
use crate::store::{self, MinuteCounts, Month, Store};

pub const AUTOSAVE: Duration = Duration::from_secs(30);
const DRAIN_EVERY: Duration = Duration::from_secs(2);
const RING_CAPACITY: usize = 4096;
/// The editor's size limits, relative to 800 × 450 (times the host's content scale).
pub const MIN_ZOOM: f32 = 0.75;
pub const MAX_ZOOM: f32 = 3.0;
/// How many recent hits the monitor keeps.
pub const RECENT: usize = 10;
/// How often months that are over get compacted.
const COMPACT_EVERY: Duration = Duration::from_secs(6 * 3600);
/// Hi-hat pedal position not known.
pub const NO_PEDAL: u8 = 255;
/// History needed (days played) before a best day or minute is celebrated.
const RECORD_MIN_DAYS: u32 = 7;

#[derive(Clone, Copy, Debug)]
pub struct Hit {
    /// Microseconds on the instance's sample clock.
    pub micros: u64,
    pub minute: u32,
    /// The host's tempo × 10 while its transport plays, else 0.
    pub host_bpm_x10: u16,
    pub note: u8,
    pub vel: u8,
    /// The hi-hat pedal's position (0-127), or [`NO_PEDAL`].
    pub pedal: u8,
    /// Changes when the sample clock restarted.
    pub epoch: u8,
}

/// Audio-thread state: the retrigger guard (in samples since the plugin started) and what the
/// current block knows about the host.
pub struct Guard {
    /// Per drum: when it last counted.
    counted: [u64; 128],
    /// Per drum: when it last sent a note-on (counted or not).
    seen: [u64; 128],
    /// The host's tempo × 10 while its transport plays, else 0. Set per block.
    pub host_bpm_x10: u16,
    /// The hi-hat pedal's position, or [`NO_PEDAL`]. Set as pedal changes arrive.
    pub pedal: u8,
    epoch: u8,
}

const NEVER: u64 = u64::MAX;
/// Recent-hit slots keep the low 31 bits of their sequence number.
const SEQ_MASK: u32 = 0x7FFF_FFFF;

impl Default for Guard {
    fn default() -> Self {
        Guard { counted: [NEVER; 128], seen: [NEVER; 128], host_bpm_x10: 0, pedal: NO_PEDAL, epoch: 0 }
    }
}

impl Guard {
    /// Forgets old hits (the sample clock stopped); the analysis starts afresh too.
    pub fn reset(&mut self) {
        let (epoch, pedal) = (self.epoch.wrapping_add(1), self.pedal);
        *self = Guard::default();
        self.epoch = epoch;
        self.pedal = pedal;
    }
}

/// A note-on as the monitor shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecentHit {
    pub note: u8,
    pub vel: u8,
    /// False if the retrigger guard dropped it.
    pub counted: bool,
    /// Milliseconds since the same drum's previous note-on, if it was recent.
    pub gap_ms: Option<u32>,
}

impl RecentHit {
    fn pack(self, seq: u32) -> u64 {
        let gap = self.gap_ms.map(|g| g.min(0xFFFE)).unwrap_or(0xFFFF) as u64;
        ((seq & SEQ_MASK) as u64) << 33 | gap << 17 | (self.counted as u64) << 16 | (self.vel as u64) << 8 | self.note as u64
    }

    fn unpack(v: u64) -> (u32, RecentHit) {
        let gap = ((v >> 17) & 0xFFFF) as u32;
        let hit = RecentHit {
            note: (v & 127) as u8,
            vel: ((v >> 8) & 127) as u8,
            counted: (v >> 16) & 1 == 1,
            gap_ms: (gap != 0xFFFF).then_some(gap),
        };
        ((v >> 33) as u32, hit)
    }
}

/// Saved and unsaved counts of the current profile.
struct Profile {
    name: String,
    kit: Kit,
    roles: [Role; 128],
    /// When `settings.txt` was last changed, as we last read it.
    kit_mtime: Option<std::time::SystemTime>,
    /// month -> (bytes of its plain log read so far, hits in those bytes)
    months: BTreeMap<Month, (u64, u64)>,
    /// month -> (size of its `.gz`, hits in it)
    gz: BTreeMap<Month, (u64, u64)>,
    saved_total: u64,
    /// Unsaved hits, per UTC minute.
    pending: BTreeMap<i64, MinuteCounts>,
    pending_total: u64,
    /// Saved hits today (local), and per minute today (for today's busiest minute).
    saved_today: u64,
    today_minutes: BTreeMap<i64, u64>,
    today: chrono::NaiveDate,
    analyzer: Analyzer,
    /// The epoch of the last hit, and when the last hit was drained.
    epoch: Option<u8>,
    last_hit: Instant,
    records: Option<Records>,
}

/// What came before today, for the counter screen's celebrations.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Records {
    /// The local day these are for (they cover the days before it).
    pub day: chrono::NaiveDate,
    pub days_played: u32,
    /// Days in a row played, up to yesterday.
    pub streak: u32,
    pub best_day: u64,
    pub best_minute: u64,
}

impl Records {
    /// Records from every saved minute of a profile, for the days before `today`.
    pub fn compute(store: &Store, profile: &str, today: chrono::NaiveDate) -> Records {
        let mut minutes: BTreeMap<i64, u64> = BTreeMap::new();
        for month in store.months(profile) {
            store.scan_month(profile, month, false, |l| *minutes.entry(l.minute).or_default() += l.total);
        }
        let mut days: BTreeMap<chrono::NaiveDate, u64> = BTreeMap::new();
        let mut r = Records { day: today, ..Records::default() };
        for (&m, &n) in &minutes {
            let d = local_date_of_minute(m);
            if d < today && n > 0 {
                *days.entry(d).or_default() += n;
                r.best_minute = r.best_minute.max(n);
            }
        }
        r.days_played = days.len() as u32;
        r.best_day = days.values().copied().max().unwrap_or(0);
        let mut d = today.pred_opt().unwrap_or(today);
        while days.contains_key(&d) {
            r.streak += 1;
            d = d.pred_opt().unwrap_or(d);
        }
        r
    }
}

/// The note counts the counter passes as milestones: 1,000, 10,000, 50,000, 100,000, 250,000,
/// 500,000, 1,000,000, 2,500,000, 5,000,000, 10,000,000 and so on.
pub fn milestones() -> impl Iterator<Item = u64> {
    [1_000u64, 10_000, 50_000].into_iter().chain((0..12).flat_map(|e| {
        let p = 10u64.pow(e);
        [100_000 * p, 250_000 * p, 500_000 * p]
    }))
}

/// Something worth celebrating today, newest kind first.
#[derive(Clone, Debug, PartialEq)]
pub enum Cheer {
    /// The total passed this many notes today.
    Milestone(u64),
    /// Today is the best day yet (notes).
    BestDay,
    /// Today has the busiest minute yet (notes in it).
    BestMinute(u64),
}

pub struct Snapshot {
    pub profile: String,
    pub total: u64,
    pub today: u64,
    /// Days in a row played, counting today if played.
    pub streak: u32,
    pub cheer: Option<Cheer>,
}

pub struct Shared {
    store: Store,
    producer: Mutex<rtrb::Producer<Hit>>,
    consumer: Mutex<rtrb::Consumer<Hit>>,
    /// Hits seen by the audio thread (for the UI to notice changes cheaply).
    hits_seen: AtomicU64,
    /// Hits that didn't fit in the ring; counted without note details.
    overflow: AtomicU32,
    /// The current profile's drum of each note, and its guard window, for the audio thread.
    groups: [AtomicU8; 128],
    retrigger_ms: AtomicU32,
    /// Recent note-ons, written by the audio thread only: `RECENT` slots used round-robin, and
    /// how many were ever written.
    recent: [AtomicU64; RECENT],
    recent_seq: AtomicU32,
    /// The editor's size relative to its default (f32 bits), kept with the instance's state.
    zoom: AtomicU32,
    /// The current profile's hi-hat pedal CC (0 = not read), for the plugin's MIDI mapping.
    pedal_cc: AtomicU8,
    /// Called (on the UI thread) when the pedal CC setting changes, so the plugin can tell the host.
    on_pedal_cc: Mutex<Option<Box<dyn Fn() + Send>>>,
    profile: Mutex<Profile>,
    /// Bumped whenever what the UI shows changes for a reason other than a hit.
    pub generation: AtomicU64,
    stop: Mutex<bool>,
    /// Wakes the saver early (with `wake`).
    kick: AtomicBool,
    wake: Condvar,
    saver: Mutex<Option<JoinHandle<()>>>,
    saver_running: AtomicBool,
}

fn local_date_of_minute(minute: i64) -> chrono::NaiveDate {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(minute * 60, 0)
        .single()
        .map(|t| t.date_naive())
        .unwrap_or_default()
}

fn local_today() -> chrono::NaiveDate {
    chrono::Local::now().date_naive()
}

impl Shared {
    pub fn new(store: Store) -> Arc<Shared> {
        let (producer, consumer) = rtrb::RingBuffer::new(RING_CAPACITY);
        let shared = Arc::new(Shared {
            store,
            producer: Mutex::new(producer),
            consumer: Mutex::new(consumer),
            hits_seen: AtomicU64::new(0),
            overflow: AtomicU32::new(0),
            groups: std::array::from_fn(|i| AtomicU8::new(i as u8)),
            retrigger_ms: AtomicU32::new(kit::DEFAULT_RETRIGGER_MS),
            recent: std::array::from_fn(|_| AtomicU64::new(0)),
            recent_seq: AtomicU32::new(0),
            zoom: AtomicU32::new(1.0f32.to_bits()),
            pedal_cc: AtomicU8::new(0),
            on_pedal_cc: Mutex::new(None),
            profile: Mutex::new(Profile::empty(String::new())),
            generation: AtomicU64::new(0),
            stop: Mutex::new(false),
            kick: AtomicBool::new(false),
            wake: Condvar::new(),
            saver: Mutex::new(None),
            saver_running: AtomicBool::new(false),
        });
        let first = shared.store.list_profiles().into_iter().next();
        let name = match first {
            Some(n) => n,
            None => shared.store.create_profile(store::DEFAULT_PROFILE).unwrap_or_else(|_| store::DEFAULT_PROFILE.into()),
        };
        // A new instance starts on "Default" if it exists, else the first profile.
        let name = if shared.store.profile_dir(store::DEFAULT_PROFILE).is_dir() { store::DEFAULT_PROFILE.to_string() } else { name };
        let p = Profile::load(&shared.store, name);
        shared.apply_kit(&p.kit);
        *shared.profile.lock().unwrap() = p;
        shared
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    // ---- Audio thread ----

    /// A note-on with velocity > 0 at `at` samples (at `rate` Hz): counts it unless the same drum
    /// counted less than the profile's retrigger window ago, and logs it for the monitor.
    /// Real-time safe. Returns whether it counted.
    pub fn note_on(&self, guard: &mut Guard, at: u64, rate: f64, minute: u32, note: u8, vel: u8) -> bool {
        let note = note & 127;
        let drum = self.groups[note as usize].load(Ordering::Relaxed) as usize & 127;
        let rate = if rate > 0.0 { rate } else { 48000.0 };
        let window = (self.retrigger_ms.load(Ordering::Relaxed) as f64 * rate / 1000.0) as u64;
        let since = |t: u64| if t == NEVER || at < t { None } else { Some(at - t) };
        let counted = match since(guard.counted[drum]) {
            Some(d) => d >= window,
            None => true,
        };
        let gap_ms = since(guard.seen[drum]).map(|d| d as f64 * 1000.0 / rate).filter(|&ms| ms < 10_000.0).map(|ms| ms.round() as u32);
        guard.seen[drum] = at;
        if counted {
            guard.counted[drum] = at;
            // 0 means "no timing" (see `record_hit`).
            let micros = (at as f64 * 1e6 / rate) as u64 + 1;
            let (host_bpm_x10, pedal, epoch) = (guard.host_bpm_x10, guard.pedal, guard.epoch);
            self.push_hit(Hit { micros, minute, host_bpm_x10, note, vel, pedal, epoch });
        }
        let seq = self.recent_seq.load(Ordering::Relaxed).wrapping_add(1);
        let slot = &self.recent[seq as usize % RECENT];
        slot.store(RecentHit { note, vel, counted, gap_ms }.pack(seq), Ordering::Relaxed);
        self.recent_seq.store(seq, Ordering::Release);
        counted
    }

    /// Records a note-on without timing (for tests and tools).
    pub fn record_hit(&self, minute: u32, note: u8, vel: u8) {
        self.push_hit(Hit { micros: 0, minute, host_bpm_x10: 0, note, vel, pedal: NO_PEDAL, epoch: 0 });
    }

    /// Records a counted note-on. Real-time safe: no allocation, no blocking (the producer lock is
    /// only ever taken by the audio thread, so `try_lock` always succeeds in practice).
    #[inline]
    pub fn push_hit(&self, hit: Hit) {
        self.hits_seen.fetch_add(1, Ordering::Relaxed);
        let pushed = match self.producer.try_lock() {
            Ok(mut p) => p.push(hit).is_ok(),
            Err(_) => false,
        };
        if !pushed {
            self.overflow.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn hits_seen(&self) -> u64 {
        self.hits_seen.load(Ordering::Relaxed)
    }

    /// How many note-ons the monitor has seen (counted or not); changes on every note-on.
    pub fn recent_seq(&self) -> u32 {
        self.recent_seq.load(Ordering::Acquire)
    }

    /// The most recent note-ons, newest first.
    pub fn recent_hits(&self) -> Vec<RecentHit> {
        let seq = self.recent_seq();
        let mut out = Vec::with_capacity(RECENT);
        for k in 0..RECENT as u32 {
            let want = seq.wrapping_sub(k);
            if want == 0 {
                break;
            }
            let (got, hit) = RecentHit::unpack(self.recent[want as usize % RECENT].load(Ordering::Relaxed));
            if got != want & SEQ_MASK {
                break; // being overwritten, or not written yet
            }
            out.push(hit);
        }
        out
    }

    pub fn zoom(&self) -> f32 {
        f32::from_bits(self.zoom.load(Ordering::Relaxed))
    }

    pub fn set_zoom(&self, zoom: f32) {
        if zoom.is_finite() {
            self.zoom.store(zoom.clamp(MIN_ZOOM, MAX_ZOOM).to_bits(), Ordering::Relaxed);
        }
    }

    fn apply_kit(&self, kit: &Kit) {
        for (slot, g) in self.groups.iter().zip(kit.groups()) {
            slot.store(g, Ordering::Relaxed);
        }
        self.retrigger_ms.store(kit.retrigger_ms, Ordering::Relaxed);
        self.pedal_cc.store(kit.pedal_cc, Ordering::Relaxed);
    }

    /// The MIDI CC of the hi-hat pedal (0 = not read).
    pub fn pedal_cc(&self) -> u8 {
        self.pedal_cc.load(Ordering::Relaxed)
    }

    /// Reads (or stops reading) the hi-hat pedal. Call from the UI thread.
    pub fn set_pedal_cc(&self, cc: u8) {
        let before = self.pedal_cc();
        self.change_kit(|k| k.pedal_cc = cc.min(127));
        if before != self.pedal_cc() {
            if let Some(f) = self.on_pedal_cc.lock().unwrap().as_ref() {
                f();
            }
        }
    }

    /// Sets what to call (on the UI thread) when the pedal CC changes.
    pub fn on_pedal_cc_change(&self, f: Option<Box<dyn Fn() + Send>>) {
        *self.on_pedal_cc.lock().unwrap() = f;
    }

    /// The current profile's drum map and guard setting.
    pub fn kit(&self) -> Kit {
        self.profile.lock().unwrap().kit.clone()
    }

    fn change_kit(&self, f: impl FnOnce(&mut Kit)) {
        let mut p = self.profile.lock().unwrap();
        f(&mut p.kit);
        self.apply_kit(&p.kit);
        p.kit_changed();
        let dir = self.store.profile_dir(&p.name);
        if let Err(e) = p.kit.save(&dir) {
            eprintln!("Modometer: couldn't save settings of {}: {e}", p.name);
        }
        p.kit_mtime = settings_mtime(&dir);
        self.generation.fetch_add(1, Ordering::Relaxed);
    }

    /// Names a note for this profile, or forgets its name with `None`.
    pub fn set_drum_name(&self, note: u8, name: Option<&str>) -> Result<(), String> {
        let name = name.map(kit::clean_drum_name).transpose()?;
        self.change_kit(|k| match name {
            Some(n) => {
                k.names.insert(note & 127, n);
            }
            None => {
                k.names.remove(&(note & 127));
            }
        });
        Ok(())
    }

    pub fn set_retrigger_ms(&self, ms: u32) {
        self.change_kit(|k| k.retrigger_ms = ms.min(1000));
    }

    // ---- Everything else (UI and saver threads) ----

    /// Moves hits from the ring into the pending counts, and queues them for the analysis (which
    /// runs on the saver thread, see [`Self::analyse`]).
    fn drain_into(&self, p: &mut Profile) {
        let mut consumer = self.consumer.lock().unwrap();
        let drums = p.kit.is_drums();
        let groups = self.groups.each_ref().map(|g| g.load(Ordering::Relaxed) & 127);
        while let Ok(hit) = consumer.pop() {
            let minute = hit.minute as i64;
            let c = p.pending.entry(minute).or_default();
            c.add_hit(hit.note, hit.vel);
            let d = &mut c.detail;
            if hit.host_bpm_x10 > 0 {
                *d.host_tempo.entry((hit.host_bpm_x10 + 5) / 10).or_default() += 1;
            }
            let note = (hit.note & 127) as usize;
            if drums {
                d.drum_vels.entry(groups[note]).or_default()[(hit.vel as usize / 16).min(store::BINS - 1)] += 1;
            }
            if hit.pedal != NO_PEDAL && p.roles[note] == Role::HiHat {
                d.pedal[(hit.pedal as usize / 16).min(store::BINS - 1)] += 1;
            }
            p.pending_total += 1;
            if hit.micros > 0 {
                if p.epoch.is_some_and(|e| e != hit.epoch) {
                    p.analyzer.flush(&mut p.pending);
                }
                p.epoch = Some(hit.epoch);
                let a = AHit { t: hit.micros as f64 / 1e6, minute, note: hit.note & 127, vel: hit.vel };
                p.analyzer.push(a, &mut p.pending);
                p.last_hit = Instant::now();
            }
        }
        let lost = self.overflow.swap(0, Ordering::Relaxed);
        if lost > 0 {
            p.pending.entry(store::now_minute()).or_default().total += lost;
            p.pending_total += lost as u64;
        }
    }

    /// Runs the analysis on hits with enough context after them, and on all of them once the
    /// playing stopped. Saver thread only.
    fn analyse(&self, p: &mut Profile) {
        p.analyzer.process(&mut p.pending);
        if p.analyzer.pending() && p.last_hit.elapsed().as_secs_f64() > analyze::GAP {
            p.analyzer.flush(&mut p.pending);
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut p = self.profile.lock().unwrap();
        self.drain_into(&mut p);
        p.roll_day();
        let today = p.today;
        let mut today_pending = 0u64;
        let mut peak = 0u64;
        for (&m, c) in p.pending.iter().filter(|(&m, _)| local_date_of_minute(m) == today) {
            today_pending += c.total as u64;
            peak = peak.max(p.today_minutes.get(&m).copied().unwrap_or(0) + c.total as u64);
        }
        peak = peak.max(p.today_minutes.values().copied().max().unwrap_or(0));
        let total = p.saved_total + p.pending_total;
        let today_total = p.saved_today + today_pending;
        let records = p.records.as_ref().filter(|r| r.day == today);
        let streak = records.map(|r| r.streak).unwrap_or(0) + (today_total > 0) as u32;
        let cheer = milestones()
            .take_while(|&m| m <= total)
            .last()
            .filter(|&m| total - today_total < m)
            .map(Cheer::Milestone)
            .or_else(|| {
                let r = records.filter(|r| r.days_played >= RECORD_MIN_DAYS)?;
                if today_total > r.best_day {
                    Some(Cheer::BestDay)
                } else if peak > r.best_minute {
                    Some(Cheer::BestMinute(peak))
                } else {
                    None
                }
            });
        Snapshot { profile: p.name.clone(), total, today: today_total, streak, cheer }
    }

    pub fn profile_name(&self) -> String {
        self.profile.lock().unwrap().name.clone()
    }

    /// Unsaved minutes, for the stats view to include.
    pub fn pending(&self) -> BTreeMap<i64, MinuteCounts> {
        let mut p = self.profile.lock().unwrap();
        self.drain_into(&mut p);
        p.pending.clone()
    }

    /// Saves now if there's anything unsaved, and picks up other instances' saves.
    pub fn save(&self) {
        let mut p = self.profile.lock().unwrap();
        self.drain_into(&mut p);
        self.save_locked(&mut p);
    }

    /// Finishes the analysis of everything played so far, then saves (for tests and tools; the
    /// saver thread does this on its own a few seconds after the playing stops).
    pub fn flush(&self) {
        let mut p = self.profile.lock().unwrap();
        self.drain_into(&mut p);
        let p = &mut *p;
        p.analyzer.flush(&mut p.pending);
        self.save_locked(p);
    }

    fn save_locked(&self, p: &mut Profile) {
        if !p.pending.is_empty() {
            match self.store.append(&p.name, &p.pending) {
                Ok(_) => {
                    p.pending.clear();
                    p.pending_total = 0;
                }
                Err(e) => eprintln!("Modometer: couldn't save {}: {e}", p.name),
            }
        }
        let before = p.saved_total + p.pending_total;
        p.refresh(&self.store);
        if p.saved_total + p.pending_total != before {
            // Another instance saved hits to this profile.
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
        // Another instance (or the user) changed the profile's settings.
        let dir = self.store.profile_dir(&p.name);
        let mtime = settings_mtime(&dir);
        if mtime != p.kit_mtime {
            p.kit_mtime = mtime;
            p.kit = Kit::load(&dir);
            self.apply_kit(&p.kit);
            p.kit_changed();
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn cache_of(p: &Profile) -> store::Cache {
        store::Cache { logs: p.months.clone(), gz: p.gz.clone() }
    }

    /// Switches this instance to another profile, saving the current one first.
    pub fn select_profile(&self, name: &str) {
        let mut p = self.profile.lock().unwrap();
        if p.name == name {
            return;
        }
        self.drain_into(&mut p);
        let pr = &mut *p;
        pr.analyzer.flush(&mut pr.pending);
        self.save_locked(&mut p);
        let _ = self.store.write_cache(&p.name, &Self::cache_of(&p));
        let name = self.store.create_profile(name).unwrap_or_else(|_| name.to_string());
        *p = Profile::load(&self.store, name);
        self.apply_kit(&p.kit);
        self.generation.fetch_add(1, Ordering::Relaxed);
        drop(p);
        // The saver works out the new profile's records now.
        self.kick.store(true, Ordering::Relaxed);
        self.wake.notify_all();
    }

    pub fn create_profile(&self, name: &str) -> Result<String, String> {
        let name = self.store.create_profile(name)?;
        self.select_profile(&name);
        Ok(name)
    }

    /// Starts the autosave thread.
    pub fn start(self: &Arc<Self>) {
        if self.saver_running.swap(true, Ordering::SeqCst) {
            return;
        }
        *self.stop.lock().unwrap() = false;
        let me = Arc::clone(self);
        let handle = std::thread::Builder::new()
            .name("modometer-save".into())
            .spawn(move || me.saver_loop())
            .ok();
        *self.saver.lock().unwrap() = handle;
    }

    fn saver_loop(&self) {
        let mut last_save = Instant::now();
        let mut last_compact: Option<(String, Instant)> = None;
        loop {
            {
                let stop = self.stop.lock().unwrap();
                let kicked = |stop: &mut bool| !*stop && !self.kick.load(Ordering::Relaxed);
                let (stop, _) = self.wake.wait_timeout_while(stop, DRAIN_EVERY, kicked).unwrap();
                if *stop {
                    break;
                }
                self.kick.store(false, Ordering::Relaxed);
            }
            let name = {
                let mut p = self.profile.lock().unwrap();
                self.drain_into(&mut p);
                self.analyse(&mut p);
                if last_save.elapsed() >= AUTOSAVE {
                    last_save = Instant::now();
                    self.save_locked(&mut p);
                }
                p.name.clone()
            };
            // Slow file work happens without the lock, so the window never waits for it.
            if last_compact.as_ref().is_none_or(|(n, at)| *n != name || at.elapsed() >= COMPACT_EVERY) {
                last_compact = Some((name.clone(), Instant::now()));
                self.compact(&name);
            }
            self.update_records(&name);
        }
    }

    /// Compacts the profile's months before last month.
    fn compact(&self, name: &str) {
        let before = Month::of_minute(store::now_minute()).prev();
        match self.store.compact(name, before) {
            Ok(done) if !done.is_empty() => {
                let mut p = self.profile.lock().unwrap();
                if p.name == name {
                    for (month, total, size) in done {
                        p.months.remove(&month);
                        p.gz.insert(month, (size, total));
                    }
                    let _ = self.store.write_cache(&p.name, &Self::cache_of(&p));
                }
            }
            Ok(_) => {}
            Err(e) => eprintln!("Modometer: couldn't compact {name}: {e}"),
        }
    }

    /// Works out the records if they're missing or from another day.
    fn update_records(&self, name: &str) {
        let today = local_today();
        let current = self.profile.lock().unwrap().records.as_ref().is_some_and(|r| r.day == today);
        if current {
            return;
        }
        let records = Records::compute(&self.store, name, today);
        let mut p = self.profile.lock().unwrap();
        if p.name == name {
            p.records = Some(records);
            self.generation.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Stops the autosave thread and saves everything. Call when the plugin is unloaded.
    pub fn shutdown(&self) {
        *self.stop.lock().unwrap() = true;
        self.wake.notify_all();
        if let Some(h) = self.saver.lock().unwrap().take() {
            let _ = h.join();
        }
        self.saver_running.store(false, Ordering::SeqCst);
        let mut p = self.profile.lock().unwrap();
        self.drain_into(&mut p);
        let pr = &mut *p;
        pr.analyzer.flush(&mut pr.pending);
        self.save_locked(&mut p);
        let _ = self.store.write_cache(&p.name, &Self::cache_of(&p));
    }
}

impl Profile {
    fn empty(name: String) -> Profile {
        let kit = Kit::default();
        Profile {
            name,
            roles: kit.roles(),
            analyzer: Analyzer::new(&kit),
            kit,
            kit_mtime: None,
            months: BTreeMap::new(),
            gz: BTreeMap::new(),
            saved_total: 0,
            pending: BTreeMap::new(),
            pending_total: 0,
            saved_today: 0,
            today_minutes: BTreeMap::new(),
            today: local_today(),
            epoch: None,
            last_hit: Instant::now(),
            records: None,
        }
    }

    fn kit_changed(&mut self) {
        self.roles = self.kit.roles();
        self.analyzer.set_kit(&self.kit);
    }

    /// Counts lines of a plain log from byte `from`: returns (end, hits), adding today's hits.
    fn scan_log(&mut self, path: &std::path::Path, from: u64) -> (u64, u64) {
        let today = self.today;
        let (mut added, mut today_added) = (0, 0);
        let today_minutes = &mut self.today_minutes;
        let end = store::scan_file(path, from, false, |l| {
            added += l.total;
            if local_date_of_minute(l.minute) == today {
                today_added += l.total;
                *today_minutes.entry(l.minute).or_default() += l.total;
            }
        })
        .unwrap_or(from);
        self.saved_today += today_added;
        (end, added)
    }

    /// Loads totals: cached months whose logs haven't changed are taken from the cache, the others
    /// are read (from where the cache left off when the log only grew). This month and last month
    /// are always read, to count today's hits.
    fn load(store: &Store, name: String) -> Profile {
        let mut p = Profile::empty(name);
        let dir = store.profile_dir(&p.name);
        p.kit = Kit::load(&dir);
        p.kit_changed();
        p.kit_mtime = settings_mtime(&dir);
        let cache = store.read_cache(&p.name);
        let recent = Month::of_minute(store::now_minute() - 2 * 1440);
        for month in store.months(&p.name) {
            let gz = store.gz_path(&p.name, month);
            if let Ok(meta) = std::fs::metadata(&gz) {
                let total = match cache.gz.get(&month) {
                    Some(&(size, t)) if size == meta.len() => t,
                    _ => {
                        let mut t = 0;
                        let _ = store::scan_gz(&gz, false, |l| t += l.total);
                        t
                    }
                };
                p.gz.insert(month, (meta.len(), total));
                p.saved_total += total;
            }
            let path = store.month_path(&p.name, month);
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            let len = meta.len();
            let (mut bytes, mut total) = (0, 0);
            if month < recent {
                if let Some(&(b, t)) = cache.logs.get(&month) {
                    if b <= len && ends_line(&path, b) {
                        (bytes, total) = (b, t);
                    }
                }
            }
            if bytes < len {
                let (end, added) = p.scan_log(&path, bytes);
                bytes = end;
                total += added;
            }
            p.months.insert(month, (bytes, total));
            p.saved_total += total;
        }
        p
    }

    /// Reads whatever was appended to recent logs since we last looked (our saves and others').
    fn refresh(&mut self, store: &Store) {
        self.roll_day();
        let now = Month::of_minute(store::now_minute());
        let mut months: Vec<Month> = self.months.keys().copied().filter(|m| m.next() >= now).collect();
        if !months.contains(&now) {
            months.push(now);
        }
        for month in months {
            let path = store.month_path(&self.name, month);
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            let from = self.months.get(&month).map(|e| e.0).unwrap_or(0);
            if meta.len() <= from {
                continue;
            }
            let (end, added) = self.scan_log(&path, from);
            let entry = self.months.entry(month).or_insert((0, 0));
            entry.0 = end;
            entry.1 += added;
            self.saved_total += added;
        }
    }

    fn roll_day(&mut self) {
        let today = local_today();
        if today != self.today {
            self.today = today;
            self.saved_today = 0;
            self.today_minutes.clear();
        }
    }
}

fn settings_mtime(profile_dir: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(profile_dir.join(kit::SETTINGS_FILE)).and_then(|m| m.modified()).ok()
}

fn ends_line(path: &std::path::Path, offset: u64) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    if offset == 0 {
        return true;
    }
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut b = [0u8];
    f.seek(SeekFrom::Start(offset - 1)).is_ok() && f.read_exact(&mut b).is_ok() && b[0] == b'\n'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("modometer-session-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    #[test]
    fn counts_save_and_reload() {
        let store = temp_store("reload");
        let a = Shared::new(store.clone());
        assert_eq!(a.profile_name(), "Default");
        let now = store::now_minute() as u32;
        for i in 0..10 {
            a.record_hit(now, 36 + i, 100);
        }
        assert_eq!(a.snapshot().total, 10);
        assert_eq!(a.snapshot().today, 10);
        a.shutdown();

        let b = Shared::new(store.clone());
        assert_eq!(b.snapshot().total, 10);
        assert_eq!(b.snapshot().today, 10);
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn two_instances_add_up() {
        let store = temp_store("two");
        let a = Shared::new(store.clone());
        let b = Shared::new(store.clone());
        let now = store::now_minute() as u32;
        a.record_hit(now, 40, 80);
        a.record_hit(now, 40, 80);
        b.record_hit(now, 41, 90);
        a.save();
        b.save();
        a.save(); // picks up b's line
        assert_eq!(a.snapshot().total, 3);
        assert_eq!(b.snapshot().total, 3);
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn profiles_are_separate() {
        let store = temp_store("profiles");
        let s = Shared::new(store.clone());
        let now = store::now_minute() as u32;
        s.record_hit(now, 36, 100);
        s.create_profile("Keys").unwrap();
        assert_eq!(s.snapshot().total, 0);
        s.record_hit(now, 60, 50);
        s.record_hit(now, 64, 50);
        s.select_profile("Default");
        assert_eq!(s.snapshot().total, 1);
        s.select_profile("Keys");
        assert_eq!(s.snapshot().total, 2);
        assert_eq!(store.list_profiles(), vec!["Default".to_string(), "Keys".to_string()]);
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn retrigger_guard() {
        let store = temp_store("guard");
        let s = Shared::new(store.clone());
        let mut g = Guard::default();
        let m = store::now_minute() as u32;
        let rate = 1000.0; // one sample per millisecond
        assert!(s.note_on(&mut g, 1000, rate, m, 38, 100));
        assert!(!s.note_on(&mut g, 1010, rate, m, 38, 60)); // a double trigger
        assert!(!s.note_on(&mut g, 1025, rate, m, 38, 30));
        assert!(s.note_on(&mut g, 1012, rate, m, 36, 90)); // another drum
        assert!(s.note_on(&mut g, 1030, rate, m, 38, 100)); // 30 ms after the last count
        assert_eq!(s.snapshot().total, 3);

        let recent = s.recent_hits();
        assert_eq!(recent.len(), 5);
        assert_eq!(recent[0], RecentHit { note: 38, vel: 100, counted: true, gap_ms: Some(5) });
        assert_eq!(recent[2], RecentHit { note: 38, vel: 30, counted: false, gap_ms: Some(15) });
        assert_eq!(recent[4], RecentHit { note: 38, vel: 100, counted: true, gap_ms: None });

        // Notes with the same name are one drum.
        s.set_drum_name(38, Some("Snare")).unwrap();
        s.set_drum_name(40, Some("snare")).unwrap();
        assert!(s.note_on(&mut g, 2000, rate, m, 38, 100));
        assert!(!s.note_on(&mut g, 2005, rate, m, 40, 100));
        // Off.
        s.set_retrigger_ms(0);
        assert!(s.note_on(&mut g, 2006, rate, m, 40, 100));
        assert_eq!(s.snapshot().total, 5);

        // Settings are per profile and saved.
        let other = Shared::new(store.clone());
        assert_eq!(other.kit().name(40), Some("snare"));
        assert_eq!(other.kit().retrigger_ms, 0);
        other.create_profile("Keys").unwrap();
        assert_eq!(other.kit(), Kit::default());
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn hits_are_analysed_and_saved() {
        let store = temp_store("analysis");
        let s = Shared::new(store.clone());
        s.set_drum_name(36, Some("Kick")).unwrap();
        s.set_drum_name(38, Some("Snare")).unwrap();
        s.set_drum_name(42, Some("Hi-hat")).unwrap();
        let mut g = Guard { host_bpm_x10: 1000, pedal: 127, ..Guard::default() };
        let rate = 48000.0;
        let m = store::now_minute() as u32;
        // 20 seconds of a 100 BPM rock beat with eighth-note hi-hats.
        let beat = rate * 0.6;
        for b in 0..34u64 {
            let t = b as f64 * beat;
            s.note_on(&mut g, t as u64, rate, m, if b % 2 == 0 { 36 } else { 38 }, 100);
            s.note_on(&mut g, t as u64 + 1, rate, m, 42, 80);
            s.note_on(&mut g, (t + beat / 2.0) as u64, rate, m, 42, 60);
        }
        s.flush();
        let mut all = MinuteCounts::default();
        for month in store.months("Default") {
            store.scan_month("Default", month, true, |l| all.merge(&l.counts));
        }
        assert_eq!(all.total, 102);
        let d = &all.detail;
        assert_eq!(d.host_tempo.get(&100), Some(&102));
        assert_eq!(d.tempo.iter().max_by_key(|(_, &n)| n).map(|(&b, _)| b), Some(100));
        assert!(d.timing.0 > 80);
        assert_eq!(d.drum_vels[&42][80 / 16] + d.drum_vels[&42][60 / 16], 68);
        assert_eq!(d.pedal[7], 68);
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn records_and_cheers() {
        let store = temp_store("records");
        store.create_profile("Default").unwrap();
        let today = local_today();
        // Played the 9 days before today: 100 notes a day, 150 the day before yesterday.
        let mut minutes = BTreeMap::new();
        for back in 1..=9i64 {
            let day = today - chrono::Duration::days(back);
            let noon = local_minute(day.and_hms_opt(12, 0, 0).unwrap());
            let mut c = MinuteCounts::default();
            for _ in 0..if back == 2 { 150 } else { 100 } {
                c.add_hit(38, 90);
            }
            minutes.insert(noon, c);
        }
        store.append("Default", &minutes).unwrap();
        let r = Records::compute(&store, "Default", today);
        assert_eq!((r.days_played, r.streak, r.best_day, r.best_minute), (9, 9, 150, 150));

        let s = Shared::new(store.clone());
        s.profile.lock().unwrap().records = Some(r);
        let snap = s.snapshot();
        assert_eq!((snap.total, snap.streak, snap.cheer), (950, 9, None));
        let now = store::now_minute() as u32;
        for _ in 0..60 {
            s.record_hit(now, 38, 90);
        }
        // 1,010 notes in all: the 1,000 milestone was passed today.
        assert_eq!(s.snapshot().cheer, Some(Cheer::Milestone(1_000)));
        assert_eq!(s.snapshot().streak, 10);

        // Without a milestone: the best day, then the busiest minute.
        s.create_profile("Keys").unwrap();
        let rec = Records { day: today, days_played: 7, streak: 0, best_day: 50, best_minute: 5 };
        s.profile.lock().unwrap().records = Some(rec.clone());
        for _ in 0..6 {
            s.record_hit(now, 60, 90);
        }
        assert_eq!(s.snapshot().cheer, Some(Cheer::BestMinute(6)));
        s.profile.lock().unwrap().records = Some(Records { best_day: 5, ..rec });
        assert_eq!(s.snapshot().cheer, Some(Cheer::BestDay));
        assert_eq!(milestones().take(8).collect::<Vec<_>>(), vec![1_000, 10_000, 50_000, 100_000, 250_000, 500_000, 1_000_000, 2_500_000]);
        let _ = std::fs::remove_dir_all(store.root());
    }

    fn local_minute(t: chrono::NaiveDateTime) -> i64 {
        use chrono::TimeZone;
        chrono::Local.from_local_datetime(&t).earliest().map(|t| t.timestamp() / 60).unwrap_or(0)
    }

    #[test]
    fn recent_ring_wraps() {
        let store = temp_store("recent");
        let s = Shared::new(store.clone());
        let mut g = Guard::default();
        for i in 0..25u64 {
            s.note_on(&mut g, i * 1000, 1000.0, 0, i as u8, 64);
        }
        let notes: Vec<u8> = s.recent_hits().iter().map(|h| h.note).collect();
        assert_eq!(notes, (15..25).rev().collect::<Vec<u8>>());
        let _ = std::fs::remove_dir_all(store.root());
    }

    #[test]
    fn cache_is_used_and_validated() {
        let store = temp_store("cache");
        let s = Shared::new(store.clone());
        // An old month, written directly.
        let mut minutes = BTreeMap::new();
        let mut c = MinuteCounts::default();
        c.add_hit(36, 100);
        c.add_hit(36, 100);
        minutes.insert(store::days_from_civil(2020, 1, 1) * 1440, c);
        store.append("Default", &minutes).unwrap();
        s.shutdown();
        let s = Shared::new(store.clone());
        assert_eq!(s.snapshot().total, 2);
        s.shutdown(); // writes the cache
        // The log grows behind the cache's back: the extra line must still count.
        store.append("Default", &minutes).unwrap();
        let s = Shared::new(store.clone());
        assert_eq!(s.snapshot().total, 4);
        let _ = std::fs::remove_dir_all(store.root());
    }
}
