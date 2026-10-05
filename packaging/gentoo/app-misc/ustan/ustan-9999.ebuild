# Copyright 1999-2026 Gentoo Authors
# Distributed under the terms of the GNU General Public License v2

EAPI=8

CRATES=""
# Live ebuild: cargo_live_src_unpack fetches the crates at unpack time (network needed, allowed for 9999).
# EGIT_REPO_URI is the public GitHub repository; to build a local checkout instead set
#   EGIT_OVERRIDE_REPO_TARILKA0GG_USTAN=file:///path/to/ustan  (see git-r3.eclass).

RUST_MIN_VER="1.92"

inherit cargo desktop git-r3 xdg

DESCRIPTION="Windows-style app installer for Linux: .deb, AppImage, Flatpak and .exe"
HOMEPAGE="https://github.com/tarilka0gg/ustan"
EGIT_REPO_URI="https://github.com/tarilka0gg/ustan.git"

LICENSE="GPL-2+"
SLOT="0"
KEYWORDS=""
IUSE="+gui flatpak +snap wine"

RDEPEND="
	gui? (
		gui-libs/gtk:4
		gui-libs/libadwaita:1
	)
	flatpak? ( sys-apps/flatpak )
	# snaps run in a bubblewrap sandbox (base snap as root), see crates/core/src/backend/snap.rs
	snap? ( sys-apps/bubblewrap )
	wine? ( virtual/wine )
	x11-misc/xdg-utils
"
DEPEND="${RDEPEND}"
# The container-format library (ar/elf/lnk/pe parsing) is Zig, built by crates/core/build.rs.
BDEPEND="
	|| ( dev-lang/zig dev-lang/zig-bin )
	virtual/pkgconfig
"

QA_FLAGS_IGNORED="usr/bin/ustan usr/bin/ustan-gui"

src_unpack() {
	git-r3_src_unpack
	cargo_live_src_unpack
}

src_compile() {
	cargo_src_compile -p ustan $(usev gui '-p ustan-gui')
}

src_install() {
	cargo_src_install --path crates/cli
	if use gui; then
		cargo_src_install --path crates/gui
		domenu packaging/io.github.tarilka0gg.Ustan.desktop
	fi
	insinto /usr/share/metainfo
	doins packaging/io.github.tarilka0gg.Ustan.metainfo.xml
	insinto /usr/share/icons/hicolor/scalable/apps
	doins packaging/icons/ustan.svg
}
