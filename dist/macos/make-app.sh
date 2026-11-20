#!/usr/bin/env sh
# Builds "Frame Fang.app" around a framefang binary that is already compiled.
#
# The release workflow runs this same script, so a bundle made by hand has
# exactly the layout and Info.plist of the one inside the published .dmg.
# Info.plist.in, next to this script, is the plist with @VERSION@ left blank.
#
# Usage, from the repository root:
#   sh dist/macos/make-app.sh --arch x64 --version 2.0.0 \
#       --binary framefang-rs/target/x86_64-apple-darwin/release/framefang \
#       [--out-dir DIR]   where the .app goes (default: dist)
#       [--icns FILE]     the app icon (default: icons/Framefang.icns)
set -e -u

me=$(basename "$0")
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo=$(CDPATH= cd -- "$here/../.." && pwd)

fail() {
    printf '%s: %s\n' "$me" "$*" >&2
    exit 2
}

# The help text is the comment block at the top of this file, minus the
# shebang, so the two can't disagree.
show_help() {
    sed -n -e 1d -e '/^#/!q' -e 's/^# \{0,1\}//' -e p "$0"
}

binary=
version=
arch=
icns="$repo/icons/Framefang.icns"
out_dir=dist

# Every option takes a value; a missing one is an error, not a silent blank.
until [ "$#" -eq 0 ]; do
    opt=$1
    if [ "$opt" = -h ] || [ "$opt" = --help ]; then
        show_help
        exit 0
    fi
    case "$opt" in
        --binary|--version|--arch|--icns|--out-dir) ;;
        *) fail "unknown argument: $opt (try --help)" ;;
    esac
    [ "$#" -ge 2 ] && [ -n "$2" ] || fail "$opt needs a value"
    value=$2
    shift 2
    case "$opt" in
        --binary)  binary=$value ;;
        --version) version=$value ;;
        --arch)    arch=$value ;;
        --icns)    icns=$value ;;
        --out-dir) out_dir=$value ;;
    esac
done

for required in binary version arch; do
    eval "given=\${$required}"
    [ -n "$given" ] || fail "--$required is required"
done
[ -f "$binary" ] || fail "binary not found: $binary"
[ -f "$icns" ]   || fail "icns not found: $icns"

# Only the two architectures the release builds.
[ "$arch" = arm64 ] || [ "$arch" = x64 ] || fail "unknown arch: $arch (expected arm64 or x64)"

# The version is pasted into the plist by sed below; letters, digits, dots,
# plus and minus are all a version needs and none of them confuse sed.
if printf '%s' "$version" | LC_ALL=C tr -d '0-9A-Za-z.+-' | grep -q .; then
    fail "version has unexpected characters: $version"
fi

plist_template="$here/Info.plist.in"
[ -f "$plist_template" ] || fail "missing plist template: $plist_template"

bundle="$out_dir/Frame Fang.app"
contents="$bundle/Contents"
rm -rf "$bundle"
mkdir -p "$contents/MacOS" "$contents/Resources"

# Info.plist names the executable `framefang` and the icon `framefang`, so
# both are copied under those names whatever the inputs were called.
cp "$binary" "$contents/MacOS/framefang"
chmod +x "$contents/MacOS/framefang"
cp "$icns" "$contents/Resources/framefang.icns"
sed "s/@VERSION@/$version/g" "$plist_template" > "$contents/Info.plist"

printf '%s: built %s (version %s, arch %s)\n' "$me" "$bundle" "$version" "$arch"
