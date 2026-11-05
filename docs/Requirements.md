# MIDI Counter: Requirements

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
- **Data location**: the normal app-data folder (`~/.local/share/midi-counter`); the owner will
  symlink it into a synced folder.
- **Name**: "MIDI Counter", developer/vendor "Kaia".
- **Branch**: work on `main` and push there.

## Design

### Plugin
- One VST3 class is both processor and controller. Stereo audio in/out, copied through untouched;
  one MIDI input bus; no MIDI output bus, so the host passes MIDI on unchanged (CCs, pitch bend and
  all, which re-emitting VST3 events couldn't do faithfully).
- No parameters. The plugin state is just the profile name.
- A note-on with velocity > 0 counts as one hit. Note-on with velocity 0 is a note-off.
- The audio thread pushes each hit (minute, note, velocity: 8 bytes) into a lock-free ring buffer.
  No allocation, locks or I/O on the audio thread.

### Storage
Append-only text log per profile, one file per UTC month:

```
~/.local/share/midi-counter/profiles/<profile>/2026-11.log
2026-11-05T15:04Z 212 n36:40,38:22,42:150 v64:12,100:200
```

One line per minute that had hits: UTC minute, total, `n` note:count pairs and `v` velocity:count
pairs.

Why this over SQLite or JSON:
- **Disk writes**: an autosave appends only the minutes since the last save, typically a few hundred
  bytes. SQLite writes whole pages plus a journal; JSON would rewrite the whole file.
- **Size**: a few hundred bytes per played minute. Three hours a day is roughly 10–30 MB a year,
  depending on how many different notes and velocities you use.
- **Robustness**: appends never touch old data; a torn last line (crash, power loss) is skipped, and
  the next save starts on a fresh line.
- **Several instances or processes on one profile**: each save is one `O_APPEND` write of whole lines,
  so writers never interleave. A minute written by two saves appears twice and readers add them up.
  Each instance picks up the others' saves when it saves.
- **Readable**: the files can be opened, searched and backed up by hand.
- **RAM**: only unsaved minutes and the running totals are kept. Month files are read when the stats
  view needs them, only the months in the selected range.
- `totals.cache` keeps each month's total and the byte length it covers, written when an instance
  closes. On load, a month whose log hasn't grown is taken from the cache; one that grew is read from
  where the cache stopped. The last two months are always read, to count today's notes.

Autosave: every 30 s if anything is new, when the track is deactivated, when switching profiles, and
when the plugin is unloaded. Worst case on a crash: 30 s of notes.

Times are stored in UTC and shown in local time, so DST changes and travel stay correct.

### UI
- baseview + femtovg (OpenGL), 800 × 450, scaled by the host's content scale factor.
- Draws only on change: a 30 Hz timer compares one atomic hit counter; mouse moves redraw only when
  the thing under the mouse changes. Measured idle cost with the window open: about 0.35% of a core.
- Font: Jost (variable; 200 for the counter, 300 elsewhere), from Xythos, SIL OFL.
- Gradient: Xythos's Iris preset, #c688d6 → #a85eea → #7278fe.
- Counter view: profile picker (top left; switch or create profiles), Stats (top right), the count,
  and today's count.
- Stats view: span (hour, day, week, month, year, all) with previous/next, bucket size, a bar chart
  with hover values, notes by pitch class or by note (with GM drum names and a likely-scale guess),
  and a velocity histogram.

## Open items

- Test in Reaper on the owner's machine: loading, Input FX, MIDI pass-through to an instrument after
  it, typing a profile name (keyboard focus in the embedded window), HiDPI.
- Renaming and deleting profiles from the UI (for now: rename or delete the folder).
- LV2, other hosts and platforms, if wanted later.
