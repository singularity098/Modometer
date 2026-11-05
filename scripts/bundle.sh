#!/usr/bin/env bash
# Builds "MIDI Counter.vst3" in target/bundle. With --install, also copies it to ~/.vst3.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release --lib
bundle="target/bundle/MIDI Counter.vst3"
rm -rf "$bundle"
mkdir -p "$bundle/Contents/x86_64-linux"
cp target/release/libmidi_counter.so "$bundle/Contents/x86_64-linux/MIDI Counter.so"
echo "Built $bundle"
if [[ "${1:-}" == "--install" ]]; then
    mkdir -p ~/.vst3
    rm -rf ~/.vst3/"MIDI Counter.vst3"
    cp -r "$bundle" ~/.vst3/
    echo "Installed to ~/.vst3/MIDI Counter.vst3"
fi
