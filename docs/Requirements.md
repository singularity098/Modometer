# Modometer: Requirements

The owner's requirements (2026-11-05) and the decisions agreed since. This is the source of truth:
record new decisions here.

## Owner's requirements

- VST3 plugin written in Rust. (LV2 was asked for, then dropped for now.)
- Gorgeous dark theme by default. Very dark background, thin classy font, pale pink/purple gradient
  text, borrowing visual styling from Xythos.
- Extremely light on CPU and memory.
- Does not produce sound or modify the sound in any way.
- Only receives MIDI and passes it along unmodified.
- Every note that comes through increases an ongoing counter by one.
- The count is saved and is meant to be ongoing forever.
- Any number of profiles (e.g. "Drums" in one instance, "Keys" in another); each instance tracks to
  its profile.
- Autosave periodically; skip the autosave if no notes arrived since the last save.
- Storage chosen to minimise CPU, RAM and disk writes while keeping counts at one-minute resolution.
- Stats view: counts over a date range as a bar graph, bucketed by minute, hour, day, week, month or
  year; counts per MIDI note (favourite scale, favourite cymbal).
- Main screen: little more than the selected profile and its live counter in a large font with
  thousands separators. The window doesn't need to be large.

## Agreed decisions (owner, 2026-11-05)

- **Only count what the user plays, not MIDI played back.** A plugin can't tell the two apart, so in
  Reaper it goes in the track's Input FX chain, which only sees live input (README, "Counting only
  what you play").
- **Platform and host**: Linux and Reaper for now.
- **Format**: VST3 only; LV2 skipped for now.
- **License**: MIT. Because of this the plugin uses the MIT-licensed `vst3` crate with its own small
  VST3 layer, not nih-plug, whose VST3 bindings are GPLv3.
- **Velocity is tracked** (per minute, all 127 values).
- **Same profile in two instances**: the counts add up.
- **Background**: plain near-black, no image.
- **Data location**: the normal app-data folder (`~/.local/share/modometer`); the owner will
  symlink it into a synced folder.
- **Name**: "Modometer", developer/vendor "Kaia". On disk the bundle is `Modometer.vst3`
  (with `Modometer.so` inside). Renamed from "MIDI Counter" (owner, 2026-11-05): crate `modometer`,
  data folder `modometer`, `MODOMETER_DATA_DIR`. No migration of old data or saved plugin state (the
  owner moves the files). The VST3 class ID is unchanged, so Reaper projects still find the plugin.
- **Branch**: work on `main` and push there.

## Owner's requests and decisions (2026-11-05, after first use)

- **One hit, one count.** Single e-drum strikes counted as 2 to 12+. A strike on an e-drum pad can
  send several note-ons (double triggers, head and rim zones). Decision: a per-profile **retrigger
  guard**: a note-on of a drum less than `retrigger-ms` after that drum's last *counted* hit is not
  counted. Default 30 ms; choices Off, 10, 20, 30, 40, 50, 75, 100, 150 ms. It applies to new hits
  only; old logs are not rewritten.
- **Drum map.** Name notes per profile ("Snare", "Crash 2", custom names) with a **Learn** button.
  Notes with the same name (compared without case) are one drum, for the guard and in the stats.
- **Time played** (owner, 2026-11-05): for the range shown, minutes logged and an estimate of time
  actually played, notes per minute of playing (average) and the busiest minute, and the average
  per bucket (hour, day, ...). The estimate counts each logged minute as min(1, hits / pace). The
  pace is learned from the profile's last 90 days: half the median of mid-session minutes (minutes
  whose neighbours both have hits), so normal variation in steady playing counts as full minutes and
  a session's ragged first and last minutes count in part. Fewer than 10 mid-session minutes: no
  estimate, every logged minute counts in full. The pace is cached for an hour per profile. The
  timeline toggles between notes and time.
- **Where the data is**: unchanged (`~/.local/share/modometer`); the README now says `.local` is
  hidden and lists the files.

## Owner's requests and decisions (2026-11-06, more analysis)

- **Tempo, both ways.** The owner plays to an external metronome and rarely sets the DAW's tempo,
  so Modometer logs both the **project tempo** (the host's tempo, only while its transport plays)
  and the **played tempo**, worked out from the notes with no click. Stats show the most common,
  median and average tempo.
- **Timing against an imagined grid** (the owner's favourite): a grid fitted to the played tempo,
  not the DAW's. It measures steadiness: how far notes land from evenly spaced grid lines that
  follow your own tempo. Also per drum (which sit ahead or behind), swing, and tempo drift.
- **From the data already logged, all of these** (everything suggested except the "silly
  comparisons"): streaks and a calendar of days played, a weekday × hour map of when you play,
  sessions, personal bests (minute, day, week) with a "new best" on the counter screen, milestones
  (1,000, 10,000, 50,000, 100,000, 250,000, 500,000, 1,000,000, 2,500,000, ...), velocity over time,
  kit balance (ride vs hi-hat, share per drum, drums hardly played), range and key per session on
  keys, and a review of any range (a "year in review" for a year).
- **From new data, all of these**: per-drum dynamics (ghost notes, accents), top speed, rolls and
  fills, chords and intervals on keys, and the hi-hat pedal's position at each hi-hat hit.
- **The hi-hat pedal is opt-in** (per profile, Drums view). Reading a CC means mapping it to a
  parameter through `IMidiMapping`; a host could keep the CC from reaching the next plugin, which
  would break the hi-hat. To check in Reaper before relying on it.
- **Disk use and number of files**: the owner suggested zipping old data. Decision: months that are
  over (older than last month) are compacted, one line per minute, gzipped, by the plugin itself
  (`YYYY-MM.log.gz`, readable with `zcat`). Gzip rather than zip: one file per month stays one file,
  and it streams. Files stay at one per month per profile (twelve a year), which is no problem.
- **Drums or keys**: a profile with at least one named drum is a drum kit (drum analysis: rolls,
  fills, per-drum dynamics and timing); any other profile gets the keys analysis (chords,
  intervals).

## Design

### Plugin
- One VST3 class is both processor and controller. Stereo audio in/out, copied through untouched;
  one MIDI input bus; no MIDI output bus, so the host passes MIDI on unchanged (CCs, pitch bend and
  all, which re-emitting VST3 events couldn't do faithfully).
- No parameters. The plugin state is just the profile name.
- A note-on with velocity > 0 counts as one hit, unless the retrigger guard drops it. Note-on with
  velocity 0 is a note-off.
- Retrigger guard: on the audio thread, per drum (a note, or all notes sharing a name), compared in
  samples (a sample clock advanced by each block, plus the event's offset). The note → drum table and
  the window are atomics the UI thread updates; no locks on the audio thread.
- Every note-on, counted or not, also goes into a 10-slot lock-free list of recent hits (packed in
  one `AtomicU64` each) for the Drums view's monitor and for Learn.
- The audio thread pushes each counted hit into a lock-free ring buffer: minute, note, velocity,
  its time on the instance's sample clock (µs), the host's tempo while the transport plays, the
  hi-hat pedal's position (if read) and a clock epoch (24 bytes). No allocation, locks or I/O on the
  audio thread. The plugin asks for the host's tempo and transport state
  (`IProcessContextRequirements`).
- Hi-hat pedal (opt-in): `IMidiMapping` maps the profile's pedal CC (`hihat-pedal-cc 4` in
  `settings.txt`) to the plugin's one parameter, "Hi-hat pedal". When the setting changes the plugin
  asks the host to read the mapping again (`restartComponent(kMidiCCAssignmentChanged)`).

### Analysis
On the saver thread, every couple of seconds (`src/analyze.rs`); results go into the same pending
minutes as the counts.
- Hits are analysed in 1 s chunks, each with 3 s of context on both sides, so a hit is analysed
  about 3 s after it's played (all at once when the playing stops: 2.5 s without a note).
- **Played tempo**: every pair of hits up to 2 s apart votes for its interval (Gaussian, 8 ms);
  kick and snare count most, hi-hats least, louder notes more. The beat is the best-voted interval
  between 60 and 200 BPM (its double counts half), weighted towards 115 BPM so eighth notes or a
  half-time feel don't win; the previous chunk's tempo gets a 25 % bonus so it doesn't flip.
  Tempos outside about 80–160 may come out doubled or halved, as with any beat tracker.
- **The grid**: sixteenths of that beat (or triplets, 32nds or sextuplets when they fit clearly
  better), fitted by weighted least squares, so its tempo and phase follow the playing. Hits more
  than 0.2 grid steps off on average (RMS) mean no steady rhythm: no tempo or timing for that
  chunk. The beat starts on the grid line where kick and snare land most.
- **Timing**: each hit's distance from the nearest grid line; hits further than 0.4 steps (grace
  notes, flams, 32nds) are left out. Per drum the signed offsets. **Swing**: in beats with a hit
  on the beat and exactly one between 42 % and 78 % (nothing else), its position. **Drift**: a
  straight line through a minute's chunk tempos (at least 4 over 20 s, all within 6 % of their
  median), in BPM per minute.
- **Drums**: rolls (6+ hits of one snare or tom, each within 100 ms), fills (4+ tom hits over 2+
  toms, each within 350 ms), top speed (8 hits of one drum in the shortest time). **Keys**: chords
  (3+ pitch classes within 35 ms), melodic intervals between single notes up to 1 s apart, top
  speed (8 onsets).
- Roles (kick, snare, tom, hi-hat, hi-hat pedal, cymbal) come from drum names ("tom", "ride",
  ...), else General MIDI notes.

### Storage
Append-only text log per profile, one file per UTC month:

```
~/.local/share/modometer/profiles/<profile>/2026-11.log
2026-11-05T15:04Z 212 n36:40,38:22,42:150 v64:12,100:200
```

One line per minute that had hits: UTC minute, total, `n` note:count pairs and `v` velocity:count
pairs. Then optional analysis fields, which older versions skip (times in 0.1 ms, so lines add up
exactly):

| Field | Meaning |
|---|---|
| `t120:30` | project tempo (BPM): hits |
| `b118:200` | played tempo (BPM): hits |
| `j190:15200` | timing: hits measured, sum of distances from the grid |
| `o38:40:1400` | per drum (lowest note): hits measured, sum of signed offsets |
| `s12:6800` | swing: off-beats, sum of positions (0.1 %) |
| `d1:-35` | drift: minutes, sum of BPM change a minute (0.01 BPM) |
| `w38:0.4.9.20` | per drum: hits per velocity bin of 16 (trailing zeros dropped) |
| `f38:142` | top speed: drum, hits a second × 10 |
| `r2` `l1` | rolls, fills |
| `p0:3,7:40` | hi-hat hits per pedal bin of 16 (0 = up) |
| `c91:3` | chords: pitch-class mask (hex, bit 0 = C): count |
| `i2:14` | melodic intervals (semitones, up to 24): count |

The analysis of a minute can finish after the minute's hits were saved; then it's a line with a
total of 0, which readers add like any other.

Why this over SQLite or JSON:
- **Disk writes**: an autosave appends only the minutes since the last save, typically a few hundred
  bytes. SQLite writes whole pages plus a journal; JSON would rewrite the whole file.
- **Size**: a few hundred bytes per played minute (the analysis adds roughly a quarter). Three hours
  a day is roughly 10–30 MB a year in the plain logs of the current and last month; compacted months
  shrink to about a sixth.
- **Compaction**: the saver compacts months older than last month when an instance starts and
  every 6 hours: the month's lines (and any earlier `.gz`) are merged, one line per minute, in
  order, and written to `YYYY-MM.log.gz` (temp file + rename), then the plain log is deleted. It's
  skipped if the log changed while being read. Nothing appends to such a month, but if a line ever
  is, it goes to a new plain log next to the `.gz` and readers read both until the next compaction.
  Older versions of the plugin don't read `.gz` months.
- **Robustness**: appends never touch old data; a torn last line (crash, power loss) is skipped, and
  the next save starts on a fresh line.
- **Several instances or processes on one profile**: each save is one `O_APPEND` write of whole lines,
  so writers never interleave. A minute written by two saves appears twice and readers add them up.
  Each instance picks up the others' saves when it saves.
- **Readable**: the files can be opened, searched and backed up by hand.
- **RAM**: only unsaved minutes and the running totals are kept. Month files are read when the stats
  view needs them, only the months in the selected range.
- `settings.txt` holds the profile's settings: `retrigger-ms <ms>` and `note <n> <name>` lines. It's
written atomically (temp file + rename) when changed in the UI; each instance re-reads it when its
modification time changes (checked at autosave), so instances on one profile agree.

`totals.cache` keeps each month's total and the byte length it covers (and for compacted months
  the `.gz` size, as `YYYY-MM gz <size> <total>`, which older versions skip), written when an
  instance closes. On load, a month whose log hasn't grown is taken from the cache; one that grew is read from
  where the cache stopped. The last two months are always read, to count today's notes.

Autosave: every 30 s if anything is new, when the track is deactivated, when switching profiles, and
when the plugin is unloaded. Worst case on a crash: 30 s of notes.

Times are stored in UTC and shown in local time, so DST changes and travel stay correct.

### UI
- baseview + femtovg (OpenGL). Laid out at 800 × 450 and scaled to the window: the view is
  resizable (owner, 2026-11-05). `checkSizeConstraint` keeps the 16:9 shape between 0.75× and 3×
  (times the host's content scale), following whichever side the user dragged; `onSize` resizes the
  window to whatever the host gives, and the layout is centred with even margins if the shape
  differs. The zoom is saved in the instance's state (`zoom 1.500` after the profile name; older
  versions ignore it).
- Mouse cursor: set explicitly when the window opens (on X11 a window without its own cursor shows
  its parent's, so it kept e.g. the host's resize arrow when entered from the side). A hand over
  anything clickable, an I-beam over text fields, the arrow elsewhere.
- Draws only on change: a 30 Hz timer compares one atomic hit counter; mouse moves redraw only when
  the thing under the mouse changes. Measured idle cost with the window open: about 0.35% of a core.
- Font: Jost (variable; 200 for the counter, 300 elsewhere), from Xythos, SIL OFL.
- Gradient: Xythos's Iris preset, #c688d6 → #a85eea → #7278fe.
- Counter view: profile picker (top left; switch or create profiles), Drums and Stats (top right),
  the count, today's count and the streak (days in a row). Under it, a celebration when today
  passed a milestone, or is the best day yet, or has the busiest minute yet (best day and minute
  only after 7 days played); a short sparkle when one happens with the window open. The records are
  worked out by the saver once a day.
- Drums view: the drum map (Learn, rename, remove; click a recent hit to name its note), the recent
  hits monitor (note, name, velocity, gap since the drum's last note, ignored or not), and the
  retrigger guard setting, and the hi-hat pedal switch. Naming dialog: text field, preset names, Save/Remove/Cancel; hitting
  another pad while it's open switches the note.
- Stats view: pages Overview, Rhythm, Kit (or Keys), Habits and Review; span (hour, day, week,
  month, year, all) with previous/next. **Overview**: bucket size, a timeline with hover values
  (notes, time, average velocity, median played tempo, or timing per bucket), notes by pitch class, by note (with your drum names, else GM drum names, and a
  likely-scale guess) or by named drum, and a velocity histogram. Under the range: time played
  (hover: minutes logged and the pace), notes a minute and the busiest minute (hover: when), and the
  average per bucket over the buckets started so far (dropped if the line doesn't fit). The
  timeline's tooltips give notes, time and the busiest minute.
  **Rhythm**: tempo (played or project): most common, median, average, histogram; timing (average
  distance from the grid and a word for it), swing, drift, and per drum ahead or behind.
  **Kit**: per drum share, velocity histogram, ghost notes (velocity < 48) and accents (≥ 112),
  "hardly played" under 1 %; top speed, rolls, fills; ride : hi-hat and shares by part of the kit;
  hi-hat pedal (open 0–47, half 48–79, closed 80–127). **Keys**: top chords, range across the
  keyboard, intervals, and the likely scale of the last sessions.
  **Habits**: streak, longest streak, days played; a calendar of the 53 weeks up to the range's end
  (click a day to see it); when you play (weekday × hour); sessions (a gap of 10 minutes ends one);
  personal bests (all time). **Review**: tiles for the range: notes, time, days played, longest
  streak, busiest day, prime time, favourite drum or key, tempo, timing, swing, top speed, rolls and
  fills or favourite chord, sessions, busiest minute.

## Open items

- Test in Reaper on the owner's machine: loading, Input FX, MIDI pass-through to an instrument after
  it, typing a profile name (keyboard focus in the embedded window), HiDPI.
- Renaming and deleting profiles from the UI (for now: rename or delete the folder).
- Check the retrigger guard with the owner's kit: what one strike sends (Drums view, Recent hits),
  and whether 30 ms suits.
- In Reaper: does Input FX get the project tempo and transport state? With the hi-hat pedal on,
  does CC 4 still reach the drum plugin after Modometer?
- The played-tempo and timing settings are tuned on made-up grooves; check them against real
  playing (tempo octave, how strict the 0.2-step "steady rhythm" threshold is).
- LV2, other hosts and platforms, if wanted later.
