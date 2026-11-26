#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

if [ ! -x .venv/bin/python ]; then
  uv venv --python 3.14 .venv
fi
if ! .venv/bin/python -c 'import platform, sys; sys.exit(not (platform.machine() == "arm64" and platform.system() == "Darwin" and sys.version_info[:2] == (3, 14)))'; then
  printf '%s\n' 'Eyes Mouse requires native arm64 Python 3.14 on macOS.' >&2
  exit 1
fi
uv pip sync --python .venv/bin/python tracker/requirements.lock

model=tracker/face_landmarker.task
hash=64184e229b263107bc2b804c6625db1341ff2bb731874b0bcc2fe6544e0bc9ff
if ! test -f "$model" || ! printf '%s  %s\n' "$hash" "$model" | shasum -a 256 -c --status; then
  temp_model=$(mktemp tracker/.face-landmarker.XXXXXX)
  trap 'rm -f "$temp_model"' EXIT
  curl -fLsS --retry 2 https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/1/face_landmarker.task -o "$temp_model"
  printf '%s  %s\n' "$hash" "$temp_model" | shasum -a 256 -c --status
  mv "$temp_model" "$model"
fi
