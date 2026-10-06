# Building slatefree

Release binaries are built automatically by GitHub Actions when a `v*` tag is pushed
([`release.yml`](../.github/workflows/release.yml)): Windows (with FFmpeg), macOS universal (Apple Silicon + Intel)
and Linux x64 archives, `SHA256SUMS.txt` and a build provenance attestation are attached to the GitHub release.
You only need a local build to hack on the code.

## Test build without a local toolchain (GitHub Actions)

Every push to any branch runs [`ci.yml`](../.github/workflows/ci.yml): tests, release build, and the built
binaries for Windows, macOS and Linux are kept as run artifacts (`slatefree-Windows`, `slatefree-macOS`,
`slatefree-Linux`) for 14 days. To try a change on your own footage:

```powershell
git switch -c my-change; git commit -am "..."; git push -u origin my-change
gh run watch                                   # wait for CI
gh run download -n slatefree-Windows -D test-build   # slatefree.exe of the latest run on this branch
```

Put `ffmpeg.exe` next to it and run it on a shoot you have already checked in Resolve (see *Validating a change*).

## Windows, native

Install [Rust](https://rustup.rs) (it will offer to install the Visual Studio C++ Build Tools), then:

```powershell
cargo test
cargo build --release
copy target\release\slatefree.exe .   # put ffmpeg.exe next to it
```

## Windows, without installing a toolchain (disposable WSL distro)

Builds the Windows `.exe` inside a throw-away Debian under WSL 2 and deletes it afterwards — nothing stays installed.
Run in PowerShell from the repository root (adjust the `/mnt/...` path to where the repo is):

```powershell
wsl --install Debian --name slatefree-build --no-launch
wsl -d slatefree-build -u root -- bash -lc "apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq curl ca-certificates build-essential gcc-mingw-w64-x86-64 && curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal && . ~/.cargo/env && rustup target add x86_64-pc-windows-gnu"
wsl -d slatefree-build -u root -- bash -lc ". ~/.cargo/env && cd /mnt/d/src/slatefree && cargo test && CARGO_TARGET_DIR=/root/target cargo build --release --target x86_64-pc-windows-gnu && cp /root/target/x86_64-pc-windows-gnu/release/slatefree.exe ."
wsl --unregister slatefree-build
```

## Linux / macOS

`cargo build --release`. ffmpeg is looked up next to the binary, on `PATH`, then in `/opt/homebrew/bin`,
`/usr/local/bin`, `/usr/bin`.

## Validating a change

Sync results must not change by accident. Before and after your change, run on a shoot you have checked in Resolve:

```powershell
slatefree.exe D:\Video\shoot --out D:\tmp\before
slatefree.exe D:\Video\shoot --out D:\tmp\after
```

and compare the two XML files (clip `<start>` values) — they should be identical unless the change is meant to move clips.
