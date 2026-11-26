#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

build_dir=$(mktemp -d)
trap 'rm -rf "$build_dir"' EXIT
uv pip install --python .venv/bin/python 'pyinstaller==6.22.3'
.venv/bin/python -m PyInstaller --noconfirm --onefile \
  --workpath "$build_dir/work" --specpath "$build_dir" --distpath "$build_dir/dist" \
  --name eyes-mouse-tracker --target-architecture arm64 \
  --collect-binaries mediapipe --add-data "$PWD/tracker/face_landmarker.task:." "$PWD/tracker/track.py"
"$build_dir/dist/eyes-mouse-tracker" --check
mkdir -p src-tauri/binaries
cp "$build_dir/dist/eyes-mouse-tracker" src-tauri/binaries/eyes-mouse-tracker-aarch64-apple-darwin
