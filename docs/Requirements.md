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
- **Name**: "MIDI Counter", developer/vendor "Kaia". On disk the bundle is `MIDI-Counter.vst3`
  (with `MIDI-Counter.so` inside), no space (owner, 2026-11-05).
- **Branch**: work on `main` and push there.

## Owner's requests and decisions (2026-11-05, after first use)

- **One hit, one count.** Single e-drum strikes counted as 2 to 12+. A strike on an e-drum pad can
  send several note-ons (double triggers, head and rim zones). Decision: a per-profile **retrigger
  guard**: a note-on of a drum less than `retrigger-ms` after that drum's last *counted* hit is not
  counted. Default 30 ms; choices Off, 10, 20, 30, 40, 50, 75, 100, 150 ms. It applies to new hits
  only; old logs are not rewritten.
- **Drum map.** Name notes per profile ("Snare", "Crash 2", custom names) with a **Learn** button.
  Notes with the same name (compared without case) are one drum, for the guard and in the stats.
- **Where the data is**: unchanged (`~/.local/share/midi-counter`); the README now says `.local` is
  hidden and lists the files.

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
- `settings.txt` holds the profile's settings: `retrigger-ms <ms>` and `note <n> <name>` lines. It's
written atomically (temp file + rename) when changed in the UI; each instance re-reads it when its
modification time changes (checked at autosave), so instances on one profile agree.

`totals.cache` keeps each month's total and the byte length it covers, written when an instance
  closes. On load, a month whose log hasn't grown is taken from the cache; one that grew is read from
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
  the count, and today's count.
- Drums view: the drum map (Learn, rename, remove; click a recent hit to name its note), the recent
  hits monitor (note, name, velocity, gap since the drum's last note, ignored or not), and the
  retrigger guard setting. Naming dialog: text field, preset names, Save/Remove/Cancel; hitting
  another pad while it's open switches the note.
- Stats view: span (hour, day, week, month, year, all) with previous/next, bucket size, a bar chart
  with hover values, notes by pitch class, by note (with your drum names, else GM drum names, and a
  likely-scale guess) or by named drum, and a velocity histogram.

## Open items

- Test in Reaper on the owner's machine: loading, Input FX, MIDI pass-through to an instrument after
  it, typing a profile name (keyboard focus in the embedded window), HiDPI.
- Renaming and deleting profiles from the UI (for now: rename or delete the folder).
- Check the retrigger guard with the owner's kit: what one strike sends (Drums view, Recent hits),
  and whether 30 ms suits.
- LV2, other hosts and platforms, if wanted later.
