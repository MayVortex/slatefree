# Changelog

All notable changes to this project are documented here.
The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), versions follow [SemVer](https://semver.org/).

## [1.0.0] - 2026-10-06

First public release.

### Added
- Sync of any number of camera folders to a continuous field-recorder track by audio
  (coarse spectral-flux search over the whole recording, fine 16-band envelope search around the predicted position).
- Long clips matched in 30 s chunks with a line fit (drift measurement, audio-glitch detection).
- Camera ↔ camera alignment for clips shot while the recorder was off.
- Recorder support: BWF TimeReference, 2 GB split files, several takes, H4n 4CH channel sets (`I`/`M`);
  WAV files rendered by editors are ignored.
- DaVinci Resolve timeline as FCP 7 XML with fixed tracks (one per camera + camera audio + recorder),
  status markers on clips; `sync_result.csv` report.
- Clips with a frame rate different from the timeline are skipped and reported.
- Parallel processing on all CPU cores; audio cache for fast re-runs.
- English and Russian console messages (auto-detected from the Windows UI language, `--lang`).
- Python version of the same program (`src_python/`) producing byte-identical XML.

[1.0.0]: https://github.com/MayVortex/slatefree/releases/tag/v1.0.0
