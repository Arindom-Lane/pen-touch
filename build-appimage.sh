#!/usr/bin/env bash
# Builds PenTouch-<arch>.AppImage from this source tree.
#
#   ./build-appimage.sh           build (tells you exactly what is missing, if anything)
#   ./build-appimage.sh --setup   first install whatever is missing, then build
#   ./build-appimage.sh --help
#
# What it does, in plain words:
#   1. compiles the program with Rust ("cargo"), using zig as the linker so the result needs only
#      an OLD glibc (2.17). That is what lets one file run on practically every Linux distro;
#   2. copies the program, icon and launcher entry into a folder called AppDir;
#   3. packs AppDir into a single self-mounting file with "appimagetool".
#
# Needs: curl, python3 (with venv), and a C toolchain is NOT needed. Rust itself and the two
# helpers (zig, cargo-zigbuild) are what --setup installs for you.
set -euo pipefail
cd "$(dirname "$0")"

ARCH="${ARCH:-$(uname -m)}"
GLIBC="${GLIBC:-2.17}"
TARGET="${ARCH}-unknown-linux-gnu"
OUT="PenTouch-${ARCH}.AppImage"
TOOLS="$PWD/.build-tools"          # everything this script installs lives here (delete it to undo)
SETUP=0

say()  { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m  %s\n' "$*" >&2; }
die()  { printf '\033[1;31mxx\033[0m  %s\n' "$*" >&2; exit 1; }

case "${1:-}" in
  --setup) SETUP=1 ;;
  -h|--help) sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  "") ;;
  *) die "unknown option '$1' (try --help)" ;;
esac

command -v curl    >/dev/null || die "curl is required (apt install curl / dnf install curl / pacman -S curl)"
command -v python3 >/dev/null || die "python3 is required (apt install python3 python3-venv)"

# ---------------------------------------------------------------- 1. Rust
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
if ! command -v cargo >/dev/null; then
  if [ "$SETUP" = 1 ]; then
    say "installing Rust with rustup (from https://rustup.rs)"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
    . "$HOME/.cargo/env"
  else
    die "Rust is not installed. Run:  ./build-appimage.sh --setup
    (or install it yourself: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh)"
  fi
fi

# ---------------------------------------------------------------- 2. zig (the linker that targets old glibc)
[ -d "$TOOLS/venv/bin" ] && export PATH="$TOOLS/venv/bin:$PATH"
if ! python3 -m ziglang version >/dev/null 2>&1; then
  if [ "$SETUP" = 1 ]; then
    say "installing zig into $TOOLS/venv (a private Python environment; nothing system-wide)"
    python3 -m venv "$TOOLS/venv" || die "python3 -m venv failed (Debian/Ubuntu: apt install python3-venv)"
    "$TOOLS/venv/bin/pip" install --quiet ziglang
    export PATH="$TOOLS/venv/bin:$PATH"
  else
    die "zig is not installed. Run:  ./build-appimage.sh --setup"
  fi
fi

# ---------------------------------------------------------------- 3. cargo-zigbuild
export CARGO_INSTALL_ROOT="${CARGO_INSTALL_ROOT:-$TOOLS/cargo}"
export PATH="$CARGO_INSTALL_ROOT/bin:$PATH"
if ! command -v cargo-zigbuild >/dev/null; then
  if [ "$SETUP" = 1 ]; then
    say "installing cargo-zigbuild (a few minutes the first time)"
    cargo install cargo-zigbuild --locked
  else
    die "cargo-zigbuild is not installed. Run:  ./build-appimage.sh --setup"
  fi
fi

# ---------------------------------------------------------------- 4. build
say "compiling (glibc ${GLIBC} compatible) - the first build takes several minutes, later ones are quick"
cargo zigbuild --release --target "${TARGET}.${GLIBC}"

# ---------------------------------------------------------------- 5. AppDir
say "assembling AppDir"
rm -rf AppDir
mkdir -p AppDir/usr/bin AppDir/usr/share/icons/hicolor/256x256/apps
install -m755 "target/${TARGET}/release/pen-touch"        AppDir/usr/bin/pen-touch
install -m755 packaging/AppRun                            AppDir/AppRun
install -m644 packaging/pen-touch.desktop                 AppDir/pen-touch.desktop
install -m644 packaging/pen-touch-256.png                 AppDir/pen-touch.png
install -m644 packaging/pen-touch-256.png                 AppDir/usr/share/icons/hicolor/256x256/apps/pen-touch.png
ln -sf pen-touch.png AppDir/.DirIcon

# ---------------------------------------------------------------- 6. pack
say "packing $OUT"
TOOL="${APPIMAGETOOL:-}"
if [ -z "$TOOL" ]; then
  if command -v appimagetool >/dev/null; then
    TOOL="$(command -v appimagetool)"
  else
    mkdir -p "$TOOLS"
    if [ ! -x "$TOOLS/appimagetool/AppRun" ]; then
      say "downloading appimagetool"
      curl -fsSL -o "$TOOLS/appimagetool.AppImage" \
        "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-${ARCH}.AppImage"
      chmod +x "$TOOLS/appimagetool.AppImage"
      # unpack it so it also works where FUSE is unavailable (containers, some CI)
      ( cd "$TOOLS" && rm -rf squashfs-root appimagetool && ./appimagetool.AppImage --appimage-extract >/dev/null && mv squashfs-root appimagetool )
    fi
    TOOL="$TOOLS/appimagetool/AppRun"
  fi
fi
ARCH="$ARCH" "$TOOL" --no-appstream AppDir "$OUT" >/dev/null
chmod +x "$OUT"

# ---------------------------------------------------------------- 7. report
NEED=$(objdump -T "target/${TARGET}/release/pen-touch" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -uV | tail -1 || true)
say "done: $OUT  ($(du -h "$OUT" | cut -f1))${NEED:+, needs ${NEED} or newer}"
echo "    try it:  ./$OUT --version"
