# Contributing

Thanks for helping! The most valuable contributions right now are **reports from real shoots** with gear that hasn't
been tested yet.

## Reporting a problem or a success

Open an [issue](https://github.com/MayVortex/slatefree/issues/new/choose) and include:

- camera models and frame rates, timecode mode (Free Run / Rec Run / none);
- recorder model and recording mode (e.g. Zoom H4n, 4CH, 48 kHz/24-bit);
- DaVinci Resolve version (free/Studio) and OS;
- the full console output of `slatefree.exe` (it is safe to paste: it contains file names and timings only);
- if relevant, the rows of `SYNC\sync_result.csv` for the clips that are wrong, and what you expected.

Please don't upload footage to issues. If a short excerpt is needed to reproduce a problem, we'll ask.

## Code

- The Rust program is a single file, `src_rust/main.rs`. `cargo test` runs the unit tests; CI builds on Windows.
- `src_python/slatefree.py` implements the same algorithm. If you change the algorithm, keep both in sync and check
  that their XML outputs are identical on a test shoot.
- Any change to sync results should be validated on a shoot you have already checked by eye in Resolve:
  run with `--out` into a separate folder and compare clip positions frame by frame.
- Keep pull requests focused; describe what you tested it on.
