# Changelog

All notable changes to ustan, newest first. The format follows [Keep a Changelog](https://keepachangelog.com/), with
`## [version] - date` headings.

## [Unreleased]

### Added
- Archives: `tar.*` is streamed from disk instead of being loaded into RAM, and a multi-block `.xz` is decoded on several threads.
- `ustan runner get cachyos` downloads proton-cachyos from GitHub (sha512-checked).
- Kron4ek Wine-wrapper games in archives: the runner, DXVK (d3d9), wow64 and a menu entry are set up.

### Changed
- Messages are English by default and Ukrainian for `uk` locales; the language is decided by
  [simple-locale](https://github.com/tarilka0gg/simple-locale) (`USTAN_LANG`, `SIMPLE_LANG`, the shared `locale.toml`, the system locale).
- `start.sh`-style programs in an archive are named after the archive, not the script.
- README: screenshots of the GUI, with a title and a description under each.

## [0.2.2] - 2026-10-07

### Changed
- Opening the GUI without a file shows the manager instead of a file chooser.

## [0.2.1] - 2026-10-07

### Fixed
- AppImages start with `APPIMAGE_EXTRACT_AND_RUN=1` when FUSE 2 is missing (the launcher used to fail silently).

### Added
- The GUI runs launchers under watch and shows a toast with the program's last output when it dies right away.

## [0.2.0] - 2026-10-06

### Added
- Formats: `.deb`, `.rpm`, Arch `.pkg.tar.*`, `.AppImage`, `.snap` (without snapd), Flatpak, `.exe` and `.msi` (through Wine), `.jar`, tarballs, `.zip`,
  `.7z`, makeself `.run`, and bare ELF binaries; everything is installed per user with a menu entry, an icon and a clean uninstall.
- deb, rpm and Arch programs run with the package overlaid on `/usr` (bubblewrap); a check for missing libraries after the install.
- Wine runner discovery (system Wine, PortProton, Steam Proton) and installer arguments for silent Windows installers.
- Progress, cancel and checksum verification; runtimes are tracked, listed apart and pruned when unused.
- Updates: `ustan update`, `scan` for AppImages found elsewhere on disk, and background checks (`ustan watch`, `autoupdate`).
- `ustan register`: ustan opens these file types on a double click; it registers itself at the first GUI start and on package install.
- Integration tests for every format and a CI workflow.
- Gentoo ebuilds (live and release) prepared for GURU.
