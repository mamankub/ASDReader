#!/usr/bin/env bash
# Regenerate the app icons from assets/app-icon/asdreader.svg and the Windows PDF file icon
# from assets/app-icon/asdreader-document.svg. Both are master vectors.
#
# Needs: resvg (brew install resvg / cargo install resvg) and python3 (stdlib only, for the .ico).
# ICNS is written with iconutil on macOS or PNG container entries elsewhere. The outputs are committed, so building and packaging
# never need these tools. After running it, update the sha256 values in ATTRIBUTION.toml, then
# `cargo xtask assets --write`.
#
#   packaging/icons.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIR="$ROOT/assets/app-icon"
SVG="$DIR/asdreader.svg"
ID="io.github.mamankub.asdreader"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

command -v resvg >/dev/null || { echo "error: resvg not found (brew install resvg)" >&2; exit 1; }

# The master is a full-bleed 512 tile (rx=112): right for Windows and Linux. macOS icons follow
# Apple's grid instead: an 824 px body centred on a transparent 1024 canvas.
MAC="$TMP/macos.svg"
{
  echo '<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 1024 1024">'
  echo '<image x="100" y="100" width="824" height="824" xlink:href="'"$SVG"'"/>'
  echo '</svg>'
} >"$MAC"

render() {
  local src="$1" size="$2" out="$3"
  resvg -w "$size" -h "$size" "$src" "$out" </dev/null
}

# 1024 px PNG on Apple's grid (also the runtime Dock icon on macOS, see apps/asdreader/src/main.rs).
render "$MAC" 1024 "$DIR/asdreader-1024.png"

# Linux hicolor theme (full bleed; hicolor/256x256 is also the runtime icon on Windows and Linux).
for s in 16 24 32 48 64 128 256 512; do
  mkdir -p "$DIR/hicolor/${s}x${s}/apps"
  render "$SVG" "$s" "$DIR/hicolor/${s}x${s}/apps/$ID.png"
done
mkdir -p "$DIR/hicolor/scalable/apps"
cp "$SVG" "$DIR/hicolor/scalable/apps/$ID.svg"

# Windows .ico: PNG-compressed entries, 16-256 px. The document icon has its own resource
# in ASDReader.exe; the app icon remains the first/default icon.
for name in asdreader asdreader-document; do
  ICO_PNGS=()
  for s in 16 20 24 32 40 48 64 128 256; do
    render "$DIR/$name.svg" "$s" "$TMP/$name-$s.png"
    ICO_PNGS+=("$TMP/$name-$s.png")
  done
  python3 - "$DIR/$name.ico" "${ICO_PNGS[@]}" <<'PY'
import struct, sys
out, pngs = sys.argv[1], sys.argv[2:]
blobs = [open(p, "rb").read() for p in pngs]
head = struct.pack("<HHH", 0, 1, len(blobs))
entries, data, offset = b"", b"", 6 + 16 * len(blobs)
for b in blobs:
    w, h = struct.unpack(">II", b[16:24])  # IHDR
    entries += struct.pack("<BBBBHHII", w % 256, h % 256, 0, 0, 1, 32, len(b), offset)
    data += b
    offset += len(b)
open(out, "wb").write(head + entries + data)
PY
done

# macOS .icns.
if command -v iconutil >/dev/null; then
  SET="$TMP/asdreader.iconset"
  mkdir -p "$SET"
  for s in 16 32 128 256 512; do
    render "$MAC" "$s" "$SET/icon_${s}x${s}.png"
    render "$MAC" $((s * 2)) "$SET/icon_${s}x${s}@2x.png"
  done
  iconutil -c icns -o "$DIR/asdreader.icns" "$SET"
else
  # Modern ICNS entries contain PNG data; generate the same sizes on Linux/CI too.
  for s in 16 32 64 128 256 512 1024; do
    render "$MAC" "$s" "$TMP/mac-$s.png"
  done
  python3 - "$DIR/asdreader.icns" "$TMP" <<'PY'
import pathlib, struct, sys
out, folder = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
entries = []
for tag, size in [(b'icp4', 16), (b'icp5', 32), (b'icp6', 64),
                  (b'ic07', 128), (b'ic08', 256), (b'ic09', 512), (b'ic10', 1024)]:
    data = (folder / f'mac-{size}.png').read_bytes()
    entries.append(tag + struct.pack('>I', len(data) + 8) + data)
body = b''.join(entries)
out.write_bytes(b'icns' + struct.pack('>I', len(body) + 8) + body)
PY
fi
echo "icons written to $DIR"
