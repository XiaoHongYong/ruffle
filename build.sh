#!/usr/bin/env bash
# Build a local Ruffle desktop package.
#
# macOS:
#   ./build.sh              # cargo --profile dist + Ruffle.app
#   ./build.sh --release    # cargo --release instead of dist
#   ./build.sh --open       # open the app after building
#   ./build.sh --zip        # also create package/Ruffle-macos.tar.gz
#
# Windows (Git Bash / MSYS / Cygwin) — Windows 7 x86 by default:
#   ./build.sh              # cargo --profile dist for i686-win7-windows-msvc
#   ./build.sh --release    # cargo --release instead of dist
#   ./build.sh --open       # run ruffle.exe after building
#   ./build.sh --zip        # also create package/Ruffle-windows-x86_32-win7.zip
#   ./build.sh --x64        # build x86_64-win7-windows-msvc instead
#   ./build.sh --modern     # use *-pc-windows-msvc (Win10+, no build-std)
#
# Win7 targets are Tier 3 and need a nightly toolchain with rust-src
# plus -Zbuild-std. MSVC x86/x64 tools and Windows SDK are required.
#
# On Windows PowerShell, prefer: .\build.ps1
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

PROFILE="dist"
DO_OPEN=0
DO_ZIP=0
WIN_ARCH="x86"
WIN_MODERN=0

for arg in "$@"; do
  case "$arg" in
    --release) PROFILE="release" ;;
    --dist) PROFILE="dist" ;;
    --open) DO_OPEN=1 ;;
    --zip) DO_ZIP=1 ;;
    --x64|--win64) WIN_ARCH="x64" ;;
    --x86|--win32) WIN_ARCH="x86" ;;
    --modern|--win10) WIN_MODERN=1 ;;
    --win7) WIN_MODERN=0 ;;
    -h|--help)
      sed -n '2,22p' "$0"
      exit 0
      ;;
    *)
      echo "Unknown option: $arg" >&2
      echo "Try: ./build.sh --help" >&2
      exit 1
      ;;
  esac
done

UNAME_S="$(uname -s)"

build_macos() {
  local APP="$ROOT/package/Ruffle.app"
  local BIN_SRC="$ROOT/target/$PROFILE/ruffle_desktop"

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
  local LSREGISTER="/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister"
  if [[ -x "$LSREGISTER" ]]; then
    "$LSREGISTER" -f "$APP" >/dev/null || true
  fi

  if [[ "$DO_ZIP" -eq 1 ]]; then
    local ARCHIVE="$ROOT/package/Ruffle-macos.tar.gz"
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
}

ensure_win7_toolchain() {
  if ! command -v rustup >/dev/null 2>&1; then
    echo "rustup is required to install nightly + rust-src for Win7 targets." >&2
    exit 1
  fi

  echo "==> Ensuring nightly toolchain with rust-src"
  rustup toolchain install nightly --profile minimal --component rust-src
}

build_windows() {
  local TARGET=""
  local BUILD_NAME=""
  local USE_BUILD_STD=0

  if [[ "$WIN_MODERN" -eq 1 ]]; then
    if [[ "$WIN_ARCH" == "x64" ]]; then
      TARGET="x86_64-pc-windows-msvc"
      BUILD_NAME="windows-x86_64"
    else
      TARGET="i686-pc-windows-msvc"
      BUILD_NAME="windows-x86_32"
    fi
  else
    USE_BUILD_STD=1
    if [[ "$WIN_ARCH" == "x64" ]]; then
      TARGET="x86_64-win7-windows-msvc"
      BUILD_NAME="windows-x86_64-win7"
    else
      TARGET="i686-win7-windows-msvc"
      BUILD_NAME="windows-x86_32-win7"
    fi
    ensure_win7_toolchain
  fi

  local BIN_SRC="$ROOT/target/$TARGET/$PROFILE/ruffle_desktop.exe"
  local OUT_DIR="$ROOT/package"
  local OUT_EXE="$OUT_DIR/ruffle.exe"

  echo "==> Building ruffle_desktop (profile=$PROFILE target=$TARGET)"
  # Static CRT matches official Windows release builds and avoids VCRUNTIME DLLs.
  export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }-C target-feature=+crt-static"

  local -a BUILD_ARGS=(
    build
    --package ruffle_desktop
    --profile "$PROFILE"
    --target "$TARGET"
  )
  if [[ "$USE_BUILD_STD" -eq 1 ]]; then
    # dist/release use panic=abort; include panic_abort when rebuilding std.
    BUILD_ARGS+=(-Z "build-std=std,panic_abort")
    cargo +nightly "${BUILD_ARGS[@]}"
  else
    cargo "${BUILD_ARGS[@]}"
  fi

  if [[ ! -f "$BIN_SRC" ]]; then
    echo "Missing binary: $BIN_SRC" >&2
    exit 1
  fi

  echo "==> Assembling $OUT_DIR"
  rm -rf "$OUT_DIR"
  mkdir -p "$OUT_DIR"
  cp "$BIN_SRC" "$OUT_EXE"
  cp "$ROOT/README.md" "$OUT_DIR/README.md"
  cp "$ROOT/LICENSE.md" "$OUT_DIR/LICENSE.md"

  if [[ "$DO_ZIP" -eq 1 ]]; then
    local ARCHIVE="$OUT_DIR/Ruffle-${BUILD_NAME}.zip"
    echo "==> Creating $ARCHIVE"
    if command -v 7z >/dev/null 2>&1; then
      (cd "$OUT_DIR" && 7z a "$(basename "$ARCHIVE")" ruffle.exe README.md LICENSE.md >/dev/null)
    elif command -v zip >/dev/null 2>&1; then
      (cd "$OUT_DIR" && zip -q "$(basename "$ARCHIVE")" ruffle.exe README.md LICENSE.md)
    else
      echo "Need 7z or zip to create $ARCHIVE" >&2
      exit 1
    fi
  fi

  echo
  echo "Done: $OUT_EXE"
  echo "Run:  \"$OUT_EXE\" /path/to/file.swf"
  if [[ "$USE_BUILD_STD" -eq 1 ]]; then
    echo "Note: binary targets Windows 7+ ($TARGET)."
  fi

  if [[ "$DO_OPEN" -eq 1 ]]; then
    "$OUT_EXE" &
  fi
}

is_windows_host() {
  case "$UNAME_S" in
    MINGW*|MSYS*|CYGWIN*) return 0 ;;
  esac
  # Native Windows env (rare under bash) or Git Bash edge cases.
  if [[ -n "${WINDIR:-}" || "${OS:-}" == "Windows_NT" ]]; then
    return 0
  fi
  return 1
}

case "$UNAME_S" in
  Darwin)
    build_macos
    ;;
  *)
    if is_windows_host; then
      build_windows
    else
      echo "Unsupported OS '$UNAME_S'. Supported: macOS and Windows (Git Bash/MSYS)." >&2
      echo "On Windows PowerShell, use: .\\build.ps1" >&2
      exit 1
    fi
    ;;
esac
