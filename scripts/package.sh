#!/usr/bin/env bash
# 릴리즈 빌드 후 dist/에 버전명이 붙은 실행 파일을 복사한다.
# (Android dist/OnePlayer-vX.Y.Z-release.apk 관례를 따름)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# workspace 버전을 Cargo.toml에서 읽는다.
VERSION="$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)"/\1/')"

cargo build --release -p oneplayer
mkdir -p dist

# Windows에서는 .exe가 생성되고, 그 외 OS에서는 확장자 없는 바이너리가 생성된다.
if [ -f target/release/OnePlayerWin.exe ]; then
  cp target/release/OnePlayerWin.exe "dist/OnePlayerWin-v${VERSION}.exe"
  echo "Built: dist/OnePlayerWin-v${VERSION}.exe"
else
  cp target/release/OnePlayerWin "dist/OnePlayerWin-v${VERSION}"
  echo "Built: dist/OnePlayerWin-v${VERSION}"
fi
