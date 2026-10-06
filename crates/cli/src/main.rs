use clap::{Parser, Subcommand};
use std::path::PathBuf;
use ustan_core::{autoupdate, backend, discover, fetch, register, runner, update, dirs::Dirs, manifest::Manifest};

#[derive(Parser)]
#[command(name = "ustan", about = "Windows-style app installer for Linux")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show what a package is, without installing
    Inspect { file: String },
    /// Install a package into ~/.local/opt
    Install {
        /// File path, http(s) URL, or `flatpak:<app-id>`
        source: String,
        /// Expected sha256 (URLs only)
        #[arg(long)]
        sha256: Option<String>,
        /// Treat a Windows .exe as an installer: run it under Wine and create launchers
        #[arg(long)]
        installer: bool,
        /// Arguments for the Windows installer, e.g. "/S" (NSIS), "/VERYSILENT" (Inno), "/qn" (msi)
        #[arg(long, allow_hyphen_values = true)]
        installer_args: Option<String>,
    },
    /// List installed apps
    List,
    /// Make double-click on .deb/.AppImage/.flatpak* open ustan (needs ustan-gui next to this binary)
    Register {
        /// Also take over Windows .exe (replaces e.g. PortProton as default!)
        #[arg(long)]
        exe: bool,
        /// Also take over .tar.gz/.tar.xz/.tar.zst/.zip (replaces your archive manager as default!)
        #[arg(long)]
        archives: bool,
    },
    /// Undo `register`
    Unregister,
    /// Check for updates and install them (all apps, or just <id>)
    Update {
        id: Option<String>,
        /// Only report, don't install
        #[arg(long)]
        check: bool,
    },
    /// Find AppImages anywhere on disk (not installed by ustan) and check them for updates
    Scan {
        /// Also update the ones that have a newer release (replaced in place)
        #[arg(long)]
        update: bool,
    },
    /// Look for updates in the background: notify (or install, see `autoupdate`); runs until stopped
    Watch {
        /// One round, then exit (what a systemd timer or cron runs)
        #[arg(long)]
        once: bool,
    },
    /// Turn background update checks on or off
    Autoupdate {
        #[command(subcommand)]
        cmd: Option<AutoCmd>,
    },
    /// Show or choose the Wine used for Windows programs
    Runner {
        #[command(subcommand)]
        cmd: Option<RunnerCmd>,
    },
    /// Remove an installed app
    Remove {
        id: String,
        /// Remove a runtime even though apps still use it
        #[arg(long)]
        force: bool,
    },
    /// Remove runtimes (base/content snaps) that no app uses any more
    Prune {
        /// Only show what would be removed
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum AutoCmd {
    /// Start checking (sets up a systemd timer or cron when the system has one)
    Enable {
        /// Install updates by themselves instead of only notifying
        #[arg(long)]
        apply: bool,
        /// Hours between checks (default 12)
        #[arg(long)]
        interval: Option<u64>,
        /// Also start `ustan watch` at login by adding a line to the niri config
        #[arg(long)]
        niri: bool,
    },
    /// Stop checking and remove what `enable` set up
    Disable,
}

#[derive(Subcommand)]
enum RunnerCmd {
    /// Use this runner (a name from `ustan runner`, or a path to a wine binary)
    Use { name: String },
}

/// Print downloads as `name  42% (12.3 / 29.2 МБ)` on one line, other steps as their own line.
fn install_progress_hook() {
    use std::io::{IsTerminal, Write};
    use ustan_core::progress::{self, Event};
    if !std::io::stderr().is_terminal() {
        return;
    }
    progress::set_hook(|e| match e {
        Event::Status(s) => eprintln!("\r\x1b[2K{s}"),
        Event::Download { name, done, total } => {
            let mb = |b: u64| b as f64 / 1_048_576.0;
            match total {
                Some(t) if t > 0 => eprint!("\r\x1b[2K{name}  {:>3}% ({:.1} / {:.1} МБ)", done * 100 / t, mb(done), mb(t)),
                _ => eprint!("\r\x1b[2K{name}  {:.1} МБ", mb(done)),
            }
            let _ = std::io::stderr().flush();
        }
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    install_progress_hook();
    let dirs = Dirs::from_env();
    let dir = dirs.state.clone();
    match Cli::parse().cmd {
        Cmd::Inspect { file } => {
            let file = PathBuf::from(file);
            let b = backend::pick(&file).ok_or("unsupported package type")?;
            println!("{:#?}", b.inspect(&file)?);
        }
        Cmd::Install { source, sha256, installer, installer_args } => {
            let mut etag = None;
            let file = if fetch::is_url(&source) {
                let expect = match &sha256 {
                    Some(h) => fetch::Expect::Sha256(h.clone()),
                    None => fetch::sidecar_sha256(&source).map(fetch::Expect::Sha256).unwrap_or_default(),
                };
                let got = fetch::download_checked(&source, &dirs.state.join("cache"), &expect, &source.rsplit('/').next().unwrap_or("файл").to_string())?;
                eprintln!();
                match got.verified {
                    Some(how) => eprintln!("{how}: перевірено"),
                    None => eprintln!("увага: контрольної суми немає ({}.sha256 не знайдено), завантаження не перевірено", source.split(['?', '#']).next().unwrap_or(&source)),
                }
                etag = got.validator;
                got.path
            } else {
                PathBuf::from(&source)
            };
            let b = backend::pick(&file).ok_or("unsupported package type")?;
            if let Ok(info) = b.inspect(&file) {
                if let Some(w) = info.warning {
                    eprintln!("увага: {w}");
                }
            }
            let mut m = b.install(&file, &dirs, &backend::Opts { installer, installer_args: installer_args.as_deref().unwrap_or("").split_whitespace().map(String::from).collect() })?;
            if fetch::is_url(&source) {
                m.url = Some(source.clone());
                m.etag = etag;
                m.save(&dir)?;
                let _ = std::fs::remove_file(&file); // installed copy lives in ~/.local/opt
            }
            for n in &m.notes {
                eprintln!("увага: {n}");
            }
            println!("installed {} {}", m.id, m.version.as_deref().unwrap_or(""));
        }
        Cmd::Register { exe, archives } => {
            let gui = std::env::current_exe()?.with_file_name("ustan-gui");
            if !gui.exists() {
                return Err(format!("{} not found; build with `cargo build --workspace`", gui.display()).into());
            }
            let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME not set")?);
            register::register(&home, &gui, exe, archives)?;
            println!("registered {}{}", gui.display(), if exe { " (incl. .exe)" } else { "" });
        }
        Cmd::Unregister => {
            let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME not set")?);
            register::unregister(&home)?;
        }
        Cmd::Update { id, check } => {
            let apps: Vec<Manifest> = match id {
                Some(id) => vec![Manifest::load(&dir, &id)?],
                None => Manifest::list(&dir)?,
            };
            for m in apps {
                match update::check(&m) {
                    Ok(update::Status::Available(v)) if check => println!("{}\tє оновлення ({v})", m.id),
                    Ok(update::Status::Available(v)) => {
                        println!("{}\tоновлюю ({v})…", m.id);
                        match update::apply(&m, &dirs) {
                            Ok(_) => println!("{}\tоновлено", m.id),
                            Err(e) => println!("{}\tпомилка: {e}", m.id),
                        }
                    }
                    Ok(update::Status::UpToDate) => println!("{}\tактуальна", m.id),
                    Ok(update::Status::Unknown(why)) => println!("{}\t? {why}", m.id),
                    Err(e) => println!("{}\tне вдалося перевірити: {e}", m.id),
                }
            }
        }
        Cmd::Scan { update } => {
            let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME not set")?);
            for f in discover::find_appimages(&home, &[dirs.opt.clone(), dirs.state.clone()]) {
                let ver = f.version.as_deref().unwrap_or("-");
                match update::check_found(&f) {
                    Ok(update::Status::Available(v)) if update => {
                        println!("{}\t{ver}\t{}\tоновлюю ({v})…", f.name, f.path.display());
                        match update::apply_found(&f, &dirs) {
                            Ok(new) => println!("\t\t\tоновлено до {}", new.as_deref().unwrap_or("?")),
                            Err(e) => println!("\t\t\tпомилка: {e}"),
                        }
                    }
                    Ok(update::Status::Available(v)) => println!("{}\t{ver}\t{}\tє оновлення ({v})", f.name, f.path.display()),
                    Ok(update::Status::UpToDate) => println!("{}\t{ver}\t{}\tактуальна", f.name, f.path.display()),
                    Ok(update::Status::Unknown(why)) => println!("{}\t{ver}\t{}\t? {why}", f.name, f.path.display()),
                    Err(e) => println!("{}\t{ver}\t{}\tне вдалося перевірити: {e}", f.name, f.path.display()),
                }
            }
        }
        Cmd::Watch { once } => {
            if !once {
                eprintln!("слідкую за оновленнями (перша перевірка за 2 хв)…");
            }
            autoupdate::watch(&dirs, once)?;
        }
        Cmd::Autoupdate { cmd } => match cmd {
            None => {
                let c = ustan_core::config::load(&dirs).autoupdate;
                println!("перевірка кожні {} год; оновлення: {}", c.interval_hours, if c.apply { "встановлюються самі" } else { "лише сповіщення" });
            }
            Some(AutoCmd::Enable { apply, interval, niri }) => {
                println!("{}", autoupdate::enable(&dirs, apply, interval)?);
                if niri {
                    let cfg = PathBuf::from(std::env::var_os("HOME").ok_or("HOME not set")?).join(".config/niri/config.kdl");
                    if autoupdate::niri_set(&cfg, true)? {
                        println!("додано в {}: {}  (резервна копія поруч)", cfg.display(), autoupdate::NIRI_LINE);
                    } else {
                        println!("у конфігу niri цей рядок уже є");
                    }
                }
            }
            Some(AutoCmd::Disable) => println!("{}", autoupdate::disable(&dirs)?),
        },
        Cmd::Runner { cmd } => match cmd {
            None => {
                let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME not set")?);
                let current = runner::pick(&dirs, &home).map(|r| r.name);
                let all = runner::discover(&home);
                if all.is_empty() {
                    println!("Wine не знайдено: немає `wine` у PATH, PortProton чи Steam Proton");
                }
                for r in all {
                    println!("{} {}", if Some(&r.name) == current.as_ref() { "*" } else { " " }, r.name);
                }
            }
            Some(RunnerCmd::Use { name }) => {
                let r = runner::set(&dirs, &name)?;
                println!("тепер Windows-програми запускає: {}", r.name);
            }
        },
        Cmd::List => {
            for m in Manifest::list(&dir)? {
                let rt = if m.runtime { if m.used_by.is_empty() { "\t[runtime, не використовується]".to_string() } else { format!("\t[runtime: {}]", m.used_by.join(", ")) } } else { String::new() };
                println!("{}\t{}\t{}{rt}", m.id, m.version.as_deref().unwrap_or("-"), m.kind);
            }
        }
        Cmd::Remove { id, force } => {
            let m = Manifest::load(&dir, &id)?;
            if m.runtime && !m.used_by.is_empty() && !force {
                return Err(format!("`{id}` використовують: {}. Видалення їх зламає (--force, щоб усе одно видалити)", m.used_by.join(", ")).into());
            }
            m.uninstall(&dir)?;
        }
        Cmd::Prune { dry_run } => {
            let unused = Manifest::unused_runtimes(&dir)?;
            let mut total = 0u64;
            for m in &unused {
                let sz = m.size();
                total += sz;
                println!("{}\t{:.0} МБ{}", m.id, sz as f64 / 1_048_576.0, if dry_run { "\t(буде видалено)" } else { "" });
                if !dry_run {
                    m.uninstall(&dir)?;
                }
            }
            println!("{} {:.1} ГБ ({} шт.)", if dry_run { "можна звільнити" } else { "звільнено" }, total as f64 / 1_073_741_824.0, unused.len());
        }
    }
    Ok(())
}
