#!/bin/bash
# Double-click to run slatefree on macOS: asks for the shoot folder (drag it into this window).
cd "$(dirname "$0")" || exit 1

if [ -n "$1" ]; then
  shoot="$1"
else
  echo "slatefree — drag the shoot folder into this window and press Enter"
  echo "slatefree — перетащите папку съёмки в это окно и нажмите Enter"
  read -r shoot
  # Terminal pastes dragged paths with escaped spaces ("My\ Shoot") or in quotes
  shoot="${shoot%"${shoot##*[![:space:]]}"}"
  shoot="${shoot#\'}"; shoot="${shoot%\'}"
  shoot="${shoot#\"}"; shoot="${shoot%\"}"
  shoot="${shoot//\\ / }"
fi

./slatefree "$shoot"
echo
read -r -p "Press Enter to close / Нажмите Enter, чтобы закрыть окно"
