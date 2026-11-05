#!/usr/bin/env bash
# Builds "Modometer.vst3" in target/bundle. With --install, also copies it to ~/.vst3.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release --lib
name="Modometer"
bundle="target/bundle/$name.vst3"
rm -rf target/bundle
mkdir -p "$bundle/Contents/x86_64-linux"
# The library inside must have the bundle's name.
cp target/release/libmodometer.so "$bundle/Contents/x86_64-linux/$name.so"
echo "Built $bundle"
if [[ "${1:-}" == "--install" ]]; then
    mkdir -p ~/.vst3
    # Also remove the bundle under its old names (MIDI Counter), so hosts don't find the plugin twice.
    rm -rf ~/.vst3/"$name.vst3" ~/.vst3/"MIDI-Counter.vst3" ~/.vst3/"MIDI Counter.vst3"
    cp -r "$bundle" ~/.vst3/
    echo "Installed to ~/.vst3/$name.vst3"
fi
