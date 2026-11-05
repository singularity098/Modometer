# MIDI Counter — Requirements

The owner's requirements (as given, 2026-11-05), followed by proposed decisions and open questions.
Once a proposal is agreed, move it to "Agreed decisions".

## Owner's requirements

- VST3 and LV2 plugins, written in Rust.
- Gorgeous dark theme by default. Very dark background, thin classy font, pale pink/purple gradient text, borrowing
  visual styling from Xythos.
- Extremely light on CPU and memory.
- Does not produce sound or modify the sound in any way.
- Only receives MIDI and passes it along unmodified.
- Every note that comes through increases an ongoing counter by one.
- The count is saved to a config file or database and is meant to be ongoing forever.
- Any number of profiles (e.g. "Drums" in one instance, "Keys" in another); each instance tracks to its profile.
- Autosave periodically; skip the autosave if no notes arrived since the last save.
- Storage chosen to minimise CPU, RAM and disk writes while keeping counts at one-minute resolution.
- Stats view: counts over a date range as a bar graph, bucketed by minute, hour, day, week, month or year; counts per
  MIDI note (favourite scale, favourite cymbal).
- Main screen: little more than the selected profile and its live counter in a large font with thousands separators.
  The window doesn't need to be large.

## Proposed decisions

### Plugin stack
- **nih-plug** for VST3 (CLAP comes for free if wanted).
- **LV2**: nih-plug has no LV2 support. Proposal: a thin LV2 wrapper of our own around the same Rust core (LV2 plugin
  with atom MIDI in/out, plus an LV2 UI that embeds the same window via baseview). Built after VST3 works.
- **UI**: baseview + femtovg (a Rust port of NanoVG, which Xythos uses) so the look matches Xythos, and unlike
  Xythos's NanoVG text, femtovg can fill text with a gradient. Redraw only when the count changes, at most ~30 fps.
- **Audio**: stereo audio in/out passed through untouched (in place, no copy), so it can sit on an instrument track;
  MIDI in/out passed through unmodified.

### What counts
- A note-on with velocity > 0 counts as one hit. Note-on with velocity 0 is a note-off and doesn't count.
- Per hit the audio thread does two integer increments (current-minute total, and that note's count). No allocation,
  no locks, no I/O on the audio thread.

### Storage
Append-only text log per profile, one file per month:

```
<data dir>/MIDI Counter/profiles/<profile>/2026-11.log
2026-11-05T15:04Z 212 36:40 38:22 42:150
```

One line per minute that had hits: UTC minute, total, then `note:count` pairs for the notes played.

Why this over SQLite or JSON:
- **Disk writes**: an autosave appends only the minutes since the last save, typically tens of bytes. SQLite writes
  whole 4 KB pages plus journal; JSON would rewrite the whole file.
- **Size**: about 60 bytes per played minute. Three hours of playing every day is ~11 KB/day, ~4 MB/year.
- **Robustness**: appends never touch old data; a torn last line (crash, power loss) is skipped on read.
- **Several instances or DAW processes on one profile**: each save is one `O_APPEND` write of whole lines, so writers
  don't clobber each other. A minute written by two saves appears twice and readers add them up.
- **Readable**: the user can open, grep or back up the files.
- **RAM**: nothing is kept in memory but the unsaved minutes and the running total. Month files are read only when the
  stats view opens, and only the months in the selected range.
- The running total is cached in `profile.json` (with the profile's name and settings), rewritten atomically on save;
  if it's missing or stale it's rebuilt from the logs.

Autosave: every 30 s if there's anything new, and on plugin unload. Worst case on a crash: 30 s of hits lost.

Times are stored in UTC and shown in local time (DST changes and travel stay correct).

### Profiles
- The selected profile is saved in the plugin state (DAW project), so each instance remembers its own.
- New instances default to a profile named "Default".

### Theme (from Xythos)
- Font: Jost Light (SIL OFL, from Xythos `resources/fonts`).
- Gradient: Xythos's Iris preset, #7278fe → #c688d6 → #a85eea (pale indigo, pink, violet).
- Very dark background; panels translucent black with hairline outlines, as in Xythos.
- Default window about 720 × 420, resizable, HiDPI-aware.

## Open questions

1. **Playback double counting**: MIDI played back from recorded clips also reaches the plugin. Count everything, or
   offer "don't count while the DAW transport is playing"? (In Reaper, putting it in the track's Input FX makes it see
   live input only.)
2. **Platforms and hosts**: Linux and Windows like Xythos, and macOS? Which DAWs matter? (Ableton can't load VST3 MIDI
   effects before an instrument; it would go on a separate MIDI track.)
3. **LV2 priority**: VST3 first and LV2 second OK?
4. **License**: MIT like Xythos?
5. **Velocity**: also track a velocity histogram (cheap: 128 more counters per minute line, sparse)?
6. **Same profile in two instances at once**: allow it (counts add up; each window shows its own live hits plus the
   saved total) or prevent it?
7. **Background**: reuse Xythos's background image, or plain near-black?
8. **Data location**: the platform data dir (`~/.local/share`, `%APPDATA%`, `~/Library/Application Support`), with a
   setting to move it (e.g. into a synced folder)?
9. **Name**: "MIDI Counter" for hosts and the window?
