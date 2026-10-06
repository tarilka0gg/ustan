//! End-to-end checks of the package formats without any network: each test builds a small package
//! in memory, installs it into a throwaway home through the same backend selection the CLI uses,
//! looks at what appeared on disk, and uninstalls it again.
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use ustan_core::backend::{self, Opts};
use ustan_core::dirs::Dirs;
use ustan_core::manifest::Manifest;

struct Env {
    home: PathBuf,
    dirs: Dirs,
}

impl Env {
    fn new(tag: &str) -> Env {
        let home = std::env::temp_dir().join(format!("ustan-it-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let dirs = Dirs::under(&home);
        Env { home, dirs }
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.home.join("dl").join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        p
    }

    /// Install `path` exactly like `ustan install`.
    fn install(&self, path: &Path) -> Manifest {
        backend::pick(path).unwrap_or_else(|| panic!("no backend for {}", path.display())).install(path, &self.dirs, &Opts::default()).unwrap()
    }

    fn bin(&self) -> PathBuf {
        self.home.join(".local/bin")
    }

    /// After uninstall nothing of the package (or its launchers) may be left; the downloaded file may.
    fn assert_clean(&self) {
        let left: Vec<_> = walk(&self.home).into_iter().filter(|p| !p.starts_with(self.home.join("dl"))).filter(|p| p.is_file() || p.is_symlink()).collect();
        assert!(left.is_empty(), "left behind: {left:?}");
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn walk(d: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(d) {
        for e in rd.flatten() {
            let p = e.path();
            v.push(p.clone());
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                v.extend(walk(&p));
            }
        }
    }
    v
}

// ---------- fixture builders ----------

fn png() -> Vec<u8> {
    // a valid 1x1 PNG
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, 0x49, 0x48, 0x44, 0x52, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1F, 0x15, 0xC4, 0x89, 0, 0, 0,
        0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xFF, 0xFF, 0x3F, 0, 5, 0xFE, 2, 0xFE, 0xA7, 0x35, 0x81, 0x84, 0, 0, 0, 0, 0x49, 0x45, 0x4E, 0x44, 0xAE,
        0x42, 0x60, 0x82,
    ]
}

enum E<'a> {
    File(&'a str, &'a [u8], u32),
    Link(&'a str, &'a str),
}

fn tar_bytes(entries: &[E]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for e in entries {
        let mut h = tar::Header::new_gnu();
        match e {
            E::File(path, data, mode) => {
                h.set_size(data.len() as u64);
                h.set_mode(*mode);
                h.set_entry_type(tar::EntryType::Regular);
                b.append_data(&mut h, path, *data).unwrap();
            }
            E::Link(path, target) => {
                h.set_size(0);
                h.set_mode(0o777);
                h.set_entry_type(tar::EntryType::Symlink);
                b.append_link(&mut h, path, target).unwrap();
            }
        }
    }
    b.into_inner().unwrap()
}

fn gz(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

fn ar(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = b"!<arch>\n".to_vec();
    for (name, data) in members {
        out.extend_from_slice(format!("{:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n", name, 0, 0, 0, "100644", data.len()).as_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    out
}

const SCRIPT: &[u8] = b"#!/bin/sh\necho ok\n";
const DESKTOP: &[u8] = b"[Desktop Entry]\nType=Application\nName=Hello Test\nExec=hello %U\nIcon=hello\n";

fn app_tree() -> Vec<E<'static>> {
    // leaked on purpose: fixtures live for the whole test run
    let png: &'static [u8] = Box::leak(png().into_boxed_slice());
    vec![
        E::File("usr/bin/hello", SCRIPT, 0o755),
        E::Link("usr/bin/hi", "/usr/bin/hello"),
        E::File("usr/share/applications/hello.desktop", DESKTOP, 0o644),
        E::File("usr/share/icons/hicolor/64x64/apps/hello.png", png, 0o644),
    ]
}

/// Common checks for a deb/rpm/arch package carrying `app_tree`.
fn check_tree_package(env: &Env, m: &Manifest, id: &str) {
    let root = env.dirs.opt.join(id);
    assert!(std::fs::metadata(root.join("usr/bin/hello")).unwrap().permissions().mode() & 0o111 != 0, "program is executable");
    // an absolute symlink inside the package must point into the private root, not at the host
    let link = std::fs::read_link(root.join("usr/bin/hi")).unwrap();
    assert!(link.is_relative(), "{link:?}");
    assert!(root.join("usr/bin/hi").exists(), "the link resolves inside the package");

    let d = env.dirs.apps.join(format!("ustan-{id}-hello.desktop"));
    let text = std::fs::read_to_string(&d).unwrap();
    assert!(text.contains(&root.join("usr/bin/hello").display().to_string()), "Exec points into the root: {text}");
    assert!(text.contains(".ustan-run/overlay"), "runs through the overlay launcher: {text}");
    assert!(text.contains(&format!("Icon={}", root.join("usr/share/icons/hicolor/64x64/apps/hello.png").display())), "{text}");
    assert!(root.join(".ustan-run/overlay").is_file());

    assert_eq!(Manifest::list(&env.dirs.state).unwrap().len(), 1);
    m.uninstall(&env.dirs.state).unwrap();
    assert!(Manifest::list(&env.dirs.state).unwrap().is_empty());
    env.assert_clean();
}

// ---------- the tests ----------

#[test]
fn deb() {
    let env = Env::new("deb");
    let control = gz(&tar_bytes(&[E::File("control", b"Package: Hello-Test\nVersion: 1.0-2\n", 0o644)]));
    let data = gz(&tar_bytes(&app_tree()));
    let p = env.file("hello-test_1.0-2_amd64.deb", &ar(&[("debian-binary", b"2.0\n"), ("control.tar.gz", &control), ("data.tar.gz", &data)]));
    let info = backend::pick(&p).unwrap().inspect(&p).unwrap();
    assert_eq!((info.id.as_str(), info.version.as_deref(), info.kind), ("hello-test", Some("1.0-2"), "deb"));
    let m = env.install(&p);
    assert_eq!(m.version.as_deref(), Some("1.0-2"));
    check_tree_package(&env, &m, "hello-test");
}

#[test]
fn deb_rejects_a_second_install_of_the_same_package() {
    let env = Env::new("deb2");
    let control = gz(&tar_bytes(&[E::File("control", b"Package: dup\nVersion: 1\n", 0o644)]));
    let data = gz(&tar_bytes(&[E::File("usr/bin/dup", SCRIPT, 0o755)]));
    let p = env.file("dup.deb", &ar(&[("debian-binary", b"2.0\n"), ("control.tar.gz", &control), ("data.tar.gz", &data)]));
    let m = env.install(&p);
    let again = backend::pick(&p).unwrap().install(&p, &env.dirs, &Opts::default());
    assert!(again.is_err());
    assert_eq!(Manifest::list(&env.dirs.state).unwrap().len(), 1, "the failed attempt must not disturb the first install");
    m.uninstall(&env.dirs.state).unwrap();
    env.assert_clean();
}

#[test]
fn arch_package() {
    let env = Env::new("arch");
    let mut entries = vec![E::File(".PKGINFO", b"pkgname = hello-test\npkgver = 2.0-1\n", 0o644), E::File(".MTREE", b"x", 0o644)];
    entries.extend(app_tree());
    let p = env.file("hello-test-2.0-1-x86_64.pkg.tar.gz", &gz(&tar_bytes(&entries)));
    let m = env.install(&p);
    assert_eq!((m.kind.as_str(), m.version.as_deref()), ("arch", Some("2.0-1")));
    assert!(!env.dirs.opt.join("hello-test/.PKGINFO").exists(), "package metadata is not unpacked");
    check_tree_package(&env, &m, "hello-test");
}

/// RPM: lead + (empty) signature header + main header with NAME/VERSION/RELEASE + gzip'ed cpio.
fn cpio(entries: &[(&str, u32, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut put = |name: &str, mode: u32, data: &[u8], ino: u32| {
        out.extend_from_slice(format!("070701{ino:08X}{mode:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}", 0, 0, 1, 0, data.len(), 0, 0, 0, 0, name.len() + 1, 0).as_bytes());
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    };
    for (i, (n, m, d)) in entries.iter().enumerate() {
        put(n, *m, d, i as u32 + 1);
    }
    put("TRAILER!!!", 0, b"", 0);
    out
}

fn rpm_header(tags: &[(u32, &str)]) -> Vec<u8> {
    let (mut index, mut store) = (Vec::new(), Vec::new());
    for (tag, val) in tags {
        index.extend_from_slice(&tag.to_be_bytes());
        index.extend_from_slice(&6u32.to_be_bytes()); // STRING
        index.extend_from_slice(&(store.len() as u32).to_be_bytes());
        index.extend_from_slice(&1u32.to_be_bytes());
        store.extend_from_slice(val.as_bytes());
        store.push(0);
    }
    let mut h = vec![0x8e, 0xad, 0xe8, 0x01, 0, 0, 0, 0];
    h.extend_from_slice(&(tags.len() as u32).to_be_bytes());
    h.extend_from_slice(&(store.len() as u32).to_be_bytes());
    h.extend(index);
    h.extend(store);
    h
}

#[test]
fn rpm_package() {
    let env = Env::new("rpm");
    let png = png();
    let payload = gz(&cpio(&[
        ("./usr", 0o040755, b""),
        ("./usr/bin", 0o040755, b""),
        ("./usr/bin/hello", 0o100755, SCRIPT),
        ("./usr/bin/hi", 0o120777, b"/usr/bin/hello"),
        ("./usr/share/applications", 0o040755, b""),
        ("./usr/share/applications/hello.desktop", 0o100644, DESKTOP),
        ("./usr/share/icons/hicolor/64x64/apps/hello.png", 0o100644, &png),
    ]));
    let mut rpm = vec![0xed, 0xab, 0xee, 0xdb];
    rpm.resize(96, 0); // lead
    let sig = rpm_header(&[]);
    let sig_len = sig.len();
    rpm.extend(sig);
    while (sig_len + rpm.len() - sig_len) % 8 != 0 && rpm.len() % 8 != 0 {
        rpm.push(0); // the signature header is padded to 8 bytes
    }
    rpm.extend(rpm_header(&[(1000, "hello-test"), (1001, "3.0"), (1002, "1.fc99"), (1125, "gzip")]));
    rpm.extend(payload);
    let p = env.file("hello-test-3.0-1.fc99.x86_64.rpm", &rpm);
    let m = env.install(&p);
    assert_eq!((m.kind.as_str(), m.version.as_deref()), ("rpm", Some("3.0-1.fc99")));
    check_tree_package(&env, &m, "hello-test");
}

#[test]
fn tar_gz_cli_tool_goes_to_bin() {
    let env = Env::new("targz");
    let tar = gz(&tar_bytes(&[E::File("tool-1.4/bin/tool", SCRIPT, 0o755), E::File("tool-1.4/README", b"hi", 0o644)]));
    let p = env.file("tool-1.4-linux.tar.gz", &tar);
    let m = env.install(&p);
    assert_eq!((m.id.as_str(), m.version.as_deref()), ("tool", Some("1.4")));
    let link = env.bin().join("tool");
    assert!(link.exists() && std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
    assert!(env.dirs.apps.read_dir().map(|mut r| r.next().is_none()).unwrap_or(true), "a CLI tool gets no menu entry");
    m.uninstall(&env.dirs.state).unwrap();
    env.assert_clean();
}

#[test]
fn tar_gz_with_icon_is_a_menu_app() {
    let env = Env::new("targzgui");
    let png = png();
    let tar = gz(&tar_bytes(&[E::File("app-2.0/bin/app", SCRIPT, 0o755), E::File("app-2.0/share/icons/app-icon.png", &png, 0o644)]));
    let p = env.file("app-2.0-linux.tar.gz", &tar);
    let m = env.install(&p);
    let d = std::fs::read_to_string(env.dirs.apps.join("ustan-app.desktop")).unwrap();
    assert!(d.contains("Name=App") && d.contains("Icon=") && d.contains("app-icon.png"), "{d}");
    assert!(!env.bin().join("app").exists());
    m.uninstall(&env.dirs.state).unwrap();
    env.assert_clean();
}

#[test]
fn path_traversal_in_an_archive_is_not_followed() {
    let env = Env::new("evil");
    let mut raw = tar_bytes(&[E::File("ok/bin/tool", SCRIPT, 0o755)]);
    // hand-made entry whose name climbs out of the install directory
    let mut h = tar::Header::new_gnu();
    h.set_size(4);
    h.set_mode(0o644);
    h.set_entry_type(tar::EntryType::Regular);
    let mut b = tar::Builder::new(Vec::new());
    b.append_data(&mut h, "x", &b"evil"[..]).unwrap();
    let mut evil = b.into_inner().unwrap();
    evil[..9].copy_from_slice(b"../../evi"); // overwrite the start of the name field
    evil[9] = b'l';
    raw.splice(raw.len() - 1024..raw.len() - 1024, evil[..evil.len() - 1024].iter().copied());
    let p = env.file("evil-1.0.tar.gz", &gz(&raw));
    let _ = backend::pick(&p).unwrap().install(&p, &env.dirs, &Opts::default());
    assert!(!env.home.join("evil").exists() && !env.home.parent().unwrap().join("evil").exists(), "nothing may escape the root");
}

#[test]
fn zip_archive() {
    let env = Env::new("zip");
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().unix_permissions(0o755);
    z.start_file("bar-2.0/bin/bar", opts).unwrap();
    z.write_all(SCRIPT).unwrap();
    let bytes = z.finish().unwrap().into_inner();
    let p = env.file("bar-2.0.zip", &bytes);
    let m = env.install(&p);
    assert!(env.bin().join("bar").exists());
    m.uninstall(&env.dirs.state).unwrap();
    env.assert_clean();
}

#[test]
fn sevenz_archive() {
    let env = Env::new("7z");
    let src = env.home.join("src/seven-1.0/bin");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("seven"), SCRIPT).unwrap();
    std::fs::set_permissions(src.join("seven"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = env.home.join("dl/seven-1.0.7z");
    std::fs::create_dir_all(out.parent().unwrap()).unwrap();
    sevenz_rust::compress_to_path(env.home.join("src/seven-1.0"), &out).unwrap();
    let m = env.install(&out);
    assert_eq!(m.id, "seven");
    assert!(env.bin().join("seven").exists(), "the executable bit survived the 7z round trip");
    m.uninstall(&env.dirs.state).unwrap();
    std::fs::remove_dir_all(env.home.join("src")).unwrap();
    env.assert_clean();
}

fn jar(with_icon: bool, main: bool) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    z.start_file("META-INF/MANIFEST.MF", o).unwrap();
    let mf = if main { "Manifest-Version: 1.0\nMain-Class: Hello\nImplementation-Title: Hello Jar\nImplementation-Version: 3.1\n" } else { "Manifest-Version: 1.0\n" };
    z.write_all(mf.as_bytes()).unwrap();
    if with_icon {
        z.start_file("images/app-icon.png", o).unwrap();
        z.write_all(&png()).unwrap();
    }
    z.finish().unwrap().into_inner()
}

#[test]
fn jar_with_icon_is_a_menu_app_and_without_is_a_command() {
    let env = Env::new("jar");
    let m = env.install(&env.file("hello-gui.jar", &jar(true, true)));
    assert_eq!((m.name.as_str(), m.version.as_deref()), ("Hello Jar", Some("3.1")));
    let d = std::fs::read_to_string(env.dirs.apps.join("ustan-hello-jar.desktop")).unwrap();
    assert!(d.contains("Exec=java -jar") && d.contains("icon.png"), "{d}");
    m.uninstall(&env.dirs.state).unwrap();
    env.assert_clean();

    let m = env.install(&env.file("hello-cli.jar", &jar(false, true)));
    let s = std::fs::read_to_string(env.bin().join("hello-jar")).unwrap();
    assert!(s.contains("exec java -jar") && s.ends_with("\"$@\"\n"), "{s}");
    m.uninstall(&env.dirs.state).unwrap();
    env.assert_clean();

    let lib = env.file("lib.jar", &jar(false, false));
    assert!(backend::pick(&lib).unwrap().install(&lib, &env.dirs, &Opts::default()).is_err(), "a library jar has no Main-Class");
}

#[test]
fn makeself_unpacks_without_running_anything() {
    let env = Env::new("makeself");
    let payload = gz(&tar_bytes(&[E::File("mk-1.0/bin/mk", SCRIPT, 0o755)]));
    let header = "#!/bin/sh\n# This script was generated using Makeself 2.5.0\nskip=\"4\"\ntouch /tmp/ustan-must-not-run\nexit 0\n";
    let mut file = header.replace("skip=\"4\"", "skip=\"5\"").into_bytes(); // 5 header lines
    file.extend(payload);
    let p = env.file("mk-1.0.run", &file);
    let m = env.install(&p);
    assert_eq!((m.kind.as_str(), m.version.as_deref()), ("makeself", Some("1.0")));
    assert!(env.bin().join("mk").exists());
    assert!(!Path::new("/tmp/ustan-must-not-run").exists(), "the embedded script must never be executed");
    m.uninstall(&env.dirs.state).unwrap();
    env.assert_clean();
}

#[test]
fn bare_elf_binary_becomes_a_command() {
    let env = Env::new("elf");
    // the test binary itself is an ELF executable that does not link a GUI toolkit
    let bytes = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    let p = env.file("tool-1.2-linux-amd64", &bytes);
    let m = env.install(&p);
    assert_eq!((m.id.as_str(), m.version.as_deref(), m.kind.as_str()), ("tool", Some("1.2"), "elf"));
    assert!(env.bin().join("tool").exists(), "named by the cleaned name, not the file name");
    m.uninstall(&env.dirs.state).unwrap();
    env.assert_clean();
}

#[test]
fn a_plain_text_file_is_not_a_package() {
    let env = Env::new("text");
    let p = env.file("notes.txt", b"hello");
    assert!(backend::pick(&p).is_none());
}
