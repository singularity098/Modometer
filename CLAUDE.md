# CLAUDE.md

Guidance for Claude when working in this repository. Keep it up to date: when a decision,
convention or command changes, update it in the same commit.

## Project

Modometer: a VST3 plugin (Linux, Reaper) in Rust that counts every MIDI note played, per profile,
forever, with stats. Audio profiles count the time an audio signal comes in instead. Vendor
"Kaia". MIT.

- **Requirements and decisions**: `docs/Requirements.md` is the source of truth; record new decisions
  there.

## Git workflow

- **Commit and push directly to `main`.** Don't create feature branches or open pull requests
  unless the owner asks for one.
- Claude Code setups (the web version especially) may create their own working branch for the
  session. Even then, don't use that branch: commit on `main` and push `main`
  (`git push origin HEAD:main`). Don't push the session branch.
- This is a solo project: branches and PRs add very little here and create unwanted overhead.

## Fixed decisions (don't revisit without the owner)

- VST3 only (no LV2 for now), Linux only, Reaper is the test host.
- MIT license, so no GPL dependencies: VST3 goes through the `vst3` crate (MIT/Apache) and our own
  layer in `src/vst3/`, not nih-plug (its VST3 bindings are GPLv3).
- Count only what's played: in Reaper the plugin goes in Input FX. The plugin itself counts every
  note-on with velocity > 0 that passes the per-profile retrigger guard (default 30 ms per drum).
- Two profile types: MIDI (counts notes, as always) and audio (`type audio` in `settings.txt`: logs
  seconds with a signal above -50 dBFS, the counter stops after 4 s of silence; the hold counts).
  Audio profiles store seconds as the log line's total, so everything built on totals works.
- Never modify audio or MIDI: audio is copied through, and there's no MIDI output bus, so the host
  passes MIDI on.
- Storage: append-only per-month text logs, one line per minute (see Requirements). Velocity is kept.
  Months older than last month are compacted to `YYYY-MM.log.gz` (flate2, pure-Rust miniz_oxide).
- Analysis (tempo, timing against a grid fitted to the playing, rolls, chords, ...) runs on the
  saver thread, never the audio thread. The hi-hat pedal (CC 4 via `IMidiMapping`) stays opt-in.
- Look: plain near-black background, Jost, Xythos's Iris gradient (#c688d6, #a85eea, #7278fe).
- Light on CPU: nothing on the audio thread but a ring-buffer push (and, for audio profiles only, one
  peak scan per block); the UI draws only on change.
- The editor resizes to any shape (each side 0.75x-3x of 800 x 450): draw code uses `self.w()`,
  `self.h()`, `self.extra()` and `self.columns()`, never the 800 x 450 constants.

## Layout

```
src/store.rs        Log format (counts + analysis fields), profiles, month files, compaction, totals cache
src/session.rs      Per-instance state: RT ring buffer, audio gate, pending minutes, saver thread (analysis,
                    autosave, compaction, records), profile switching, editor size
src/analyze.rs      Played tempo, grid fit, timing, swing, drift, rolls, fills, speed, chords, intervals
src/stats.rs        Ranges and buckets in local time, histograms, summaries, sessions, history and streaks,
                    scale guess, chord names, GM drum names
src/kit.rs          Per-profile settings.txt: profile type (MIDI or audio), audio threshold, drum map
                    (note -> name), retrigger guard, hi-hat pedal CC, roles
src/ui/             The editor: baseview + femtovg (mod.rs: counter, overview), stats pages (pages.rs),
                    Drums view (kit.rs), colours and helpers (theme.rs)
src/vst3/           VST3 factory and component (mod.rs), editor view embedding (view.rs)
examples/preview.rs     The window on its own with fake data; --stats, --kit, --page <name>, --chart <name>,
                        --profile <name> (Bass is an audio profile), --size WxH, --screenshot <png>
examples/embed_test.rs  Embeds the editor via IPlugView into a parent window, like a host; --resize
tests/vst3_host.rs      Drives the plugin through its VST3 interfaces
scripts/bundle.sh       Builds "Modometer.vst3" (--install copies to ~/.vst3)
scripts/branding.py     Writes the icon and banner SVGs to resources/branding/ (needs fonttools, uharfbuzz);
                        scripts/branding_png.js renders them to PNG
resources/branding/     Icon (Velocity dots M) and README banner, SVG and PNG
resources/fonts/        Jost (OFL)
```

## Commands

- Test: `cargo test`
- Bundle and install: `./scripts/bundle.sh --install`
- CI: `.github/workflows/build.yml` tests and bundles every push on ubuntu-22.04 (glibc 2.35) and
  uploads `Modometer.vst3` as an artifact. Keep it green.
- Screenshots without a display:
  `xvfb-run -a -s "-screen 0 1024x768x24 +extension GLX" cargo run --example preview -- --stats --screenshot out.png`
- `MODOMETER_DATA_DIR` overrides the data folder (tests and tools use it).

New source files start with `// SPDX-License-Identifier: MIT` and `// Copyright (c) 2026 singularity098`.
