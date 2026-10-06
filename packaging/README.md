# Packaging

`packaging/package.sh` → `dist/ustan-<version>-linux-<arch>.tar.gz` (+ `.sha256`). Needs zig to build. Gentoo: `packaging/gentoo/app-misc/ustan/` (live ebuild; USE gui, flatpak, wine).

Install from the tarball: `./install.sh` (under `/usr/local`, `PREFIX=$HOME/.local` works without root), `DESTDIR=… ./install.sh` to stage, `./install.sh uninstall` to remove
what it installed (it records a manifest). Existing files in `/etc` are never overwritten; the new copy is written as `<name>.new`.
Checked: tarball install/uninstall in a DESTDIR, desktop-file-validate, appstreamcli, `emerge -pv` on the ebuild (not built).
