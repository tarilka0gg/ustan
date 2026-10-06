# Copyright 2026 Gentoo Authors
# Distributed under the terms of the GNU General Public License v2

EAPI=8

CRATES="
	adler2@2.0.1
	anstream@1.0.0
	anstyle-parse@1.0.0
	anstyle-query@1.1.5
	anstyle-wincon@3.0.11
	anstyle@1.0.14
	arbitrary@1.4.2
	async-channel@2.5.0
	autocfg@1.5.1
	backhand@0.25.5
	base64@0.22.1
	bit-set@0.6.0
	bit-vec@0.7.0
	bitflags@2.13.2
	bitvec@1.1.1
	block-buffer@0.10.4
	bumpalo@3.20.3
	bytemuck@1.25.2
	byteorder-lite@0.1.0
	byteorder@1.5.0
	bzip2@0.6.1
	cairo-rs@0.22.9
	cairo-sys-rs@0.22.9
	cc@1.6.0
	cfg-expr@0.20.10
	cfg-if@1.0.5
	chrono@0.4.45
	clap@4.6.7
	clap_builder@4.6.7
	clap_derive@4.6.7
	clap_lex@1.1.1
	colorchoice@1.0.5
	concurrent-queue@2.5.0
	cpufeatures@0.2.17
	crc-catalog@2.5.0
	crc32fast@1.5.2
	crc@3.4.0
	crossbeam-deque@0.8.8
	crossbeam-epoch@0.9.21
	crossbeam-utils@0.8.23
	crypto-common@0.1.7
	darling@0.21.3
	darling_core@0.21.3
	darling_macro@0.21.3
	deku@0.20.3
	deku_derive@0.20.3
	deranged@0.5.8
	derive_arbitrary@1.4.2
	digest@0.10.7
	displaydoc@0.2.7
	either@1.18.0
	equivalent@1.0.2
	errno@0.3.14
	event-listener-strategy@0.5.4
	event-listener@5.4.2
	fdeflate@0.3.7
	field-offset@0.3.6
	filetime@0.2.29
	filetime_creation@0.2.0
	find-msvc-tools@0.1.14
	flate2@1.1.10
	fnv@1.0.7
	form_urlencoded@1.2.2
	funty@2.0.0
	futures-channel@0.3.34
	futures-core@0.3.34
	futures-executor@0.3.34
	futures-io@0.3.34
	futures-macro@0.3.34
	futures-task@0.3.34
	futures-util@0.3.34
	gdk-pixbuf-sys@0.22.9
	gdk-pixbuf@0.22.0
	gdk4-sys@0.11.5
	gdk4@0.11.5
	generic-array@0.14.7
	getrandom@0.2.17
	getrandom@0.4.3
	gio-sys@0.22.9
	gio@0.22.10
	glib-macros@0.22.9
	glib-sys@0.22.9
	glib@0.22.10
	gobject-sys@0.22.9
	graphene-rs@0.22.8
	graphene-sys@0.22.9
	gsk4-sys@0.11.5
	gsk4@0.11.5
	gtk4-macros@0.11.5
	gtk4-sys@0.11.5
	gtk4@0.11.5
	hashbrown@0.17.1
	heck@0.5.0
	hermit-abi@0.5.3
	icu_collections@2.3.0
	icu_locale_core@2.3.0
	icu_normalizer@2.3.0
	icu_normalizer_data@2.3.0
	icu_properties@2.3.0
	icu_properties_data@2.3.0
	icu_provider@2.3.1
	ident_case@1.0.1
	idna@1.1.0
	idna_adapter@1.2.2
	image@0.25.10
	indexmap@2.14.2
	is_terminal_polyfill@1.70.2
	itoa@1.0.18
	jobserver@0.1.35
	js-sys@0.3.106
	keccak@0.1.6
	libadwaita-sys@0.9.2
	libadwaita@0.9.2
	libbz2-rs-sys@0.2.5
	libc@0.2.190
	liblzma-sys@0.4.9
	liblzma@0.4.8
	linux-raw-sys@0.12.1
	litemap@0.8.3
	log@0.4.34
	lz4_flex@0.14.0
	lzma-rust@0.1.7
	memchr@2.8.3
	memmap2@0.9.11
	memoffset@0.9.1
	miniz_oxide@0.8.9
	miniz_oxide@0.9.1
	moxcms@0.8.1
	no_std_io2@0.9.4
	nt-time@0.8.1
	num-conv@0.2.2
	num-traits@0.2.19
	num_cpus@1.17.0
	once_cell@1.21.4
	once_cell_polyfill@1.70.2
	pango-sys@0.22.9
	pango@0.22.9
	parking@2.2.1
	percent-encoding@2.3.2
	pin-project-lite@0.2.17
	pkg-config@0.3.34
	png@0.18.1
	potential_utf@0.1.6
	powerfmt@0.2.1
	proc-macro-crate@3.5.0
	proc-macro2@1.0.107
	pxfm@0.1.30
	quote@1.0.47
	r-efi@6.0.0
	radium@0.7.0
	rayon-core@1.13.0
	rayon@1.12.0
	ring@0.17.14
	rust-lzo@0.6.2
	rustc_version@0.4.1
	rustix@1.1.5
	rustls-pki-types@1.15.1
	rustls-webpki@0.103.15
	rustls@0.23.45
	rustversion@1.0.23
	ruzstd@0.8.3
	ryu@1.0.23
	semver@1.0.28
	serde@1.0.229
	serde_core@1.0.229
	serde_derive@1.0.229
	serde_json@1.0.151
	serde_spanned@0.6.9
	serde_spanned@1.1.1
	serde_yaml@0.9.34+deprecated
	sevenz-rust@0.6.1
	sha2@0.10.9
	sha3@0.10.9
	shlex@2.0.1
	simd-adler32@0.3.10
	slab@0.4.12
	smallvec@1.16.2
	solana-nohash-hasher@0.2.1
	stable_deref_trait@1.2.1
	strsim@0.11.1
	subtle@2.6.1
	syn@2.0.119
	syn@3.0.6
	synstructure@0.14.0
	system-deps@7.0.8
	system-deps@9.0.0
	tap@1.0.1
	tar@0.4.46
	target-lexicon@0.13.5
	thiserror-impl@1.0.69
	thiserror-impl@2.0.21
	thiserror@1.0.69
	thiserror@2.0.21
	time-core@0.1.9
	time-macros@0.2.32
	time@0.3.55
	tinystr@0.8.4
	toml@0.8.23
	toml@1.1.6+spec-1.1.0
	toml_datetime@0.6.11
	toml_datetime@1.1.1+spec-1.1.0
	toml_edit@0.22.27
	toml_edit@0.25.15+spec-1.1.0
	toml_parser@1.1.3+spec-1.1.0
	toml_write@0.1.2
	toml_writer@1.1.2+spec-1.1.0
	tracing-attributes@0.1.31
	tracing-core@0.1.36
	tracing@0.1.44
	twox-hash@2.1.5
	typenum@1.20.1
	unicode-ident@1.0.26
	unsafe-libyaml@0.2.11
	untrusted@0.9.0
	ureq@2.12.1
	url@2.5.8
	utf8_iter@1.0.4
	utf8parse@0.2.2
	version-compare@0.2.1
	version_check@0.9.5
	wasi@0.11.1+wasi-snapshot-preview1
	wasm-bindgen-macro-support@0.2.129
	wasm-bindgen-macro@0.2.129
	wasm-bindgen-shared@0.2.129
	wasm-bindgen@0.2.129
	webpki-roots@0.26.11
	webpki-roots@1.0.9
	windows-link@0.2.1
	windows-sys@0.52.0
	windows-sys@0.61.2
	windows-targets@0.52.6
	windows_aarch64_gnullvm@0.52.6
	windows_aarch64_msvc@0.52.6
	windows_i686_gnu@0.52.6
	windows_i686_gnullvm@0.52.6
	windows_i686_msvc@0.52.6
	windows_x86_64_gnu@0.52.6
	windows_x86_64_gnullvm@0.52.6
	windows_x86_64_msvc@0.52.6
	winnow@0.7.15
	winnow@1.0.4
	writeable@0.6.4
	wyz@0.5.1
	xattr@1.6.1
	xxhash-rust@0.8.19
	yoke-derive@0.8.4
	yoke@0.8.3
	zerofrom-derive@0.1.8
	zerofrom@0.1.8
	zeroize@1.9.0
	zerotrie@0.2.5
	zerovec-derive@0.11.6
	zerovec@0.11.8
	zip@2.4.2
	zlib-rs@0.6.8
	zmij@1.0.23
	zopfli@0.8.3
	zstd-safe@7.3.0
	zstd-sys@2.1.0+zstd.1.5.7
	zstd@0.13.3
"

# highest rust-version among the dependencies
RUST_MIN_VER="1.92"

# The sources were written for this Zig (container-format parsers, built by crates/core/build.rs).
ZIG_SLOT="0.16"

inherit cargo desktop optfeature xdg zig-utils

DESCRIPTION="Windows-style app installer: deb, rpm, snap, AppImage, Flatpak, exe, archives"
HOMEPAGE="https://github.com/tarilka0gg/ustan"
SRC_URI="
	https://github.com/tarilka0gg/ustan/archive/refs/tags/v${PV}.tar.gz -> ${P}.tar.gz
	${CARGO_CRATE_URIS}
"

LICENSE="GPL-2+"
# Dependent crate licenses (generated by packaging/gentoo/gen-ebuild-data.py). rust-lzo is GPL-2 only,
# so the installed binaries are a GPL-2 combined work. liblzma-sys builds xz from source (0BSD),
# which backhand forces; bzip2 is a pure-Rust crate, zstd is linked from the system.
LICENSE+=" 0BSD Apache-2.0 Apache-2.0-with-LLVM-exceptions BSD BZIP2 Boost-1.0 CDLA-Permissive-2.0 GPL-2 ISC LGPL-2.1+ MIT Unicode-3.0 Unlicense ZLIB"
SLOT="0"
KEYWORDS="~amd64"
IUSE="+gui flatpak wine"

# bubblewrap: deb/rpm/Arch programs run with the package overlaid on /usr, snaps run in a sandbox
RDEPEND="
	app-arch/zstd:=
	sys-apps/bubblewrap
	x11-misc/xdg-utils
	gui? (
		>=gui-libs/gtk-4.10:4
		>=gui-libs/libadwaita-1.6:1
	)
	flatpak? ( sys-apps/flatpak )
	wine? ( virtual/wine )
"
DEPEND="
	app-arch/zstd:=
	gui? (
		>=gui-libs/gtk-4.10:4
		>=gui-libs/libadwaita-1.6:1
	)
"
BDEPEND="virtual/pkgconfig"

# Zig builds a static library that does not honour CFLAGS/LDFLAGS; the Rust side does
QA_FLAGS_IGNORED="usr/bin/ustan usr/bin/ustan-gui"

pkg_setup() {
	rust_pkg_setup
}

src_configure() {
	zig-utils_find_installation
	export ZIG="${ZIG_EXE}"          # crates/core/build.rs
	export ZSTD_SYS_USE_PKG_CONFIG=1 # system libzstd instead of the bundled copy
	cargo_src_configure
}

src_compile() {
	cargo_src_compile -p ustan $(usev gui '-p ustan-gui')
}

src_test() {
	cargo_src_test -p ustan-core -p ustan --  # trailing "--": cargo.eclass mis-splits args without it
	( cd zig && ezig build test ) || die "zig tests failed"
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
	einstalldocs
}

pkg_postinst() {
	xdg_pkg_postinst
	optfeature "update notifications (ustan watch)" x11-libs/libnotify
	optfeature ".jar applications" virtual/jre
	optfeature "installing from Flathub (flatpak: references)" sys-apps/flatpak
	if use gui; then
		elog "ustan-gui registers itself as the handler of its file types on its first start"
		elog "(undo with: ustan unregister). Or run: ustan register"
	fi
}

pkg_postrm() {
	xdg_pkg_postrm
}
