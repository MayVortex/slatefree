<div align="center">

# slatefree

**Sync multi-camera event footage to one continuous audio-recorder track — no clapper, no timecode box.**
Drop the shoot folder on `slatefree.exe`, import the result into DaVinci Resolve, every clip is in place.

[![Release](https://img.shields.io/github/v/release/MayVortex/slatefree)](https://github.com/MayVortex/slatefree/releases/latest)
[![CI](https://github.com/MayVortex/slatefree/actions/workflows/ci.yml/badge.svg)](https://github.com/MayVortex/slatefree/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

[Русская версия](README.ru.md)

</div>

---

## Why

At weddings, conferences and corporate events the cameras start and stop all evening — tens or hundreds of short
clips per camera — while a field recorder (Zoom H4n and the like) runs the whole time. Syncing that by hand, or with
waveform tools that compare every clip against every other clip, is slow and fragile with music, crowd noise and gaps.

**slatefree** uses the recorder as the master clock: it finds every camera clip inside the recorder's audio by
correlating the camera's scratch audio, and writes a ready DaVinci Resolve timeline with **one track per camera**
and the recorder at the bottom.

## Highlights

- **Hundreds of clips, one pass.** 161 clips / 90 GB / 5.3 h of recorder audio: about 50 s on a 4-core laptop
  (all cores used; the rest is disk speed).
- **Sub-frame accurate** where the recorder was rolling (typ. ±5 ms, then rounded to the frame grid).
- **Robust to real events:** loud repetitive music, reverberant halls, camera clocks that jump at every power-on,
  recorder files split at 2 GB, recorder clock never set.
- **Gaps handled:** where the recorder was off, clips are aligned to an overlapping clip of another camera by audio;
  otherwise placed by camera timecode and marked.
- **Tells you what to check:** every clip gets a status; weak matches get a marker in Resolve and a line in a CSV report.
- **Free DaVinci Resolve is enough** (no scripting API needed). Nothing is re-encoded; source files are never modified.

## Quick start

1. Download `slatefree-vX.Y.Z-windows-x64.zip` from [Releases](https://github.com/MayVortex/slatefree/releases/latest)
   and unzip anywhere. Keep `ffmpeg.exe` next to `slatefree.exe`.
2. Lay out the shoot like this (folder names are up to you; `AUDIO` is the default for the recorder):

   ```text
   D:\Video\2026-10-04 Wedding\
       ACAM\     camera A clips (.MOV/.MP4/...)
       BCAM\     camera B clips            ← as many camera folders as you like
       AUDIO\    recorder WAV files, as copied from the card (don't rename them)
   ```

3. **Drag the shoot folder onto `slatefree.exe`.** A console window shows progress and a summary.
4. In DaVinci Resolve (project frame rate = the rate printed at the end):
   **File → Import → Timeline →** `<shoot>\SYNC\<shoot>_sync.xml`.
   Leave *“Automatically import source clips into media pool”* on for a fresh project.

You get:

| Track | Content |
|---|---|
| V1, V2, … | one video track per camera folder |
| A1, A2, … | each camera's own audio, linked to its video |
| last tracks | the recorder, one track per channel set (H4n 4CH: `I` built-in mics, `M` inputs) |

## Preparing the shoot

The tool needs only two things: **the recorder runs continuously** and **every camera records its own audio**.
The rest makes it more reliable.

### Recorder (Zoom H4n / H5 / H6, Tascam, …)

- **Start it before the first important moment and don't stop it.** Clips shot while it was off can only be placed
  by camera timecode or by another camera's audio (see *Statuses*).
- It must write **Broadcast WAV (BWF)** with a time stamp (`bext` TimeReference). Zoom H4n does; most current field
  recorders do. The recorder clock does **not** have to be correct.
- Files split by the recorder at 2 GB are joined seamlessly. Several separate recordings are fine too.
- Place it where it hears what the cameras hear (room / PA sound). Lavalier-only or line-only feeds sync worse;
  on an H4n in 4CH mode the built-in X/Y mics (`…I.wav`) are used by default (`--ref-suffix M` switches to the inputs).
- Don't rename the WAV files (the `I`/`M` suffix tells the channel sets apart).
- Copy only the recorder's own files into `AUDIO`. Mixes exported from an editor are skipped automatically
  (`--exclude` for anything else).

### Cameras

- **Keep in-camera audio recording on** (built-in mic is fine). It is the only thing slatefree matches against.
- **Timecode: Free Run (time of day), not Rec Run.** slatefree uses each camera's timecode — or the file's creation
  time if there is no timecode — as a rough clock to know where to search. *Rec Run* timecode only counts recorded
  time, so it doesn't work as a clock. (Canon: *Menu → Time code → Count up → Free run*.)
- Camera clocks don't need to match each other or the recorder, and jumps of a second at power-on are fine.
  Don't change the clock or time zone during the event.
- **One frame rate for all main cameras.** Clips with a different rate (e.g. 50/60p slow motion) are skipped and listed in
  the report — put them on the timeline yourself.

## Results

`<shoot>\SYNC\` contains:

- `<shoot>_sync.xml` — the timeline (FCP 7 XML).
- `sync_result.csv` — one row per clip: timeline timecode, status, correlation, drift inside long clips, notes
  (`;`-separated, opens in Excel).
- `.cache\` — 8 kHz audio used for analysis; makes re-runs fast. Delete when done.

### Statuses (also shown as markers on the clips in Resolve)

| Status | Meaning | Accuracy |
|---|---|---|
| `ok` | found in the recorder audio with a clear match | ±5 ms + frame rounding |
| `check` | found and consistent with neighbouring clips, but the match is weak — have a look | usually as `ok` |
| `camsync` | recorder was off, aligned to an overlapping clip of another camera by audio | frame-accurate between cameras |
| `outside-recorder` | recorder was off and no other camera overlaps — placed by camera timecode | ±0.5 s |
| `no-camera-audio`, `not-found`, `model` | placed by camera timecode | ±0.5 s |

## Limitations

Please read before relying on it for a paid job.

- **Tested on one real event so far:** Canon EOS 5D Mark III (MOV) + a Canon EOS (MP4) + Zoom H4n in 4CH mode,
  23.976 fps, 161 clips over ~6 h, imported into **DaVinci Resolve 21 (free) on Windows 11**. Other cameras,
  recorders and Resolve versions should work but are untested — reports are very welcome.
- **Windows build only.** The source is plain Rust and should build on macOS/Linux, but that is untested.
- **Output is FCP 7 XML for DaVinci Resolve.** Premiere Pro may import it but is untested; there is no FCPX/AAF output.
- **One recorder** per shoot is used as the master clock.
- **Rec Run timecode is not supported** (see *Cameras*).
- **One timeline frame rate;** clips at other rates are skipped. The sequence is written as 1920×1080
  (Resolve conforms it to your project settings).
- Where only one camera was shooting and the recorder was off, there is nothing to sync against — those clips are placed
  by timecode (±0.5 s) and marked.
- Drift between camera and recorder clocks *within* a clip is measured but not corrected: typically under one frame for
  clips up to ~15 min; very long takes (30 min+) may slip by a frame or two towards their end.
- Precision on the timeline is limited to the frame grid (±½ frame).

## Troubleshooting

| Symptom | What to do |
|---|---|
| Resolve log: *“failed to link because the timecode extents do not match”* | Import the XML into a project where the clips are not yet in the media pool, or with the original, unmodified files. Don't transcode/rename files between running slatefree and importing. |
| A whole camera is `unplaced` / *“no reliable match”* | The camera has no audio, or never overlaps with the recorder. Check that in-camera audio was on. |
| Many `check` clips in one section | Usually very quiet or very noisy passages. Look at them; most are right. |
| Clips of one camera consistently off by seconds | The camera timecode was Rec Run or the clock was changed during the event. |
| `Cannot run ffmpeg` | Put `ffmpeg.exe` next to `slatefree.exe` (or have ffmpeg on `PATH`). |

## Command line

```text
slatefree.exe <shoot folder> [options]

  --cams ACAM BCAM     camera folders (default: every folder with video except --audio)
  --audio AUDIO        folder with the recorder WAV files (default AUDIO)
  --exclude MASK ...   ignore these files (e.g. "mix*.wav" "*_proxy*")
  --ref-suffix I       recorder channel set to analyse (H4n 4CH: I = built-in mics, M = inputs)
  --fps 25             timeline frame rate (default: the most common clip rate)
  --out FOLDER         output folder (default <shoot>\SYNC)
  --jobs N             parallel ffmpeg processes for audio extraction (default 6)
  --threads N          compute threads (default: all cores)
  --lang ru|en         message language (default: Windows UI language)
```

## How it works

1. **Metadata** — reads each clip's timecode, creation time and frame rate, and the recorder's BWF time stamps
   (ffmpeg). Recorder files are grouped into takes; 2 GB continuation files are joined end-to-end.
2. **Audio features** — all audio is decoded to 8 kHz mono and turned into 16-band log-energy envelopes (10 ms hop).
   This ignores level, EQ and reverb differences between a camera's built-in mic and the recorder.
3. **Coarse search** — each clip's spectral flux (onsets) is cross-correlated with the *entire* recorder track (FFT).
   Strong, mutually consistent hits give each camera's clock offset.
4. **Fine search** — every clip is searched in a ±4 s window around the position predicted from its nearest
   anchor clips, using the 16-band envelopes (which, unlike onsets alone, are not fooled by a repeating beat).
   Long clips are matched in 30 s chunks and fitted with a line, which also measures clock drift and catches audio
   glitches.
5. **Camera ↔ camera** — clips outside the recorder's coverage are aligned by audio to overlapping clips of other
   cameras, chaining from clips that are already placed.
6. **Timeline** — positions are snapped to the frame grid and written as FCP 7 XML with explicit tracks. Recorder WAVs
   are referenced with in-points that lie strictly inside the files, which is what Resolve needs to link them.

## Building from source

```bash
cargo build --release          # Windows with Rust + MSVC Build Tools
```

Release binaries are built by GitHub Actions ([`.github/workflows/release.yml`](.github/workflows/release.yml)).
To build on Windows without installing a toolchain, see [docs/BUILD.md](docs/BUILD.md) (disposable WSL distro).

The repository has two implementations of the same program:

| Folder | What | Use it for |
|---|---|---|
| [`src_rust/`](src_rust) | the released `slatefree.exe` (parallel, single file) | everyday use |
| [`src_python/`](src_python) | the same algorithm in Python (NumPy/SciPy), byte-identical XML output | experimenting without a compiler: `pip install -r src_python/requirements.txt`, then `python src_python/slatefree.py <shoot folder>` |

## Contributing

Bug reports with your camera / recorder models and the console output are the most useful contribution —
see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE). Release archives include an unmodified FFmpeg build, which is licensed separately (GPL) —
see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
