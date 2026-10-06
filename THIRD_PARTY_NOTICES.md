# Third-party notices

## FFmpeg (bundled in release archives)

The Windows release archive (`slatefree-*-windows-x64.zip`) contains an **unmodified** `ffmpeg.exe` build of
[FFmpeg](https://ffmpeg.org), downloaded at release time from <https://www.gyan.dev/ffmpeg/builds/>
(“release essentials” build). slatefree runs it as a separate program to read audio and metadata; it is not linked into
slatefree.

That build is licensed under the **GNU General Public License v3**. Its license text is included in the archive
(`FFMPEG_LICENSE*`), the exact version in `FFMPEG_VERSION.txt`. Source code for FFmpeg is available from
<https://ffmpeg.org/download.html> and from the build provider's page above.
slatefree itself is MIT-licensed; you may replace `ffmpeg.exe` with any other FFmpeg build.
The macOS and Linux archives contain no FFmpeg; slatefree uses the one installed on the system.

## Rust crates (compiled into slatefree.exe)

| Crate | License |
|---|---|
| [rayon](https://crates.io/crates/rayon) | MIT OR Apache-2.0 |
| [realfft](https://crates.io/crates/realfft) / [rustfft](https://crates.io/crates/rustfft) | MIT OR Apache-2.0 |
| [regex](https://crates.io/crates/regex) | MIT OR Apache-2.0 |
| [windows-sys](https://crates.io/crates/windows-sys) | MIT OR Apache-2.0 |

and their dependencies (see `Cargo.lock`), all under permissive licenses.

## Python version (`src_python/`)

Uses [NumPy](https://numpy.org) (BSD-3-Clause), [SciPy](https://scipy.org) (BSD-3-Clause) and
[imageio-ffmpeg](https://github.com/imageio/imageio-ffmpeg) (BSD-2-Clause), installed separately via pip.
