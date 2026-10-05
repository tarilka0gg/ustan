//! `.snap` packages: a squashfs with `meta/snap.yaml`. We unpack without snapd and generate
//! launchers that set up the `SNAP*` environment. Snaps that rely on other snaps (a `base`
//! or content runtimes such as gnome-*) can only run if they bundle what they need.
use super::tree::{which, dest, install_tree, remove_existing, safe_rel, Spec};
use std::path::PathBuf;
use super::{slug, Backend, Icon, Info, Opts};
use crate::{dirs::Dirs, manifest::Manifest, Error, Result};
use backhand::{FilesystemReader, InnerNode};
use std::io::{BufReader, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub struct Snap;

type Fs = FilesystemReader<'static>;

fn open(path: &Path) -> Result<Fs> {
    FilesystemReader::from_reader(BufReader::new(std::fs::File::open(path)?)).map_err(|e| Error::Format(format!("squashfs: {e}")))
}

fn read(fs: &Fs, rel: &str) -> Option<Vec<u8>> {
    let want = Path::new("/").join(rel);
    let n = fs.files().find(|n| n.fullpath == want)?;
    match &n.inner {
        InnerNode::File(f) => {
            let mut v = Vec::new();
            fs.file(f).reader().read_to_end(&mut v).ok()?;
            Some(v)
        }
        _ => None,
    }
}

#[derive(Debug, Clone)]
struct App {
    name: String,
    command: String,
    chain: Vec<String>,
    daemon: bool,
    env: Vec<(String, String)>,
}

/// A content-interface plug that wants a directory from another snap (snapd would bind-mount it).
#[derive(Debug, Clone)]
struct Plug {
    name: String,
    target: String,
    provider: String,
    slot: String,
}

#[derive(Debug)]
struct Meta {
    name: String,
    version: String,
    base: Option<String>,
    apps: Vec<App>,
    /// Other snaps this one expects to be connected (content interface default-providers).
    providers: Vec<String>,
    plugs: Vec<Plug>,
    /// snap.yaml `environment:`; snapd exports it to every app.
    env: Vec<(String, String)>,
}

fn env_of(v: Option<&serde_yaml::Value>) -> Vec<(String, String)> {
    let Some(m) = v.and_then(|v| v.as_mapping()) else { return vec![] };
    m.iter()
        .filter_map(|(k, v)| {
            let val = v.as_str().map(str::to_string).or_else(|| v.as_i64().map(|i| i.to_string())).or_else(|| v.as_bool().map(|b| b.to_string()))?;
            Some((k.as_str()?.to_string(), val))
        })
        // the value ends up inside a double-quoted shell string: refuse anything that could break out
        .filter(|(k, v)| k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && !v.contains('`') && !v.contains("$(") && !v.contains('"') && !v.contains('\\'))
        .collect()
}

fn parse_yaml(text: &str) -> Result<Meta> {
    let y: serde_yaml::Value = serde_yaml::from_str(text).map_err(|e| Error::Format(format!("snap.yaml: {e}")))?;
    let s = |k: &str| y.get(k).and_then(|v| v.as_str().map(str::to_string).or_else(|| v.as_f64().map(|f| f.to_string())));
    let name = s("name").ok_or_else(|| Error::Format("snap.yaml has no name".into()))?;
    let mut apps = Vec::new();
    if let Some(m) = y.get("apps").and_then(|a| a.as_mapping()) {
        for (k, v) in m {
            let (Some(an), Some(cmd)) = (k.as_str(), v.get("command").and_then(|c| c.as_str())) else { continue };
            let chain = v
                .get("command-chain")
                .and_then(|c| c.as_sequence())
                .map(|c| c.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            apps.push(App { name: an.to_string(), command: cmd.to_string(), chain, daemon: v.get("daemon").is_some(), env: env_of(v.get("environment")) });
        }
    }
    let mut plugs = Vec::new();
    if let Some(m) = y.get("plugs").and_then(|p| p.as_mapping()) {
        for (k, v) in m {
            let (Some(pn), Some(target), Some(dp)) = (k.as_str(), v.get("target").and_then(|t| t.as_str()), v.get("default-provider").and_then(|d| d.as_str())) else { continue };
            if v.get("interface").and_then(|i| i.as_str()) != Some("content") {
                continue;
            }
            // `snap` or `snap:slot`; without a slot name the slot is called like the plug.
            let (provider, slot) = dp.split_once(':').map_or((dp, pn), |(a, b)| (a, b));
            plugs.push(Plug { name: pn.to_string(), target: target.to_string(), provider: provider.to_string(), slot: slot.to_string() });
        }
    }
    let mut providers: Vec<String> = plugs.iter().map(|p| p.provider.clone()).collect();
    providers.sort();
    providers.dedup();
    Ok(Meta { name, version: s("version").unwrap_or_default(), base: s("base"), apps, providers, plugs, env: env_of(y.get("environment")) })
}

fn meta(fs: &Fs) -> Result<Meta> {
    let text = read(fs, "meta/snap.yaml").ok_or_else(|| Error::Format("no meta/snap.yaml: not a snap".into()))?;
    parse_yaml(&String::from_utf8_lossy(&text))
}

/// Make a command from snap.yaml runnable from our wrapper: relative paths live under $SNAP.
fn absolutize(c: &str) -> String {
    match c.split_whitespace().next() {
        Some(first) if !first.starts_with('/') && !first.starts_with('$') => format!("\"$SNAP\"/{c}"),
        _ => c.to_string(),
    }
}

fn wrapper(root: &Path, m: &Meta, app: &App, sandbox: Option<&Path>) -> String {
    let cmd = app.chain.iter().map(|c| absolutize(c)).chain([absolutize(&app.command)]).collect::<Vec<_>>().join(" ");
    let n = &m.name;
    let env: String = m.env.iter().chain(&app.env).map(|(k, v)| format!("export {k}=\"{v}\"\n")).collect();
    // Snaps that bring their own runtime also bring its glibc: it only works together with the base
    // snap's ld.so and tools, which snapd provides through a mount namespace. Emulate that with bwrap.
    let prelude = sandbox
        .map(|b| {
            format!(
                r#"if [ -z "$USTAN_SANDBOX" ]; then
  R="{base}"; UIDN=$(id -u)
  exec bwrap --unshare-pid --die-with-parent \
    --tmpfs / --ro-bind "$R/usr" /usr --symlink usr/bin /bin --symlink usr/lib /lib --symlink usr/lib64 /lib64 --symlink usr/sbin /sbin \
    --ro-bind "$R/etc" /etc --dev /dev --dev-bind-try /dev/dri /dev/dri --proc /proc --ro-bind-try /sys /sys \
    --tmpfs /tmp --bind-try /tmp/.X11-unix /tmp/.X11-unix \
    --bind "$HOME" "$HOME" --bind-try "/run/user/$UIDN" "/run/user/$UIDN" \
    --ro-bind-try /usr/share/fonts /usr/share/fonts \
    --ro-bind-try /etc/resolv.conf /run/systemd/resolve/stub-resolv.conf --ro-bind-try /etc/machine-id /etc/machine-id \
    --setenv USTAN_SANDBOX 1 /bin/sh "$0" "$@"
fi
"#,
                base = b.display()
            )
        })
        .unwrap_or_default();
    format!(
        r#"#!/bin/sh
# generated by ustan
{prelude}SNAP="{root}"
export SNAP SNAP_NAME="{n}" SNAP_INSTANCE_NAME="{n}" SNAP_VERSION="{v}" SNAP_ARCH=amd64 SNAP_REVISION=x1
export SNAP_DATA="$HOME/snap/{n}/current" SNAP_COMMON="$HOME/snap/{n}/common"
export SNAP_USER_DATA="$HOME/snap/{n}/current" SNAP_USER_COMMON="$HOME/snap/{n}/common"
export SNAP_REAL_HOME="$HOME" SNAP_LIBRARY_PATH=/var/lib/snapd/lib/gl
mkdir -p "$SNAP_USER_DATA" "$SNAP_USER_COMMON"
export LD_LIBRARY_PATH="$SNAP/lib:$SNAP/usr/lib:$SNAP/lib/x86_64-linux-gnu:$SNAP/usr/lib/x86_64-linux-gnu${{LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}}"
export PATH="$SNAP/.ustan-run/bin:$SNAP/usr/sbin:$SNAP/usr/bin:$SNAP/sbin:$SNAP/bin${{PATH:+:$PATH}}"
{env}exec {cmd} "$@"
"#,
        root = root.display(),
        v = m.version,
        prelude = prelude,
    )
}

fn extract(fs: &Fs, root: &Path) -> Result<()> {
    let canon = root.canonicalize()?;
    for n in fs.files() {
        let Some(rel) = safe_rel(&n.fullpath.to_string_lossy()) else { continue };
        let mode = (n.header.permissions as u32) & 0o777;
        match &n.inner {
            InnerNode::Dir(_) => {
                let d = dest(root, &canon, &rel)?;
                std::fs::create_dir_all(&d)?;
                std::fs::set_permissions(&d, std::fs::Permissions::from_mode(mode | 0o700))?;
            }
            InnerNode::File(f) => {
                let d = dest(root, &canon, &rel)?;
                remove_existing(&d);
                let mut out = std::fs::File::create(&d)?;
                std::io::copy(&mut fs.file(f).reader(), &mut out)?;
                std::fs::set_permissions(&d, std::fs::Permissions::from_mode(mode | 0o200))?;
            }
            InnerNode::Symlink(l) => {
                let d = dest(root, &canon, &rel)?;
                remove_existing(&d);
                std::os::unix::fs::symlink(&l.link, &d)?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Directories a provider snap offers for `slot`, as existing paths inside its root. `read` may sit
/// directly on the slot or under `source`; `/` and `$SNAP` mean the snap root, `$SNAP_DATA/..` (kernel
/// drivers, only present under snapd) is dropped.
fn slot_reads(provider_root: &Path, slot: &str) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(provider_root.join("meta/snap.yaml")) else { return vec![] };
    let Ok(y) = serde_yaml::from_str::<serde_yaml::Value>(&text) else { return vec![] };
    let Some(sl) = y.get("slots").and_then(|s| s.get(slot)) else { return vec![] };
    let list = sl.get("read").or_else(|| sl.get("source").and_then(|s| s.get("read"))).and_then(|r| r.as_sequence());
    list.map(|r| {
        r.iter()
            .filter_map(|p| p.as_str())
            .filter(|p| !p.contains("SNAP_DATA") && !p.contains("SNAP_COMMON"))
            .map(|p| {
                let rel = p.trim_start_matches("${SNAP}").trim_start_matches("$SNAP").trim_start_matches('/');
                if rel.is_empty() { provider_root.to_path_buf() } else { provider_root.join(rel) }
            })
            .filter(|p| p.exists())
            .collect()
    })
    .unwrap_or_default()
}

/// Download URL of the stable amd64 revision of a snap from the store.
fn store_url(name: &str) -> Result<String> {
    let resp = ureq::get(&format!("https://api.snapcraft.io/v2/snaps/info/{name}?fields=download"))
        .set("Snap-Device-Series", "16")
        .call()
        .map_err(|e| Error::Format(format!("store: {e}")))?;
    let j: serde_json::Value = resp.into_json().map_err(|e| Error::Format(format!("store json: {e}")))?;
    let maps = j["channel-map"].as_array().ok_or_else(|| Error::Format(format!("snap `{name}` not found in the store")))?;
    let pick = maps
        .iter()
        .find(|c| c["channel"]["architecture"] == "amd64" && c["channel"]["name"] == "stable")
        .or_else(|| maps.iter().find(|c| c["channel"]["architecture"] == "amd64"))
        .ok_or_else(|| Error::Format(format!("no amd64 build of `{name}`")))?;
    pick["download"]["url"].as_str().map(str::to_string).ok_or_else(|| Error::Format("store gave no download url".into()))
}

/// The provider snap, installed under ustan like any other app (downloaded first if needed).
fn ensure_provider(name: &str, dirs: &Dirs, opts: &Opts) -> Result<PathBuf> {
    let root = dirs.opt.join(slug(name));
    if root.join("meta/snap.yaml").exists() {
        return Ok(root);
    }
    eprintln!("завантажую runtime-снап `{name}` (може бути кілька сотень МБ)…");
    let file = crate::fetch::download(&store_url(name)?, &dirs.state.join("cache"), None)?;
    let file = {
        // the store URL has no extension; give it one so the snap backend recognises it
        let renamed = file.with_file_name(format!("{}.snap", slug(name)));
        std::fs::rename(&file, &renamed)?;
        renamed
    };
    let res = Snap.install(&file, dirs, opts);
    let _ = std::fs::remove_file(&file);
    res?;
    Ok(root)
}

/// Emulate snapd's content bind-mounts with symlinks: `target` -> directory of the provider.
fn link_plug(root: &Path, plug: &Plug, dirs: &Dirs, opts: &Opts) -> Result<()> {
    let provider = ensure_provider(&plug.provider, dirs, opts)?;
    let reads = slot_reads(&provider, &plug.slot);
    if reads.is_empty() {
        return Ok(()); // the provider offers nothing for this slot
    }
    let rel = plug.target.trim_start_matches("$SNAP").trim_start_matches("${SNAP}");
    let target = root.join(safe_rel(rel).ok_or_else(|| Error::Format(format!("unsafe plug target {}", plug.target)))?);
    if let Some(p) = target.parent() {
        std::fs::create_dir_all(p)?;
    }
    remove_existing(&target);
    // The snap root (or a single dir) *is* the target; several dirs go under target/<name>.
    if reads.len() == 1 || reads.iter().any(|r| r == &provider) {
        let src = reads.iter().find(|r| *r == &provider).unwrap_or(&reads[0]);
        std::os::unix::fs::symlink(src, &target)?;
    } else {
        // several source dirs: snapd mounts each under target/<name>
        std::fs::create_dir_all(&target)?;
        for r in &reads {
            if let Some(n) = r.file_name() {
                std::os::unix::fs::symlink(r, target.join(n))?;
            }
        }
    }
    Ok(())
}

fn warning(m: &Meta) -> Option<String> {
    (!m.providers.is_empty())
        .then(|| format!("потребує runtime-снапи ({}): ustan завантажить їх сам, якщо їх ще немає (можуть бути сотні МБ). Запускається в пісочниці bubblewrap, без snapd робота не гарантована", m.providers.join(", ")))
}

fn icon_of(fs: &Fs) -> Option<Icon> {
    for (n, ext) in [("meta/gui/icon.png", "png"), ("meta/gui/icon.svg", "svg"), ("meta/gui/icon.xpm", "xpm")] {
        if let Some(bytes) = read(fs, n) {
            return Some(Icon { ext, bytes });
        }
    }
    // Themed icon: take the name from the snap's launcher and find the file in the image.
    let name = fs
        .files()
        .filter(|n| n.fullpath.starts_with("/meta/gui") && n.fullpath.extension().is_some_and(|e| e == "desktop"))
        .find_map(|n| {
            let rel = n.fullpath.strip_prefix("/").ok()?.to_string_lossy().into_owned();
            let text = String::from_utf8(read(fs, &rel)?).ok()?;
            text.lines().find_map(|l| l.strip_prefix("Icon=")).map(str::to_string)
        })?;
    // Themed names are dotted (org.gnome.Calculator): only strip real image extensions.
    let stem = [".svg", ".png", ".xpm"].iter().find_map(|e| name.strip_suffix(e)).unwrap_or(&name).to_string();
    let best = fs
        .files()
        .filter(|n| matches!(n.inner, InnerNode::File(_)))
        .filter(|n| n.fullpath.file_stem().is_some_and(|s| s.to_string_lossy() == stem))
        .filter(|n| n.fullpath.extension().is_some_and(|e| e == "svg" || e == "png"))
        .max_by_key(|n| {
            let p = n.fullpath.to_string_lossy();
            // svg wins; otherwise the largest size directory (e.g. 256x256)
            if p.ends_with(".svg") { u32::MAX } else { p.split('/').filter_map(|c| c.split('x').next()?.parse::<u32>().ok()).max().unwrap_or(0) }
        })?;
    let rel = best.fullpath.strip_prefix("/").ok()?.to_string_lossy().into_owned();
    let ext = if rel.ends_with(".svg") { "svg" } else { "png" };
    Some(Icon { ext, bytes: read(fs, &rel)? })
}

impl Backend for Snap {
    fn kind(&self) -> &'static str {
        "snap"
    }

    fn detect(&self, path: &Path) -> bool {
        path.extension().is_some_and(|e| e.eq_ignore_ascii_case("snap"))
    }

    fn inspect(&self, path: &Path) -> Result<Info> {
        let fs = open(path)?;
        let m = meta(&fs)?;
        let warning = warning(&m);
        Ok(Info { id: slug(&m.name), name: m.name, version: Some(m.version), kind: "snap", icon: icon_of(&fs), warning })
    }

    fn install(&self, path: &Path, dirs: &Dirs, opts: &Opts) -> Result<Manifest> {
        let fs = open(path)?;
        let m = meta(&fs)?;
        let spec = Spec { kind: "snap", id: slug(&m.name), name: m.name.clone(), version: Some(m.version.clone()), source: path };
        install_tree(
            spec,
            dirs,
            opts,
            |root| extract(&fs, root),
            |root, dirs, id, files| {
                // Only our own launchers: drop the generic ones install_tree made from usr/share/applications.
                for f in files.drain(1..) {
                    let _ = std::fs::remove_file(f);
                }
                for plug in &m.plugs {
                    link_plug(root, plug, dirs, opts)?;
                }
                let sandbox = if m.providers.is_empty() {
                    None
                } else {
                    if which("bwrap").is_none() {
                        return Err(Error::Format("для цього snap потрібен bubblewrap (команда `bwrap`), його немає в PATH".into()));
                    }
                    match m.base.as_deref().filter(|b| *b != "bare") {
                        Some(b) => Some(ensure_provider(b, dirs, opts)?),
                        None => None,
                    }
                };
                let run = root.join(".ustan-run");
                std::fs::create_dir_all(run.join("bin"))?;
                // snapd isn't here: answer `snapctl is-connected <plug>` ourselves for the plugs we linked.
                let connected: Vec<&str> = m.plugs.iter().map(|p| p.name.as_str()).collect();
                let shim = run.join("bin/snapctl");
                std::fs::write(
                    &shim,
                    format!("#!/bin/sh\n# generated by ustan: stand-in for snapd's snapctl\ncase \"$1\" in\n  is-connected) case \"$2\" in {}) exit 0;; *) exit 1;; esac;;\n  *) exit 0;;\nesac\n", if connected.is_empty() { "__none__".to_string() } else { connected.join("|") }),
                )?;
                std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))?;
                std::fs::create_dir_all(&dirs.apps)?;
                let bin = dirs.opt.parent().unwrap_or(&dirs.opt).join("bin");
                for app in m.apps.iter().filter(|a| !a.daemon) {
                    let w = run.join(&app.name);
                    std::fs::write(&w, wrapper(root, &m, app, sandbox.as_deref()))?;
                    std::fs::set_permissions(&w, std::fs::Permissions::from_mode(0o755))?;

                    if let Ok(text) = std::fs::read_to_string(root.join("meta/gui").join(format!("{}.desktop", app.name))) {
                        // The snap's own launcher: swap its Exec for our wrapper and resolve ${SNAP} in Icon.
                        let mut out = String::new();
                        for l in text.lines() {
                            let l = l.replace("${SNAP}", &root.display().to_string());
                            match l.split_once('=') {
                                Some(("Icon", v)) if !v.starts_with('/') => match crate::desktop::find_icon_deep(root, v) {
                                    Some(p) => out.push_str(&format!("Icon={}\n", p.display())),
                                    None => {
                                        out.push_str(&l);
                                        out.push('\n');
                                    }
                                },
                                Some(("Exec", v)) => {
                                    let rest = v.split_once(char::is_whitespace).map_or("", |(_, r)| r);
                                    out.push_str(&format!("Exec=\"{}\" {rest}\n", w.display()));
                                }
                                _ => {
                                    out.push_str(&l);
                                    out.push('\n');
                                }
                            }
                        }
                        let dst = dirs.apps.join(format!("ustan-{id}-{}.desktop", app.name));
                        std::fs::write(&dst, out)?;
                        files.push(dst);
                    } else {
                        // A command-line app: expose it in PATH under the usual snap name.
                        std::fs::create_dir_all(&bin)?;
                        let name = if app.name == m.name { app.name.clone() } else { format!("{}.{}", m.name, app.name) };
                        let link = bin.join(name);
                        if std::fs::symlink_metadata(&link).is_err() {
                            std::os::unix::fs::symlink(&w, &link)?;
                            files.push(link);
                        }
                    }
                }
                Ok(())
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_snap_yaml() {
        let y = "name: demo\nversion: '1.5'\nbase: core22\napps:\n  demo:\n    command: bin/demo --x\n    command-chain: [snap/command-chain/launch]\n  svc:\n    command: bin/svc\n    daemon: simple\n";
        let m = parse_yaml(y).unwrap();
        assert_eq!((m.name.as_str(), m.version.as_str()), ("demo", "1.5"));
        assert_eq!(m.apps.len(), 2);
        assert!(m.apps[1].daemon);
        assert_eq!(m.apps[0].chain, vec!["snap/command-chain/launch".to_string()]);
    }

    #[test]
    fn commands_are_rooted_in_snap() {
        assert_eq!(absolutize("bin/hello"), "\"$SNAP\"/bin/hello");
        assert_eq!(absolutize("$SNAP/bin/x"), "$SNAP/bin/x");
        assert_eq!(absolutize("/usr/bin/env a"), "/usr/bin/env a");
    }
}
