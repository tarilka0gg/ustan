#!/bin/bash
# package.sh - build a release and pack it: dist/ustan-<version>-linux-<arch>.tar.gz (+ .sha256).
#   packaging/package.sh                 build with cargo (needs zig for the container-format library) and pack
#   SKIP_BUILD=1 packaging/package.sh    pack what target/release already holds
# The tarball has `install.sh` (install / uninstall, PREFIX, DESTDIR) and `prefix/`.
set -euo pipefail
cd "$(dirname "$0")/.."
NAME=ustan
ID=io.github.tarilka0gg.Ustan
VERSION=$(sed -n '/^\[workspace.package\]/,/^$/s/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
ARCH=$(uname -m)
[ "${SKIP_BUILD:-}" = 1 ] || cargo build --release --locked

D=dist/$NAME-$VERSION
rm -rf "${D:?}"
install -Dm755 target/release/ustan target/release/ustan-gui -t "$D/prefix/bin"
# The build machine's libc may carry an "x86-64-v3 needed" note that stops the binary on older CPUs
# ("CPU ISA level is lower than required"); the code itself is compiled for the baseline.
objcopy --remove-section=.note.gnu.property "$D/prefix/bin/ustan"
objcopy --remove-section=.note.gnu.property "$D/prefix/bin/ustan-gui"
install -Dm644 "packaging/$ID.desktop" -t "$D/prefix/share/applications"
install -Dm644 "packaging/$ID.metainfo.xml" -t "$D/prefix/share/metainfo"
install -Dm644 packaging/icons/$NAME.svg -t "$D/prefix/share/icons/hicolor/scalable/apps"
[ ! -f LICENSE ] || install -Dm644 LICENSE -t "$D/prefix/share/doc/$NAME"
install -m755 packaging/install.sh "$D/install.sh"
cat > "$D/POST-INSTALL.txt" <<'TXT'
`ustan-gui` opens a .deb / AppImage / Flatpak you double-click; `ustan install <file|url>` does it from a terminal.
To make double-click open it for good: `ustan register` (add `--exe` to also take over Windows .exe files).
Optional: wine (for .exe installers), flatpak, xdg-utils.
TXT

OUT=dist/$NAME-$VERSION-linux-$ARCH.tar.gz
tar -C dist --owner=0 --group=0 -czf "$OUT" "$NAME-$VERSION"
(cd dist && sha256sum "$(basename "$OUT")" > "$(basename "$OUT").sha256")
echo "$OUT"
