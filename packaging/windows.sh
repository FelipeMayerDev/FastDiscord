#!/usr/bin/env bash
# Windows build: fastdiscord.exe with the GStreamer runtime DLLs beside it
# and the plugins its pipelines use under lib\gstreamer-1.0 (main.rs points
# GStreamer there), zipped and embedded in a single-file launcher
# (packaging/launcher) that extracts it on first run. Runs in Git Bash on
# the CI runner, with the official GStreamer MSVC runtime + devel installed.
#
# Usage: packaging/windows.sh  →  dist/FastDiscord-windows-x86_64.exe
set -euo pipefail
cd "$(dirname "$0")/.."

GST=$(cygpath -u "${GSTREAMER_1_0_ROOT_MSVC_X86_64:?GStreamer MSVC root not set}")
OUT=dist/FastDiscord
ZIP=dist/FastDiscord-windows-x86_64.zip
EXE=dist/FastDiscord-windows-x86_64.exe

# Required elements: Desktop Duplication capture, WHIP publish, WHEP watch,
# the encoder probe and the webrtcbin internals.
ELEMENTS=(d3d11screencapturesrc d3d11download appsrc appsink queue videorate capsfilter
          videoconvert videotestsrc fakesink h264parse rtph264pay rtph264depay
          openh264enc openh264dec whipsink whepsrc webrtcbin nicesrc nicesink
          dtlssrtpenc dtlssrtpdec srtpenc srtpdec rtpbin rtpsession rtpjitterbuffer
          rtprtxsend rtprtxreceive rtpstorage)
# Probed at runtime; absent ones just shorten the encoder/decoder ladders.
OPTIONAL=(nvh264enc mfh264enc x264enc d3d11h264dec)

echo "── release build"
cargo build --release

echo "── layout"
rm -rf "$OUT" "$ZIP" "$EXE"
mkdir -p "$OUT/lib/gstreamer-1.0"
cp target/release/fastdiscord.exe "$OUT/"
# GStreamer's own libraries (glib, gstreamer, libnice, openssl…): next to
# the exe, where Windows looks for the plugins' dependencies too.
cp "$GST"/bin/*.dll "$OUT/"

echo "── DJ helpers"
TOOLS=.cache/windows-dj
mkdir -p "$TOOLS" "$OUT/tools" "$OUT/licenses"
# Pin executable downloads and verify cached files too.
# shortcut: BtbN retains monthly builds for two years; refresh this pin before 2028-09.
download() {
    local url=$1 file=$2 checksum=$3
    if [ -f "$file" ] && printf '%s  %s\n' "$checksum" "$file" | sha256sum -c --status; then
        return
    fi
    curl -fLsS --retry 3 -o "$file" "$url"
    printf '%s  %s\n' "$checksum" "$file" | sha256sum -c
}
download https://github.com/yt-dlp/yt-dlp/releases/download/2026.08.19/yt-dlp.exe \
    "$TOOLS/yt-dlp.exe" 66674953fe251b89f4d08c5f0e35e0728679bd67ab3d7d05c0562af101dd3e7a
download https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-09-30-13-08/ffmpeg-n9.0.2-17-g2a571b6068-win64-lgpl-shared-9.0.zip \
    "$TOOLS/ffmpeg.zip" 7157177b8a6cb2174c1650ba8c71b363f2c78cba5330f88c4c02cf5b2b880646
download https://github.com/denoland/deno/releases/download/v2.9.7/deno-x86_64-pc-windows-msvc.zip \
    "$TOOLS/deno.zip" a0c3101b4158d1dfb7d6a78a7bf0f3de80c96bb423c152beec8beb22786f2238
rm -rf "$TOOLS/ffmpeg"
7z x -y "-o$TOOLS/ffmpeg" "$TOOLS/ffmpeg.zip" >/dev/null
FFMPEG="$TOOLS/ffmpeg/ffmpeg-n9.0.2-17-g2a571b6068-win64-lgpl-shared-9.0"
# Keep FFmpeg DLLs away from GStreamer's DLLs, and omit the unused ffplay.
cp "$FFMPEG"/bin/*.dll "$FFMPEG/bin/ffmpeg.exe" "$FFMPEG/bin/ffprobe.exe" "$OUT/tools/"
cp "$TOOLS/yt-dlp.exe" "$OUT/tools/"
7z x -y "-o$OUT/tools" "$TOOLS/deno.zip" >/dev/null
cp "$FFMPEG/LICENSE.txt" "$OUT/licenses/ffmpeg-LICENSE.txt"
for document in LICENSE THIRD_PARTY_LICENSES.txt; do
    curl -fLsS --retry 3 "https://raw.githubusercontent.com/yt-dlp/yt-dlp/2026.08.19/$document" \
        -o "$OUT/licenses/yt-dlp-$document"
done
curl -fLsS --retry 3 https://raw.githubusercontent.com/denoland/deno/v2.9.7/LICENSE.md \
    -o "$OUT/licenses/deno-LICENSE.md"
cp packaging/windows-dj.md "$OUT/licenses/DJ-SOURCES.md"
# Fail the Windows build here if any bundled helper or its DLLs cannot run.
"$OUT/tools/yt-dlp.exe" --ignore-config --version
"$OUT/tools/ffmpeg.exe" -version
"$OUT/tools/ffprobe.exe" -version
"$OUT/tools/deno.exe" --version

declare -A SEEN
copy_plugin() {
    local so
    so=$(gst-inspect-1.0 "$1" 2>/dev/null | tr -d '\r' | grep -m1 -oP '^\s+Filename\s+\K.+' || true)
    [ -n "$so" ] || return 1
    so=$(cygpath -u "$so")
    [ -n "${SEEN[$so]:-}" ] && return 0
    SEEN[$so]=1
    cp "$so" "$OUT/lib/gstreamer-1.0/"
}
for el in "${ELEMENTS[@]}"; do
    copy_plugin "$el" || { echo "missing plugin for element: $el"; exit 1; }
done
for el in "${OPTIONAL[@]}"; do
    copy_plugin "$el" || echo "   optional, absent: $el"
done
echo "   $(ls "$OUT/lib/gstreamer-1.0" | wc -l) plugins, $(ls "$OUT"/*.dll | wc -l) libraries"

echo "── zip"
(cd dist && 7z a -tzip -mx=7 "$(basename "$ZIP")" FastDiscord >/dev/null)

echo "── single exe"
# Version plus the zip's hash: each build extracts to its own folder.
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
FASTDISCORD_BUILD="$VERSION-$(sha256sum "$ZIP" | cut -c1-8)" \
FASTDISCORD_ZIP=$(cygpath -w "$PWD/$ZIP") \
    cargo build --release --manifest-path packaging/launcher/Cargo.toml --target-dir target/launcher
cp target/launcher/release/FastDiscord.exe "$EXE"
rm "$ZIP"
du -h "$EXE"
