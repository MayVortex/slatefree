<div align="center">

# slatefree

**Sync multi-camera event footage to one continuous audio-recorder track — no clapper, no timecode box.**
Drop the shoot folder on `slatefree.exe`, import the result into DaVinci Resolve, every clip is in place.
Windows · macOS · Linux

[![Release](https://img.shields.io/github/v/release/MayVortex/slatefree)](https://github.com/MayVortex/slatefree/releases/latest)
[![CI](https://github.com/MayVortex/slatefree/actions/workflows/ci.yml/badge.svg)](https://github.com/MayVortex/slatefree/actions/workflows/ci.yml)
[![CodeQL](https://github.com/MayVortex/slatefree/actions/workflows/codeql.yml/badge.svg)](https://github.com/MayVortex/slatefree/actions/workflows/codeql.yml)
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

## Quick start (Windows)

1. Download `slatefree-vX.Y.Z-windows-x64.zip` from [Releases](https://github.com/MayVortex/slatefree/releases/latest)
   and unzip anywhere. Keep `ffmpeg.exe` next to `slatefree.exe`. (macOS and Linux: [see below](#macos).)
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

### macOS

1. Install FFmpeg once: `brew install ffmpeg` (get Homebrew at <https://brew.sh>).
2. Download `slatefree-vX.Y.Z-macos-universal.tar.gz` (Apple Silicon and Intel) and double-click it to unpack.
3. The binary is not signed with an Apple developer certificate, so macOS blocks it at first. Allow it once in
   Terminal (drag the unpacked folder into the window after typing the command):

   ```bash
   xattr -dr com.apple.quarantine ~/Downloads/slatefree-v1.2.0-macos-universal
   ```

4. **Double-click `slatefree.command`**, drag the shoot folder into the Terminal window that opens, press Enter.
   (Or in Terminal: `./slatefree "/Volumes/Work/2026-10-04 Wedding"`.)
5. Import `<shoot>/SYNC/<shoot>_sync.xml` into DaVinci Resolve as above.

### Linux

Install FFmpeg from your distribution (`sudo apt install ffmpeg`, `sudo dnf install ffmpeg`, …), unpack
`slatefree-vX.Y.Z-linux-x64.tar.gz` and run `./slatefree <shoot folder>`.

## Preparing the shoot

The tool needs only two things: **the recorder runs continuously** and **every camera records its own audio**.
The rest makes it more reliable.

### Recorder (Zoom H4n / H5 / H6, Tascam, …)

- **Start it before the first important moment and don't stop it.** Clips shot while it was off can only be placed
  by camera timecode or by another camera's audio (see *Statuses*).
- It must write **Broadcast WAV (BWF)** with a time stamp (`bext` TimeReference). Zoom H4n does; most current field
  recorders do. The recorder clock does **not** have to be correct.
- Files split by the recorder at 2 GB are joined seamlessly. Several separate recordings are fine too.
- Place it where it hears what the cameras hear (room / PA sound). On an H4n in 4CH mode the built-in X/Y mics
  (`…I.wav`) are used for sync by default (`--ref-suffix M` switches to the inputs).
- **Recommended: take a feed from the mixing desk as two separate channels — one channel = the full mix
  (music + mics), the other = the dry microphones only** (which is which doesn't matter). slatefree analyses the sum of both channels, so the mix keeps a sync signal
  even when nobody is talking, while the editor later uses the dry-mics channel alone: clean speech without the background
  music that is so hard to cut around. On an H4n, record this in 4CH mode so the built-in mics keep recording the room
  (`…I.wav`) and the desk lands in `…M.wav` — both end up on the timeline as separate tracks.
- Don't rename the WAV files (the `I`/`M` suffix tells the channel sets apart).
- Copy only the recorder's own files into `AUDIO`. Mixes exported from an editor are skipped automatically
  (`--exclude` for anything else).

### Cameras

- **Keep in-camera audio recording on** (built-in mic is fine). It is the only thing slatefree matches against.
- **Timecode: Free Run (time of day) is best.** slatefree uses each camera's timecode as a rough clock to know where
  to search. Every clip's timecode is checked against the file's creation time: clips whose timecode doesn't behave
  like a clock (*Rec Run*, which only counts recorded time, or a reset) are listed by name — or the whole folder is
  flagged — and placed using the file creation time instead. That still works; Free Run just gives the most reliable
  starting point. (Canon: *Menu → Time code → Count up → Free run*.)
- Camera clocks don't need to match each other or the recorder, and jumps of a second at power-on are fine.
  Don't change the clock or time zone during the event.
- **One frame rate for all main cameras.** Clips with a different rate (e.g. 50/60p slow motion) are skipped and listed in
  the report — put them on the timeline yourself.

## Results

`<shoot>\SYNC\` contains:

- `<shoot>_sync.xml` — the timeline (FCP 7 XML).
- `sync_result.csv` — one row per clip: timecode check (`ok` / `bad` / `none`), timeline timecode, status,
  correlation, drift inside long clips, notes (`;`-separated, opens in Excel).
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

- **Tested on one real event so far:** Canon EOS 5D Mark III (MOV) + Canon EOS R8 (MP4) + Zoom H4n in 4CH mode,
  23.976 fps, 161 clips over ~6 h, imported into **DaVinci Resolve 21 (free) on Windows 11**. Other cameras,
  recorders and Resolve versions should work but are untested — reports are very welcome.
- **macOS and Linux builds** are produced and unit-tested by CI, but have not yet been tried on real footage or with
  DaVinci Resolve for macOS/Linux — please report how it goes.
- **Output is FCP 7 XML for DaVinci Resolve.** Premiere Pro may import it but is untested; there is no FCPX/AAF output.
- **One recorder** per shoot is used as the master clock.
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
| *“ALL clips have an invalid timecode”* / *“clips with an invalid timecode”* | The camera was in Rec Run or its timecode was reset. Sync still works (file creation time is used); switch the camera to Free Run for next time. |
| Clips of one camera consistently off by seconds | The camera clock or time zone was changed during the event. |
| `Cannot run ffmpeg` | Windows: put `ffmpeg.exe` next to `slatefree.exe`. macOS: `brew install ffmpeg`. Linux: install the `ffmpeg` package. |
| macOS: *“cannot be opened because the developer cannot be verified”* | Run the `xattr -dr com.apple.quarantine …` command from the [macOS](#macos) section once. |

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

Release binaries for Windows, macOS (universal) and Linux are built by GitHub Actions
([`.github/workflows/release.yml`](.github/workflows/release.yml)); every push to any branch also uploads test builds.
See [docs/BUILD.md](docs/BUILD.md).

The repository has two implementations of the same program:

| Folder | What | Use it for |
|---|---|---|
| [`src_rust/`](src_rust) | the released `slatefree.exe` (parallel, single file) | everyday use |
| [`src_python/`](src_python) | the same algorithm in Python (NumPy/SciPy), byte-identical XML output | experimenting without a compiler: `pip install -r src_python/requirements.txt`, then `python src_python/slatefree.py <shoot folder>` |

## Security and trust

slatefree reads your footage and writes two files next to it; it **never touches the network** and launches no program
other than **ffmpeg**. You don't have to take that on faith — every push is checked automatically:

| Check | What it does |
|---|---|
| [CodeQL](https://codeql.github.com) (GitHub) | security static analysis of the Rust code, the Python code and the CI workflows |
| [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) | fails on any known vulnerability (RustSec / CVE) in Rust dependencies, non-permissive licenses, crates not from crates.io, and any networking / TLS crate ([`deny.toml`](deny.toml)) |
| [pip-audit](https://github.com/pypa/pip-audit) · [bandit](https://github.com/PyCQA/bandit) | known vulnerabilities in the Python dependencies · security lint of the Python code |
| [clippy](https://github.com/rust-lang/rust-clippy) · [ruff](https://github.com/astral-sh/ruff) | linters, warnings are errors |
| [no-network check](.github/scripts/no_network.sh) | no networking APIs/modules in the code; ffmpeg is the only external program |
| [Dependabot](.github/dependabot.yml) | weekly dependency update proposals |

**Release files are built by GitHub Actions from the tagged source** — nobody uploads binaries by hand. Each release
carries `SHA256SUMS.txt` and a signed [build provenance attestation](https://docs.github.com/actions/security-for-github-actions/using-artifact-attestations);
verify a download with the [GitHub CLI](https://cli.github.com):

```bash
gh attestation verify slatefree-v1.2.0-windows-x64.zip --repo MayVortex/slatefree
```

No tool can *prove* the absence of a backdoor; what these give you is open code, automated scrutiny of every change,
and proof that the binary you run was built from exactly that code.

## Contributing

Bug reports with your camera / recorder models and the console output are the most useful contribution —
see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE). The Windows release archive includes an unmodified FFmpeg build, which is licensed separately (GPL) —
see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
