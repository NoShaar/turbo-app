#!/bin/sh
# Cross-compila o Turbo para Windows x64 a partir do Linux.
# Precisa de rustup, o target x86_64-pc-windows-gnu e mingw-w64
# (pacote gcc-mingw-w64-x86-64). O instalador precisa do NSIS (makensis).
set -eu
cd "$(dirname "$0")/.."
rustup target add x86_64-pc-windows-gnu
cargo build --release --features ui --target x86_64-pc-windows-gnu --bin turbo-app
mkdir -p dist
cp target/x86_64-pc-windows-gnu/release/turbo-app.exe dist/turbo-app.exe
if command -v makensis >/dev/null 2>&1; then
  (cd installer && makensis turbo.nsi)
  echo "Instalador: dist/TurboApp-Setup.exe"
else
  echo "makensis não encontrado. O exe está em dist/turbo-app.exe"
fi
