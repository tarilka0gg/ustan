use clap::{Parser, Subcommand};
use std::path::PathBuf;
use ustan_core::{backend, fetch, register, update, dirs::Dirs, manifest::Manifest};

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
    },
    /// List installed apps
    List,
    /// Make double-click on .deb/.AppImage/.flatpak* open ustan (needs ustan-gui next to this binary)
    Register {
        /// Also take over Windows .exe (replaces e.g. PortProton as default!)
        #[arg(long)]
        exe: bool,
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
    /// Remove an installed app
    Remove { id: String },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dirs = Dirs::from_env();
    let dir = dirs.state.clone();
    match Cli::parse().cmd {
        Cmd::Inspect { file } => {
            let file = PathBuf::from(file);
            let b = backend::pick(&file).ok_or("unsupported package type")?;
            println!("{:#?}", b.inspect(&file)?);
        }
        Cmd::Install { source, sha256, installer } => {
            let mut etag = None;
            let file = if fetch::is_url(&source) {
                let (f, e) = fetch::download_with_validator(&source, &dirs.state.join("cache"), sha256.as_deref())?;
                eprintln!("downloaded {}", f.display());
                etag = e;
                f
            } else {
                PathBuf::from(&source)
            };
            let b = backend::pick(&file).ok_or("unsupported package type")?;
            let mut m = b.install(&file, &dirs, &backend::Opts { installer })?;
            if fetch::is_url(&source) {
                m.url = Some(source.clone());
                m.etag = etag;
                m.save(&dir)?;
                let _ = std::fs::remove_file(&file); // installed copy lives in ~/.local/opt
            }
            println!("installed {} {}", m.id, m.version.as_deref().unwrap_or(""));
        }
        Cmd::Register { exe } => {
            let gui = std::env::current_exe()?.with_file_name("ustan-gui");
            if !gui.exists() {
                return Err(format!("{} not found; build with `cargo build --workspace`", gui.display()).into());
            }
            let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME not set")?);
            register::register(&home, &gui, exe)?;
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
        Cmd::List => {
            for m in Manifest::list(&dir)? {
                println!("{}\t{}\t{}", m.id, m.version.as_deref().unwrap_or("-"), m.kind);
            }
        }
        Cmd::Remove { id } => Manifest::load(&dir, &id)?.uninstall(&dir)?,
    }
    Ok(())
}
