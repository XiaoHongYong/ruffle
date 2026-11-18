#!/usr/bin/env bash
# Build a local macOS Ruffle.app package (dist profile by default).
#
# Usage:
#   ./build.sh              # cargo --profile dist + Ruffle.app
#   ./build.sh --release    # cargo --release instead of dist
#   ./build.sh --open       # open the app after building
#   ./build.sh --zip        # also create package/Ruffle-macos.tar.gz
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

PROFILE="dist"
DO_OPEN=0
DO_ZIP=0

for arg in "$@"; do
  case "$arg" in
    --release) PROFILE="release" ;;
    --dist) PROFILE="dist" ;;
    --open) DO_OPEN=1 ;;
    --zip) DO_ZIP=1 ;;
    -h|--help)
      sed -n '2,12p' "$0"
      exit 0
      ;;
    *)
      echo "Unknown option: $arg" >&2
      echo "Try: ./build.sh --help" >&2
      exit 1
      ;;
  esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This script builds macOS Ruffle.app and must be run on macOS." >&2
  exit 1
fi

APP="$ROOT/package/Ruffle.app"
BIN_SRC="$ROOT/target/$PROFILE/ruffle_desktop"

echo "==> Building ruffle_desktop (profile=$PROFILE)"
cargo build --package ruffle_desktop --profile "$PROFILE"

if [[ ! -x "$BIN_SRC" ]]; then
  echo "Missing binary: $BIN_SRC" >&2
  exit 1
fi

echo "==> Assembling $APP"
rm -rf "$ROOT/package"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp -R "$ROOT/desktop/packages/macOS/Contents/"* "$APP/Contents/"
cp "$BIN_SRC" "$APP/Contents/MacOS/ruffle"
chmod +x "$APP/Contents/MacOS/ruffle"

if command -v xcrun >/dev/null 2>&1 \
  && xcrun --find actool >/dev/null 2>&1 \
  && [[ -d "$ROOT/desktop/assets/Assets.xcassets" ]]; then
  echo "==> Compiling asset catalog"
  xcrun actool \
    --compile "$APP/Contents/Resources" \
    "$ROOT/desktop/assets/Assets.xcassets" \
    --minimum-deployment-target 10.0 \
    --platform macosx \
    --warnings --errors --notices \
    --include-all-app-icons
else
  echo "==> Skipping actool (not available); app icon may be missing"
fi

echo "==> Ad-hoc codesign"
codesign --force -s - "$APP" >/dev/null

echo "==> Registering with Launch Services"
LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
if [[ -x "$LSREGISTER" ]]; then
  "$LSREGISTER" -f "$APP" >/dev/null || true
fi

if [[ "$DO_ZIP" -eq 1 ]]; then
  ARCHIVE="$ROOT/package/Ruffle-macos.tar.gz"
  echo "==> Creating $ARCHIVE"
  tar -C "$ROOT/package" -czf "$ARCHIVE" Ruffle.app
fi

echo
echo "Done: $APP"
echo "Run:  open \"$APP\""
echo "  or: \"$APP/Contents/MacOS/ruffle\" /path/to/file.swf"

if [[ "$DO_OPEN" -eq 1 ]]; then
  open "$APP"
fi
