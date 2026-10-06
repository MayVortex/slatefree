#!/usr/bin/env bash
# Guards a promise made in the README: slatefree never touches the network and only runs ffmpeg.
set -euo pipefail
fail=0

check() { # pattern, files..., message
  local pat="$1" msg="$2"; shift 2
  if grep -nE "$pat" "$@"; then echo "::error::$msg"; fail=1; fi
}

# Rust: no networking APIs
check 'std::net|TcpStream|TcpListener|UdpSocket|ToSocketAddrs' "networking API in Rust code" src_rust/*.rs
# Rust: the only external program is ffmpeg (Command::new appears exactly once, in ffmpeg())
n=$(grep -c 'Command::new(' src_rust/main.rs || true)
if [ "$n" != "1" ] || ! grep -q 'Command::new(ffmpeg_path())' src_rust/main.rs; then
  echo "::error::Rust code launches something other than ffmpeg ($n Command::new calls)"; fail=1
fi

# Python: no networking modules
check '^\s*(import|from)\s+(socket|urllib|http|requests|httpx|aiohttp|ftplib|smtplib|xmlrpc)\b' \
  "networking module imported in Python code" src_python/*.py
# Python: every subprocess call is a list starting with FFMPEG (path to the ffmpeg binary)
if grep -nE 'subprocess\.(run|Popen|call|check_output|check_call|getoutput|getstatusoutput)\(' src_python/*.py \
    | grep -vE 'subprocess\.run\(\[FFMPEG,'; then
  echo "::error::Python code launches something other than ffmpeg"; fail=1
fi

[ "$fail" = 0 ] && echo "OK: no network access; ffmpeg is the only external program."
exit "$fail"
