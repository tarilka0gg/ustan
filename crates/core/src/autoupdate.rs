//! Looking for updates in the background: `ustan watch` checks every N hours and tells the user
//! (or, if enabled, installs). Schedulers (systemd user timer, cron) just run `ustan watch --once`.
use crate::{config, dirs::Dirs, discover, manifest::Manifest, update, Error, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    /// "name (version)" of what has an update and was not installed.
    pub available: Vec<String>,
    pub applied: Vec<String>,
    pub failed: Vec<String>,
}

/// Desktop notification through `notify-send` (or `$USTAN_NOTIFY_CMD`, which tests use).
pub fn notify(title: &str, body: &str) {
    let cmd = std::env::var("USTAN_NOTIFY_CMD").unwrap_or_else(|_| "notify-send".into());
    let _ = Command::new(cmd).args(["-a", "ustan", "-i", "system-software-update", title, body]).status();
}

fn notified_path(dirs: &Dirs) -> PathBuf {
    dirs.state.join("notified.toml")
}

/// Updates we already told the user about (`id = "version"`): no repeated popups for the same one.
fn load_notified(dirs: &Dirs) -> BTreeMap<String, String> {
    std::fs::read_to_string(notified_path(dirs)).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
}

fn save_notified(dirs: &Dirs, n: &BTreeMap<String, String>) {
    if let Ok(t) = toml::to_string(n) {
        let _ = std::fs::create_dir_all(&dirs.state);
        let _ = std::fs::write(notified_path(dirs), t);
    }
}

/// One round: look at every managed app and every AppImage found on disk.
pub fn run_once(dirs: &Dirs, apply: bool, notify_user: bool) -> Result<Summary> {
    let mut sum = Summary::default();
    let mut notified = load_notified(dirs);
    let mut fresh = Vec::new();

    for m in Manifest::list(&dirs.state)? {
        if let Ok(update::Status::Available(v)) = update::check(&m) {
            let line = format!("{} ({v})", m.name);
            if apply {
                match update::apply(&m, dirs) {
                    Ok(_) => sum.applied.push(line),
                    Err(e) => sum.failed.push(format!("{}: {e}", m.name)),
                }
            } else {
                if notified.get(&m.id) != Some(&v) {
                    fresh.push(line.clone());
                    notified.insert(m.id.clone(), v);
                }
                sum.available.push(line);
            }
        } else {
            notified.remove(&m.id);
        }
    }

    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    for f in discover::find_appimages(&home, &[dirs.opt.clone(), dirs.state.clone()]) {
        if let Ok(update::Status::Available(v)) = update::check_found(&f) {
            let line = format!("{} ({v})", f.name);
            let key = format!("found:{}", f.path.display());
            if apply {
                match update::apply_found(&f, dirs) {
                    Ok(_) => sum.applied.push(line),
                    Err(e) => sum.failed.push(format!("{}: {e}", f.name)),
                }
            } else {
                if notified.get(&key) != Some(&v) {
                    fresh.push(line.clone());
                    notified.insert(key, v);
                }
                sum.available.push(line);
            }
        }
    }
    save_notified(dirs, &notified);

    if notify_user {
        if !fresh.is_empty() {
            notify("Є оновлення", &format!("{}\nВідкрий ustan, щоб оновити.", fresh.join(", ")));
        }
        if !sum.applied.is_empty() {
            notify("Програми оновлено", &sum.applied.join(", "));
        }
        if !sum.failed.is_empty() {
            notify("Не вдалося оновити", &sum.failed.join("\n"));
        }
    }
    Ok(sum)
}

/// Check forever (or once) according to the saved settings. Never returns when `once` is false.
pub fn watch(dirs: &Dirs, once: bool) -> Result<()> {
    if !once {
        // let the desktop finish starting before the first (network) round
        std::thread::sleep(std::time::Duration::from_secs(120));
    }
    loop {
        let c = config::load(dirs).autoupdate;
        let _ = run_once(dirs, c.apply, true);
        if once {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(c.interval_hours.max(1) * 3600));
    }
}

// ---------- schedulers ----------

pub fn systemd_units(exe: &Path, hours: u64) -> (String, String) {
    (
        format!("[Unit]\nDescription=Check ustan apps for updates\n\n[Service]\nType=oneshot\nExecStart=\"{}\" watch --once\n", exe.display()),
        format!(
            "[Unit]\nDescription=Check ustan apps for updates\n\n[Timer]\nOnBootSec=5min\nOnUnitActiveSec={hours}h\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n"
        ),
    )
}

pub fn cron_line(exe: &Path, hours: u64) -> String {
    format!("0 */{} * * * \"{}\" watch --once # ustan-autoupdate", hours.clamp(1, 23), exe.display())
}

pub const NIRI_LINE: &str = "spawn-at-startup \"ustan\" \"watch\"";

/// Add (or remove) the `ustan watch` autostart line to a niri config, keeping a backup.
pub fn niri_set(config: &Path, enable: bool) -> Result<bool> {
    let text = std::fs::read_to_string(config)?;
    let has = text.lines().any(|l| l.trim() == NIRI_LINE);
    if has == enable {
        return Ok(false);
    }
    std::fs::copy(config, config.with_extension("kdl.pre-ustan-watch.bak"))?;
    let new = if enable {
        let mut lines: Vec<&str> = text.lines().collect();
        // next to the other startup entries when there are any, else at the end
        let at = lines.iter().rposition(|l| l.trim_start().starts_with("spawn-at-startup")).map_or(lines.len(), |i| i + 1);
        lines.insert(at, NIRI_LINE);
        lines.join("\n") + "\n"
    } else {
        text.lines().filter(|l| l.trim() != NIRI_LINE).collect::<Vec<_>>().join("\n") + "\n"
    };
    std::fs::write(config, new)?;
    Ok(true)
}

fn run_ok(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd).args(args).output().is_ok_and(|o| o.status.success())
}

/// Save the settings and set up the best scheduler available. Returns what was done / what to do next.
pub fn enable(dirs: &Dirs, apply: bool, hours: Option<u64>) -> Result<String> {
    let mut c = config::load(dirs);
    c.autoupdate.apply = apply;
    if let Some(h) = hours {
        c.autoupdate.interval_hours = h.max(1);
    }
    config::save(dirs, &c)?;
    let h = c.autoupdate.interval_hours;
    let exe = std::env::current_exe().map_err(Error::Io)?;
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());

    if run_ok("systemctl", &["--user", "show-environment"]) {
        let d = home.join(".config/systemd/user");
        std::fs::create_dir_all(&d)?;
        let (svc, timer) = systemd_units(&exe, h);
        std::fs::write(d.join("ustan-update.service"), svc)?;
        std::fs::write(d.join("ustan-update.timer"), timer)?;
        let _ = run_ok("systemctl", &["--user", "daemon-reload"]);
        if run_ok("systemctl", &["--user", "enable", "--now", "ustan-update.timer"]) {
            return Ok(format!("systemd-таймер ustan-update.timer: кожні {h} год"));
        }
    }
    if crate::backend::tree::which("crontab").is_some() {
        let old = Command::new("crontab").arg("-l").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        let kept: Vec<&str> = old.lines().filter(|l| !l.contains("# ustan-autoupdate")).collect();
        let new = format!("{}\n{}\n", kept.join("\n"), cron_line(&exe, h));
        let mut child = Command::new("crontab").arg("-").stdin(std::process::Stdio::piped()).spawn()?;
        use std::io::Write;
        child.stdin.take().unwrap().write_all(new.as_bytes())?;
        if child.wait()?.success() {
            return Ok(format!("cron: кожні {h} год"));
        }
    }
    Ok(format!(
        "налаштування збережено, але планувальника (systemd/cron) тут немає. Запускай `ustan watch` при вході, \
         наприклад у niri: додай рядок\n  {NIRI_LINE}\nабо виконай `ustan autoupdate enable --niri`"
    ))
}

pub fn disable(dirs: &Dirs) -> Result<String> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let mut did = Vec::new();
    if run_ok("systemctl", &["--user", "disable", "--now", "ustan-update.timer"]) {
        did.push("systemd-таймер вимкнено");
    }
    for f in ["ustan-update.service", "ustan-update.timer"] {
        let _ = std::fs::remove_file(home.join(".config/systemd/user").join(f));
    }
    if crate::backend::tree::which("crontab").is_some() {
        let old = Command::new("crontab").arg("-l").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        if old.contains("# ustan-autoupdate") {
            let kept: Vec<&str> = old.lines().filter(|l| !l.contains("# ustan-autoupdate")).collect();
            if let Ok(mut ch) = Command::new("crontab").arg("-").stdin(std::process::Stdio::piped()).spawn() {
                use std::io::Write;
                let _ = ch.stdin.take().unwrap().write_all((kept.join("\n") + "\n").as_bytes());
                let _ = ch.wait();
                did.push("запис cron прибрано");
            }
        }
    }
    let niri = home.join(".config/niri/config.kdl");
    if niri.exists() && niri_set(&niri, false).unwrap_or(false) {
        did.push("рядок `ustan watch` прибрано з конфігу niri");
    }
    let _ = dirs;
    Ok(if did.is_empty() { "нічого вимикати".into() } else { did.join("; ") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_and_cron_line() {
        let (svc, timer) = systemd_units(Path::new("/usr/bin/ustan"), 6);
        assert!(svc.contains("ExecStart=\"/usr/bin/ustan\" watch --once"));
        assert!(timer.contains("OnUnitActiveSec=6h") && timer.contains("timers.target"));
        assert_eq!(cron_line(Path::new("/usr/bin/ustan"), 12), "0 */12 * * * \"/usr/bin/ustan\" watch --once # ustan-autoupdate");
        assert!(cron_line(Path::new("/x"), 99).starts_with("0 */23 "), "hours are clamped to cron's range");
    }

    #[test]
    fn niri_line_is_added_next_to_other_startup_entries_and_removed_again() {
        let d = std::env::temp_dir().join(format!("ustan-niri-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let c = d.join("config.kdl");
        std::fs::write(&c, "input {}\nspawn-at-startup \"a\"\nspawn-at-startup \"b\"\nbinds {}\n").unwrap();
        assert!(niri_set(&c, true).unwrap());
        let t = std::fs::read_to_string(&c).unwrap();
        assert_eq!(t.lines().collect::<Vec<_>>(), ["input {}", "spawn-at-startup \"a\"", "spawn-at-startup \"b\"", NIRI_LINE, "binds {}"]);
        assert!(!niri_set(&c, true).unwrap(), "already there: nothing to do");
        assert!(c.with_extension("kdl.pre-ustan-watch.bak").exists());
        assert!(niri_set(&c, false).unwrap());
        assert!(!std::fs::read_to_string(&c).unwrap().contains("ustan"));
        let _ = std::fs::remove_dir_all(d);
    }
}
