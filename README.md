# ustan

A Windows-style installer for Linux: open a package, see what it is, press *Install*.
Everything goes into your home directory — no root — with a menu entry, an icon and a clean
uninstall, whatever the format.

| Format | How it is installed |
|---|---|
| `.deb`, `.rpm`, Arch `.pkg.tar.*` | unpacked into `~/.local/opt/<app>`; programs run with the package overlaid on `/usr` (bubblewrap), so hard-coded `/usr/share/<app>` paths work |
| `.AppImage` | copied, icon and `.desktop` read from the image (no need to run it) |
| `.snap` | unpacked without snapd; base and content snaps are fetched from the store (SHA3-384 verified) and run in a bubblewrap sandbox |
| Flatpak (`flatpak:<id>`, `.flatpakref`, `.flatpak`) | delegated to `flatpak --user` |
| `.exe`, `.msi` | run through Wine in a prefix per app; installers (`--installer`, `--installer-args "/S"`) have their Start Menu shortcuts turned into launchers |
| `.tar.*`, `.zip`, `.7z` | menu entry for GUI programs, command in `~/.local/bin` for tools; Windows programs inside run through Wine |
| `.jar` | `java -jar` launcher |
| makeself `.run`/`.sh` | the payload is unpacked; the embedded script is never run |
| bare ELF binary | a menu entry or a command, depending on the toolkit it links |

Downloads (`ustan install <url>`) are verified against a checksum published next to the file or
by the store, and say so when nothing could be checked.

## Use

```
ustan install <file | url | flatpak:id> [--installer] [--installer-args "/S"] [--sha256 …]
ustan inspect <file | url>      what is this, without installing
ustan list                      installed apps (runtimes are marked)
ustan remove <id>               ustan prune        remove unused runtimes
ustan update [id] [--check]     ustan scan [--update]   AppImages found elsewhere on disk
ustan watch [--once]            check in the background; ustan autoupdate enable|disable
ustan runner [use <name>]       which Wine to use (system, PortProton, Steam Proton, …)
ustan register [--exe] [--archives]   open these file types with ustan on double click
```

`ustan-gui` is the GTK4/libadwaita front end: it opens the files you double-click, shows what
they are, with progress and a cancel button, and lists installed apps with Update / Launch /
Remove. Running it without a file opens the manager.

## Notes

* Updates come from the Flatpak remote, the store (snaps), GitHub Releases (AppImages that carry
  `gh-releases-zsync` update info) and the HTTP validator of the URL a file was installed from.
* After install, the programs of a package are scanned for libraries that are neither in the package
  nor on the system; the result is reported, not guessed.
* bubblewrap is used to make a package believe it is installed, **not** as a security boundary:
  programs still see your home directory. Install what you trust.
* Most behaviour was verified on one machine (Gentoo, OpenRC, niri). Anything that talks to a
  package manager's runtime (snaps that rely on other snaps, Windows installers) can still fail
  in ways the tests cannot foresee.

## Build

Needs Rust and Zig (the container-format parsers are Zig, built by `crates/core/build.rs`);
the GUI also needs GTK 4 and libadwaita 1.6.

```
cargo build --release --workspace
cargo test --workspace && (cd zig && zig build test)
```

Gentoo: a live ebuild is in `packaging/gentoo/` (`app-misc/ustan-9999`).

GPL-2.0-or-later.
