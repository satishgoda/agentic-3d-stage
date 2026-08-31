#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PIN="1319002982e09970cb50f727e3f299cea78de229"
SUB="$ROOT/third_party/cycles"
PATCH="$ROOT/patches/cycles/0001-cycles-stream.patch"
URL="https://github.com/blender/cycles.git"
mkdir -p "$ROOT/third_party"
if [[ ! -e "$SUB/.git" ]]; then
  echo "cloning blender/cycles (this is large)"
  git clone --filter=blob:none "$URL" "$SUB"
fi
cd "$SUB"
HEAD=$(git rev-parse HEAD)
if [[ "$HEAD" != "$PIN"* ]]; then
  git fetch --filter=blob:none origin "$PIN"
  git checkout --detach "$PIN"
fi
if [[ -f src/app/cycles_stream.cpp ]]; then
  echo "cycles-stream.cpp already present (patch applied)"
else
  git apply --check "$PATCH"
  git apply "$PATCH"
  echo "applied 0001-cycles-stream.patch"
fi
echo "cycles at $(git rev-parse --short HEAD) with cycles-stream"
