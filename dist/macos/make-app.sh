#!/bin/sh
# Assemble "Frame Fang.app" from an already-built `framefang` binary.
#
# The one definition of the macOS bundle: the release workflow
# (.github/workflows/release.yml) calls this script, so a local run produces
# the same layout and the same Info.plist as the released .dmg. The plist
# lives next to this script in Info.plist.in, with @VERSION@ filled in.
#
#   dist/macos/make-app.sh \
#     --binary framefang-rs/target/release/framefang \
#     --version 1.3.1 \
#     --arch arm64 \
#     --out-dir framefang-rs/dist
#
# Optional:
#   --icns PATH      prebuilt .icns (default icons/Framefang.icns)
#
# Adapted from Kashot's dist/macos/make-app.sh (Apache-2.0, see NOTICE).
set -eu

PROG=$(basename "$0")
SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)

BINARY=
VERSION=
ARCH=
ICNS="$REPO_ROOT/icons/Framefang.icns"
OUT_DIR=dist

die() {
    echo "$PROG: $*" >&2
    exit 1
}

# Print this file's leading comment block as the help text.
usage() {
    awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"
}

while [ $# -gt 0 ]; do
    case "$1" in
        --binary)  BINARY=${2:?--binary needs a path};   shift 2 ;;
        --version) VERSION=${2:?--version needs a value}; shift 2 ;;
        --arch)    ARCH=${2:?--arch needs a value};       shift 2 ;;
        --icns)    ICNS=${2:?--icns needs a path};        shift 2 ;;
        --out-dir) OUT_DIR=${2:?--out-dir needs a path};  shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) die "unknown argument: $1 (try --help)" ;;
    esac
done

[ -n "$BINARY" ]  || die "--binary is required"
[ -n "$VERSION" ] || die "--version is required"
[ -n "$ARCH" ]    || die "--arch is required"
[ -f "$BINARY" ]  || die "binary not found: $BINARY"
[ -f "$ICNS" ]    || die "icns not found: $ICNS"

case "$ARCH" in
    arm64|x64) ;;
    *) die "unknown arch: $ARCH (expected arm64 or x64)" ;;
esac

# The version lands in the plist through sed, so keep it to safe characters.
case "$VERSION" in
    *[!0-9A-Za-z.+-]*) die "version has unexpected characters: $VERSION" ;;
esac

TEMPLATE="$SCRIPT_DIR/Info.plist.in"
[ -f "$TEMPLATE" ] || die "missing plist template: $TEMPLATE"

APP="$OUT_DIR/Frame Fang.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

# The binary inside the bundle is always `framefang` (CFBundleExecutable).
cp "$BINARY" "$APP/Contents/MacOS/framefang"
chmod +x "$APP/Contents/MacOS/framefang"
cp "$ICNS" "$APP/Contents/Resources/framefang.icns"

sed "s/@VERSION@/$VERSION/g" "$TEMPLATE" > "$APP/Contents/Info.plist"

echo "$PROG: built $APP (version $VERSION, arch $ARCH)"
