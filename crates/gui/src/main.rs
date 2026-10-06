use adw::prelude::*;
use adw::{gdk, gio, glib, gtk};
use std::path::PathBuf;
use ustan_core::backend::{self, Info, Opts};
use ustan_core::progress::{self, Event};
use ustan_core::{dirs::Dirs, discover::{self, Found}, fetch, manifest::Manifest, update};

const APP_ID: &str = "io.github.tarilka0gg.Ustan";

/// First start after installing: make ustan the handler of its file types, once. A later
/// `ustan unregister` is remembered, so this never overrides the user's choice again.
fn register_on_first_start() {
    let dirs = Dirs::from_env();
    let mut cfg = ustan_core::config::load(&dirs);
    if cfg.registered {
        return;
    }
    let (Some(home), Ok(exe)) = (std::env::var_os("HOME").map(PathBuf::from), std::env::current_exe()) else { return };
    if ustan_core::register::register(&home, &exe, true, true).is_ok() {
        cfg.registered = true;
        let _ = ustan_core::config::save(&dirs, &cfg);
        ustan_core::autoupdate::notify("ustan тепер відкриває ці формати", "Пакети, AppImage, .exe/.msi, .jar та архіви. Повернути як було: ustan unregister");
    }
}

fn main() -> glib::ExitCode {
    std::thread::spawn(register_on_first_start);
    // Without a session bus GTK falls back to the program name for the Wayland app-id; keep it identical.
    glib::set_prgname(Some(APP_ID));
    let app = adw::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_OPEN).build();
    app.connect_activate(manager_window);
    app.connect_open(|app, files, _| {
        for f in files {
            let src = f.path().map(|p| p.display().to_string()).unwrap_or_else(|| f.uri().to_string());
            install_window(app, src);
        }
    });
    app.run()
}

// ---------- worker side (runs off the UI thread) ----------

struct Prepared {
    path: PathBuf,
    info: Info,
    downloaded: bool,
    /// Set when this app is already installed.
    existing: Option<Manifest>,
    /// Source URL and its HTTP validator, when downloaded.
    origin: Option<(String, Option<String>)>,
    /// Which checksum matched the download (None: nothing to compare against).
    verified: Option<&'static str>,
}

fn prepare(src: &str) -> Result<Prepared, String> {
    let dirs = Dirs::from_env();
    let (path, downloaded, origin, verified) = if fetch::is_url(src) {
        let label = src.rsplit('/').next().unwrap_or("файл").to_string();
        let got = fetch::download_auto(src, &dirs.state.join("cache"), &label).map_err(|e| e.to_string())?;
        (got.path, true, Some((src.to_string(), got.validator)), got.verified)
    } else {
        (PathBuf::from(src), false, None, None)
    };
    let b = backend::pick(&path).ok_or("Цей тип файлу не підтримується")?;
    let info = b.inspect(&path).map_err(|e| e.to_string())?;
    let existing = Manifest::load(&dirs.state, &info.id).ok();
    Ok(Prepared { path, info, downloaded, existing, origin, verified })
}

fn cleanup(p: &Prepared) {
    if p.downloaded {
        let _ = std::fs::remove_file(&p.path);
    }
}

/// Install; with `replace` the existing copy is removed first (reinstall).
fn run_install(p: &Prepared, installer: bool, replace: bool) -> Result<(String, Option<PathBuf>), String> {
    let dirs = Dirs::from_env();
    if let (true, Some(old)) = (replace, &p.existing) {
        old.uninstall(&dirs.state).map_err(|e| e.to_string())?;
    }
    let b = backend::pick(&p.path).ok_or("Цей тип файлу не підтримується")?;
    let mut m = b.install(&p.path, &dirs, &Opts { installer, ..Default::default() }).map_err(|e| e.to_string())?;
    if let Some((url, etag)) = &p.origin {
        m.url = Some(url.clone());
        m.etag = etag.clone();
        m.save(&dirs.state).map_err(|e| e.to_string())?;
    }
    cleanup(p);
    // notes (e.g. missing libraries) are shown under the name on the result page
    let mut text = m.notes.iter().fold(m.name.clone(), |acc, n| format!("{acc}\n⚠ {n}"));
    if p.origin.is_some() {
        match p.verified {
            Some(how) => text.push_str(&format!("\n✓ {how} перевірено")),
            None => text.push_str("\n⚠ Контрольної суми немає: завантаження не перевірено"),
        }
    }
    Ok((text, launcher_of(&m)))
}

fn run_remove(p: &Prepared) -> Result<(String, Option<PathBuf>), String> {
    let m = p.existing.as_ref().ok_or("Програма не встановлена")?;
    m.uninstall(&Dirs::from_env().state).map_err(|e| e.to_string())?;
    cleanup(p);
    Ok((m.name.clone(), None))
}

fn spawn<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> impl std::future::Future<Output = T> {
    let h = gio::spawn_blocking(f);
    async move { h.await.expect("worker thread panicked") }
}

// ---------- install dialog ----------

fn page(title: &str, desc: &str) -> adw::StatusPage {
    adw::StatusPage::builder().title(title).description(desc).build()
}

/// A page with a progress bar and a line of text, fed by [`follow_progress`].
struct Prog {
    page: adw::StatusPage,
    bar: gtk::ProgressBar,
    label: gtk::Label,
}

fn progress_page(title: &str, desc: &str) -> Prog {
    let page = page(title, desc);
    let bar = gtk::ProgressBar::builder().width_request(360).build();
    let label = gtk::Label::builder().css_classes(["dim-label"]).wrap(true).justify(gtk::Justification::Center).build();
    let col = gtk::Box::new(gtk::Orientation::Vertical, 10);
    col.append(&bar);
    col.append(&label);
    let clamp = adw::Clamp::builder().maximum_size(420).child(&col).build();
    page.set_child(Some(&clamp));
    Prog { page, bar, label }
}

/// Route the core's progress events into `p` until [`progress::clear_hook`] is called.
fn follow_progress(p: &Prog) {
    let (tx, rx) = async_channel::unbounded::<Event>();
    progress::set_hook(move |e| {
        let _ = tx.send_blocking(e);
    });
    let (bar, label) = (p.bar.clone(), p.label.clone());
    glib::spawn_future_local(async move {
        while let Ok(e) = rx.recv().await {
            match e {
                Event::Status(s) => {
                    label.set_text(&s);
                    bar.pulse();
                }
                Event::Download { name, done, total } => {
                    let mb = |b: u64| b as f64 / 1_048_576.0;
                    match total {
                        Some(t) if t > 0 => {
                            bar.set_fraction(done as f64 / t as f64);
                            label.set_text(&format!("Завантажую {name}: {:.1} з {:.1} МБ", mb(done), mb(t)));
                        }
                        _ => {
                            bar.pulse();
                            label.set_text(&format!("Завантажую {name}: {:.1} МБ", mb(done)));
                        }
                    }
                }
            }
        }
    });
}

#[allow(dead_code)]
fn spinner_page(title: &str, desc: &str) -> adw::StatusPage {
    let p = page(title, desc);
    p.set_child(Some(&adw::Spinner::new()));
    p
}

/// Animate `w`'s opacity (0 -> 1 fades in, 1 -> 0 fades out), optionally after a delay.
fn fade(w: &impl IsA<gtk::Widget>, from: f64, to: f64, ms: u32, delay_ms: u32, then: impl Fn() + 'static) {
    let w = w.clone().upcast::<gtk::Widget>();
    w.set_opacity(from);
    let start = move || {
        let target = adw::CallbackAnimationTarget::new({
            let w = w.clone();
            move |v| w.set_opacity(v)
        });
        let anim = adw::TimedAnimation::builder().widget(&w).value_from(from).value_to(to).duration(ms).easing(adw::Easing::EaseOutCubic).target(&target).build();
        anim.connect_done(move |_| then());
        anim.play();
    };
    if delay_ms == 0 {
        start();
    } else {
        glib::timeout_add_local_once(std::time::Duration::from_millis(delay_ms as u64), start);
    }
}

/// A green check mark that does not depend on the icon theme (some themes lack the symbolic icons).
fn ok_icon() -> Option<gdk::Texture> {
    const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96" viewBox="0 0 96 96"><circle cx="48" cy="48" r="44" fill="#33a76a"/><path d="M27 50l14 14 28-30" fill="none" stroke="#fff" stroke-width="9" stroke-linecap="round" stroke-linejoin="round"/></svg>"##;
    gdk::Texture::from_bytes(&glib::Bytes::from_static(SVG.as_bytes())).ok()
}

fn show_icon(sp: &adw::StatusPage, info: &Info) {
    if let Some(i) = &info.icon {
        let f = std::env::temp_dir().join(format!("ustan-icon-{}.{}", std::process::id(), i.ext));
        if std::fs::write(&f, &i.bytes).is_ok() {
            if let Ok(t) = gdk::Texture::from_file(&gio::File::for_path(&f)) {
                sp.set_paintable(Some(&t));
                return;
            }
        }
    }
    sp.set_icon_name(Some("package-x-generic"));
}

/// Bottom action bar: secondary actions on the left, the main one on the right.
struct Bar {
    revealer: gtk::Revealer,
    cancel: gtk::Button,
    remove: gtk::Button,
    main: gtk::Button,
    close: gtk::Button,
    /// "Open": start the app that was just installed.
    open: gtk::Button,
    /// "Unpack": for an archive that holds nothing to install.
    extract: gtk::Button,
    /// True while a job runs: the Cancel button then cancels it instead of closing the window.
    busy: std::cell::Cell<bool>,
}

impl Bar {
    fn new() -> Self {
        let btn = |label: &str, class: &str| gtk::Button::builder().label(label).css_classes([class]).margin_top(6).margin_bottom(6).build();
        let root = gtk::ActionBar::new();
        let cancel = btn("Скасувати", "flat");
        let remove = btn("Видалити", "destructive-action");
        let main = btn("Встановити", "suggested-action");
        let close = btn("Готово", "suggested-action");
        let open = btn("Відкрити", "suggested-action");
        let extract = btn("Розпакувати поруч", "suggested-action");
        root.pack_start(&cancel);
        root.pack_start(&remove);
        root.pack_end(&main);
        root.pack_end(&close);
        root.pack_end(&open);
        root.pack_end(&extract);
        let revealer = gtk::Revealer::builder().transition_type(gtk::RevealerTransitionType::SlideUp).transition_duration(260).child(&root).build();
        Bar { revealer, cancel, remove, main, close, open, extract, busy: std::cell::Cell::new(false) }
    }

    fn show_ready(&self, has_existing: bool) {
        self.busy.set(false);
        self.cancel.set_label("Скасувати");
        self.cancel.set_sensitive(true);
        self.revealer.set_reveal_child(true);
        self.cancel.set_visible(true);
        self.remove.set_visible(has_existing);
        self.main.set_visible(true);
        self.close.set_visible(false);
    }

    fn show_busy(&self, cancelable: bool) {
        self.busy.set(true);
        self.cancel.set_label("Скасувати");
        self.cancel.set_sensitive(true);
        self.cancel.set_visible(cancelable);
        for b in [&self.remove, &self.main, &self.close, &self.open, &self.extract] {
            b.set_visible(false);
        }
        self.revealer.set_reveal_child(cancelable);
    }

    fn show_end(&self, label: &str, ok: bool, can_open: bool) {
        self.busy.set(false);
        self.revealer.set_reveal_child(true);
        for b in [&self.cancel, &self.remove, &self.main] {
            b.set_visible(false);
        }
        self.close.set_label(if can_open { "Закрити" } else { label });
        self.close.set_css_classes(&[if ok && !can_open { "suggested-action" } else { "flat" }]);
        self.close.set_visible(true);
        self.open.set_visible(can_open);
        self.extract.set_visible(false);
    }
}

type Opener = std::rc::Rc<std::cell::RefCell<Option<std::rc::Rc<dyn Fn()>>>>;

fn install_window(app: &adw::Application, src: String) {
    let win = adw::ApplicationWindow::builder().application(app).default_width(900).default_height(720).title("Встановлення").build();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    let bar = std::rc::Rc::new(Bar::new());
    view.add_bottom_bar(&bar.revealer);
    let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::SlideLeft).transition_duration(280).build();
    view.set_content(Some(&stack));
    win.set_content(Some(&view));
    let loading = progress_page("Читаю пакет…", &src);
    stack.add_named(&loading.page, Some("loading"));
    win.present();

    let w = win.clone();
    bar.cancel.connect_clicked({
        let (w, bar) = (w.clone(), bar.clone());
        move |_| {
            if bar.busy.get() {
                progress::cancel();
                bar.cancel.set_sensitive(false);
                bar.cancel.set_label("Скасовую…");
            } else {
                w.close()
            }
        }
    });
    // what the "Open" button does: start the installed app, open the file elsewhere, show the folder...
    let opener: Opener = Default::default();
    bar.open.connect_clicked({
        let (w, opener) = (w.clone(), opener.clone());
        move |_| {
            if let Some(f) = opener.borrow().as_ref() {
                f();
            }
            w.close();
        }
    });
    bar.close.connect_clicked(move |_| w.close());

    glib::spawn_future_local(async move {
        let src2 = src.clone();
        progress::reset();
        follow_progress(&loading);
        let prepared = spawn(move || prepare(&src2)).await;
        progress::clear_hook();
        match prepared {
            Err(e) => {
                // an archive with nothing to install is usually just an archive: offer its old handler
                let nothing_to_install = e.contains("не знайшов в архіві") || e.contains("не підтримується");
                let title = if nothing_to_install { "Тут немає чого встановлювати" } else { "Не вдалося відкрити файл" };
                let p = page(title, &e);
                p.set_icon_name(Some(if nothing_to_install { "dialog-information-symbolic" } else { "dialog-error-symbolic" }));
                stack.add_named(&p, Some("end"));
                stack.set_visible_child_name("end");
                let elsewhere = if nothing_to_install { other_handler(&src) } else { None };
                if let Some((desktop, name)) = elsewhere.clone() {
                    let file = PathBuf::from(&src);
                    *opener.borrow_mut() = Some(std::rc::Rc::new(move || {
                        launch_with(&desktop, Some(&file));
                    }));
                    bar.open.set_label(&format!("Відкрити в «{name}»"));
                }
                bar.show_end("Закрити", false, elsewhere.is_some());
                // an archive that is only an archive: ustan can unpack it itself
                if nothing_to_install && is_unpackable(&src) {
                    bar.extract.set_visible(true);
                    let (stack, bar2, opener) = (stack.clone(), bar.clone(), opener.clone());
                    let src2 = src.clone();
                    bar.extract.connect_clicked(move |b| {
                        b.set_sensitive(false);
                        let (stack, bar, opener, src) = (stack.clone(), bar2.clone(), opener.clone(), src2.clone());
                        glib::spawn_future_local(async move {
                            let dest = unpack_dir(&src);
                            let (s2, d2) = (src.clone(), dest.clone());
                            let r = spawn(move || ustan_core::backend::archive::extract_to(std::path::Path::new(&s2), &d2).map_err(|e| e.to_string())).await;
                            let end = match r {
                                Ok(()) => {
                                    let p = page("Розпаковано", &dest.display().to_string());
                                    p.set_paintable(ok_icon().as_ref());
                                    let d = dest.clone();
                                    *opener.borrow_mut() = Some(std::rc::Rc::new(move || {
                                        open_folder(&d);
                                    }));
                                    bar.open.set_label("Відкрити папку");
                                    p
                                }
                                Err(e) => {
                                    *opener.borrow_mut() = None;
                                    let p = page("Не вдалося розпакувати", &e);
                                    p.set_icon_name(Some("dialog-error-symbolic"));
                                    p
                                }
                            };
                            let opened = opener.borrow().is_some();
                            if let Some(old) = stack.child_by_name("unpacked") {
                                stack.remove(&old);
                            }
                            stack.add_named(&end, Some("unpacked"));
                            stack.set_visible_child_name("unpacked");
                            bar.show_end("Закрити", false, opened);
                        });
                    });
                    if std::env::var_os("USTAN_GUI_AUTOSTART").is_some() {
                        bar.extract.emit_clicked(); // test hook, see below
                    }
                }
            }
            Ok(prep) => {
                let prep = std::sync::Arc::new(prep);
                let existing = prep.existing.is_some();
                let ready = match &prep.existing {
                    Some(old) => {
                        let new_v = prep.info.version.as_deref().unwrap_or("—");
                        let old_v = old.version.as_deref().unwrap_or("—");
                        page(&prep.info.name, &format!("Уже встановлено · {old_v}\nУ файлі · {new_v}"))
                    }
                    None => page(&prep.info.name, &describe(&prep.info)),
                };
                if let Some(w) = &prep.info.warning {
                    ready.set_description(Some(&format!("{}\n⚠ {w}", ready.description().unwrap_or_default())));
                }
                show_icon(&ready, &prep.info);

                let installer = adw::SwitchRow::builder().title("Це програма-установник").subtitle("Запустити через Wine і створити ярлики").build();
                if prep.info.kind == "exe" {
                    let n = prep.info.name.to_lowercase();
                    installer.set_active(n.contains("setup") || n.contains("install"));
                    let g = adw::PreferencesGroup::new();
                    g.add(&installer);
                    let clamp = adw::Clamp::builder().maximum_size(380).child(&g).build();
                    ready.set_child(Some(&clamp));
                }
                stack.add_named(&ready, Some("ready"));
                stack.set_visible_child_name("ready");
                bar.main.set_label(if existing { "Перевстановити" } else { "Встановити" });
                bar.show_ready(existing);

                // Run `job` off-thread, then show a result page.
                let run = std::rc::Rc::new({
                    let (stack, prep, bar, opener) = (stack.clone(), prep.clone(), bar.clone(), opener.clone());
                    move |job: Box<dyn FnOnce(&Prepared) -> Result<(String, Option<PathBuf>), String> + Send>, ok_title: &'static str, err_title: &'static str, cancelable: bool| {
                        let prog = progress_page("Зачекай…", &prep.info.name);
                        if let Some(old) = stack.child_by_name("busy") {
                            stack.remove(&old);
                        }
                        stack.add_named(&prog.page, Some("busy"));
                        stack.set_visible_child_name("busy");
                        bar.show_busy(cancelable);
                        progress::reset();
                        follow_progress(&prog);
                        let (stack, prep, bar, opener) = (stack.clone(), prep.clone(), bar.clone(), opener.clone());
                        glib::spawn_future_local(async move {
                            let r = spawn({
                                let prep = prep.clone();
                                move || job(&prep)
                            })
                            .await;
                            progress::clear_hook();
                            let ok = r.is_ok();
                            let launcher = r.as_ref().ok().and_then(|(_, l)| l.clone());
                            let end = match r {
                                Err(e) if e == "скасовано" => {
                                    let p = page("Скасовано", "Нічого не змінено");
                                    p.set_icon_name(Some("dialog-information-symbolic"));
                                    p
                                }
                                Ok((name, _)) => {
                                    let p = page(ok_title, &name);
                                    match ok_icon() {
                                        Some(t) => p.set_paintable(Some(&t)),
                                        None => p.set_icon_name(Some("object-select-symbolic")),
                                    }
                                    p
                                }
                                Err(e) => {
                                    let p = page(err_title, &e);
                                    p.set_icon_name(Some("dialog-error-symbolic"));
                                    p
                                }
                            };
                            stack.add_named(&end, Some("end"));
                            stack.set_visible_child_name("end");
                            *opener.borrow_mut() = launcher.clone().map(|d| std::rc::Rc::new(move || {
                                launch(&d);
                            }) as std::rc::Rc<dyn Fn()>);
                            bar.show_end(if ok { "Готово" } else { "Закрити" }, ok, launcher.is_some());
                        });
                    }
                });
                {
                    let run = run.clone();
                    let installer = installer.clone();
                    bar.main.connect_clicked(move |_| {
                        let inst = installer.is_active();
                        run(Box::new(move |p| run_install(p, inst, existing)), if existing { "Перевстановлено" } else { "Встановлено" }, "Не вдалося встановити", !existing);
                    });
                }
                bar.remove.connect_clicked(move |_| run(Box::new(run_remove), "Видалено", "Не вдалося видалити", false));

                // Test hooks (used by automated checks; harmless when the variables are unset):
                // press "Install" by itself, and optionally press "Cancel" after N milliseconds.
                if std::env::var_os("USTAN_GUI_AUTOSTART").is_some() {
                    bar.main.emit_clicked();
                    if let Some(ms) = std::env::var("USTAN_GUI_AUTOCANCEL_MS").ok().and_then(|v| v.parse::<u64>().ok()) {
                        let bar = bar.clone();
                        glib::timeout_add_local_once(std::time::Duration::from_millis(ms), move || bar.cancel.emit_clicked());
                    }
                }
            }
        }
    });
}

fn describe(i: &Info) -> String {
    match &i.version {
        Some(v) => format!("{} · {v}", i.kind),
        None => i.kind.to_string(),
    }
}

// ---------- manager window ----------

fn manager_window(app: &adw::Application) {
    let win = adw::ApplicationWindow::builder().application(app).default_width(900).default_height(720).title("Ustan").build();
    let view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    let open = gtk::Button::builder().icon_name("document-open-symbolic").tooltip_text("Встановити з файлу…").build();
    header.pack_start(&open);
    let recheck = gtk::Button::builder().icon_name("view-refresh-symbolic").tooltip_text("Перевірити оновлення").build();
    header.pack_end(&recheck);
    view.add_top_bar(&header);
    let bin = adw::Bin::new();
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&gtk::ScrolledWindow::builder().child(&bin).hscrollbar_policy(gtk::PolicyType::Never).build()));
    view.set_content(Some(&toasts));
    win.set_content(Some(&view));
    refresh(&bin, &toasts, true, true);
    // Installs/removals happen in other windows: re-read the list whenever this one is focused again.
    win.connect_is_active_notify({
        let (bin, toasts) = (bin.clone(), toasts.clone());
        move |w| {
            if w.is_active() {
                refresh(&bin, &toasts, false, false);
            }
        }
    });

    recheck.connect_clicked({
        let (bin, toasts) = (bin.clone(), toasts.clone());
        move |_| {
            UPDATES.with(|u| u.borrow_mut().clear());
            FOUND_UPD.with(|u| u.borrow_mut().clear());
            FOUND.with(|c| *c.borrow_mut() = None);
            toasts.add_toast(adw::Toast::new("Перевіряю оновлення…"));
            refresh(&bin, &toasts, false, true);
        }
    });
    let (a2, w2) = (app.clone(), win.clone());
    open.connect_clicked(move |_| pick_file(&a2, &w2));
    win.present();
    // Launched without a file: go straight to the file chooser; cancelling leaves the manager.
    pick_file(app, &win);
}

fn pick_file(app: &adw::Application, parent: &adw::ApplicationWindow) {
    let (app, parent) = (app.clone(), parent.clone());
    glib::spawn_future_local(async move {
        if let Ok(f) = gtk::FileDialog::new().open_future(Some(&parent)).await {
            if let Some(p) = f.path() {
                install_window(&app, p.display().to_string());
            }
        }
    });
}

thread_local! {
    /// Unmanaged AppImages found on disk (None = not scanned yet) and the updates found for them.
    static FOUND: std::cell::RefCell<Option<Vec<Found>>> = Default::default();
    static FOUND_UPD: std::cell::RefCell<std::collections::HashMap<PathBuf, String>> = Default::default();
    /// id -> label of the update found by the last check (kept across list rebuilds, so focus
    /// changes don't re-query GitHub and don't lose the badges).
    static UPDATES: std::cell::RefCell<std::collections::HashMap<String, String>> = Default::default();
}

fn add_update_button(actions: &gtk::Box, row: &adw::ActionRow, m: &Manifest, label: &str, bin: &adw::Bin, toasts: &adw::ToastOverlay) {
    row.set_subtitle(&format!("{} · {} · є оновлення: {label}", m.kind, m.version.as_deref().unwrap_or("—")));
    let upd = gtk::Button::builder().label("Оновити").valign(gtk::Align::Center).css_classes(["suggested-action"]).build();
    actions.prepend(&upd);
    let (m, bin, toasts) = (m.clone(), bin.clone(), toasts.clone());
    upd.connect_clicked(move |b| {
        b.set_sensitive(false);
        b.set_label("Оновлюю…");
        let (m, bin, toasts) = (m.clone(), bin.clone(), toasts.clone());
        glib::spawn_future_local(async move {
            let m2 = m.clone();
            let r = spawn(move || update::apply(&m2, &Dirs::from_env()).map_err(|e| e.to_string())).await;
            toasts.add_toast(adw::Toast::new(&match &r {
                Ok(_) => format!("{} оновлено", m.name),
                Err(e) => format!("Не вдалося оновити: {e}"),
            }));
            if r.is_ok() {
                UPDATES.with(|u| u.borrow_mut().remove(&m.id));
            }
            refresh(&bin, &toasts, false, false);
        });
    });
}

/// Rebuild the list. `check` queries the update sources in the background.
fn refresh(bin: &adw::Bin, toasts: &adw::ToastOverlay, animate: bool, check: bool) {
    let state = Dirs::from_env().state;
    let (runtimes, apps): (Vec<Manifest>, Vec<Manifest>) = Manifest::list(&state).unwrap_or_default().into_iter().partition(|m| m.runtime);
    UPDATES.with(|u| u.borrow_mut().retain(|id, _| apps.iter().any(|m| &m.id == id)));
    let group = adw::PreferencesGroup::builder().title("Встановлені програми").margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
    if apps.is_empty() {
        group.set_description(Some("Поки нічого. Відкрий .deb, .AppImage, .exe чи .flatpakref подвійним кліком або кнопкою зверху."));
    }
    for (i, m) in apps.into_iter().enumerate() {
        let row = adw::ActionRow::builder().title(&m.name).subtitle(format!("{} · {}", m.kind, m.version.as_deref().unwrap_or("—"))).build();
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let del = gtk::Button::builder().label("Видалити").valign(gtk::Align::Center).css_classes(["destructive-action"]).build();
        if let Some(d) = launcher_of(&m) {
            let img = match icon_file(&d) {
                Some(p) => gtk::Image::from_file(p),
                None => gtk::Image::from_icon_name("application-x-executable"),
            };
            img.set_pixel_size(36);
            row.add_prefix(&img);
            let run = gtk::Button::builder().label("Запустити").valign(gtk::Align::Center).build();
            let toasts = toasts.clone();
            run.connect_clicked(move |_| launch_watched(&d, &toasts));
            actions.append(&run);
        }
        actions.append(&del);
        row.add_suffix(&actions);

        if let Some(label) = UPDATES.with(|u| u.borrow().get(&m.id).cloned()) {
            add_update_button(&actions, &row, &m, &label, bin, toasts);
        } else if check {
            let (actions, row, m, bin, toasts) = (actions.clone(), row.clone(), m.clone(), bin.clone(), toasts.clone());
            glib::spawn_future_local(async move {
                let m2 = m.clone();
                if let Ok(update::Status::Available(label)) = spawn(move || update::check(&m2)).await {
                    UPDATES.with(|u| u.borrow_mut().insert(m.id.clone(), label.clone()));
                    add_update_button(&actions, &row, &m, &label, &bin, &toasts);
                }
            });
        }

        let (bin, toasts, id, row2) = (bin.clone(), toasts.clone(), m.id.clone(), row.clone());
        del.connect_clicked(move |b| {
            b.set_sensitive(false);
            let (bin, toasts, id, row) = (bin.clone(), toasts.clone(), id.clone(), row2.clone());
            glib::spawn_future_local(async move {
                let id2 = id.clone();
                let r = spawn(move || {
                    let d = Dirs::from_env().state;
                    Manifest::load(&d, &id2).and_then(|m| m.uninstall(&d)).map_err(|e| e.to_string())
                })
                .await;
                toasts.add_toast(adw::Toast::new(&match r {
                    Ok(()) => format!("{id} видалено"),
                    Err(e) => format!("Помилка: {e}"),
                }));
                fade(&row, 1.0, 0.0, 220, 0, move || refresh(&bin, &toasts, false, false));
            });
        });
        if animate {
            fade(&row, 0.0, 1.0, 380, 60 * i as u32, || {});
        }
        group.add(&row);
    }
    let found = adw::PreferencesGroup::builder()
        .title("Знайдено на диску")
        .description("AppImage, які лежать поза ustan. Оновлюються на місці.")
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    fill_found(&found, bin, toasts, check);
    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.append(&group);
    if !runtimes.is_empty() {
        col.append(&runtime_group(&runtimes, bin, toasts));
    }
    col.append(&found);
    bin.set_child(Some(&col));
}

/// The menu entry (.desktop file) a manifest created, if any.
/// The app that handled this file's type before ustan took it over, and its display name.
fn other_handler(src: &str) -> Option<(PathBuf, String)> {
    let info = gio::File::for_path(src).query_info("standard::content-type", gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE).ok()?;
    let mime = info.content_type()?;
    let home = PathBuf::from(std::env::var_os("HOME")?);
    // who handled it before ustan (remembered by `register`), else any other app registered for the type
    let desktop = ustan_core::register::previous_handler(&home, &mime).or_else(|| {
        gio::AppInfo::all_for_type(&mime)
            .into_iter()
            .filter_map(|a| a.id())
            .filter(|id| !id.starts_with("io.github.tarilka0gg.Ustan"))
            .find_map(|id| ustan_core::register::find_desktop(&home, &id))
    })?;
    let name = std::fs::read_to_string(&desktop).ok()?.lines().find_map(|l| l.strip_prefix("Name=").map(str::to_string))?;
    Some((desktop, name))
}

/// Archive formats `extract_to` can unpack.
fn is_unpackable(src: &str) -> bool {
    let l = src.to_lowercase();
    [".zip", ".7z", ".tar", ".tar.gz", ".tgz", ".tar.xz", ".txz", ".tar.zst", ".tar.bz2", ".tbz2", ".jar"].iter().any(|e| l.ends_with(e))
}

/// Next to the archive, in a folder named like it: `holiday.zip` -> `holiday/`, `holiday-2/`, ...
fn unpack_dir(src: &str) -> PathBuf {
    let p = std::path::Path::new(src);
    let mut name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "archive".into());
    for e in [".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2", ".tgz", ".txz", ".tbz2", ".tar", ".zip", ".7z", ".jar"] {
        if let Some(s) = name.to_lowercase().strip_suffix(e).map(str::len) {
            name.truncate(s);
            break;
        }
    }
    let base = p.parent().unwrap_or(std::path::Path::new(".")).join(&name);
    let mut cand = base.clone();
    let mut n = 2;
    while cand.exists() {
        cand = base.with_file_name(format!("{name}-{n}"));
        n += 1;
    }
    cand
}

fn open_folder(dir: &std::path::Path) {
    let _ = std::process::Command::new("gio").arg("open").arg(dir).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

fn launcher_of(m: &Manifest) -> Option<PathBuf> {
    m.files.iter().find(|f| f.extension().is_some_and(|e| e == "desktop") && f.is_file()).cloned()
}

fn launch(desktop: &std::path::Path) -> bool {
    launch_with(desktop, None)
}

/// Start a launcher entry ourselves and tell the user when it dies right away: `gio launch` returns
/// at once and swallows the app's errors, so a broken program looked like a dead button.
fn launch_watched(desktop: &std::path::Path, toasts: &adw::ToastOverlay) {
    let Some(spec) = std::fs::read_to_string(desktop).ok().and_then(|t| ustan_core::desktop::launch_spec(&t)).filter(|s| !s.terminal) else {
        launch(desktop);
        return;
    };
    let log = Dirs::from_env().state.join("last-launch.log");
    let toasts = toasts.clone();
    glib::spawn_future_local(async move {
        if let Err(msg) = spawn(move || run_watched(&spec, &log)).await {
            let toast = adw::Toast::new(&msg);
            toast.set_timeout(10);
            toasts.add_toast(toast);
        }
    });
}

/// Run `spec` with its output in `log`; Err (with the last output lines) if it fails within 20 s.
fn run_watched(spec: &ustan_core::desktop::LaunchSpec, log: &std::path::Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let name = std::path::Path::new(&spec.argv[0]).file_name().map_or_else(|| spec.argv[0].clone(), |n| n.to_string_lossy().into_owned());
    let _ = std::fs::create_dir_all(log.parent().unwrap_or(log));
    let out = std::fs::File::create(log).map_err(|e| format!("журнал запуску: {e}"))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(&spec.argv[0]);
    cmd.args(&spec.argv[1..]).stdin(std::process::Stdio::null()).stdout(out).stderr(err).process_group(0);
    if let Some(d) = spec.cwd.as_ref().filter(|d| d.is_dir()) {
        cmd.current_dir(d);
    }
    let started = std::time::Instant::now();
    let mut child = cmd.spawn().map_err(|e| format!("«{name}» не запустилась: {e}"))?;
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() || started.elapsed() > std::time::Duration::from_secs(20) {
        return Ok(());
    }
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let tail: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).rev().take(3).collect();
    let tail = tail.into_iter().rev().collect::<Vec<_>>().join(" ");
    let tail: String = tail.chars().take(220).collect();
    let code = status.code().map_or_else(|| "сигналом".to_string(), |c| format!("з кодом {c}"));
    Err(if tail.is_empty() { format!("«{name}» завершилась {code}") } else { format!("«{name}» завершилась {code}: {tail}") })
}

/// `gio launch` starts the entry exactly like the menu does (Exec, Path, env, field codes);
/// with `file` it opens that file in the app.
fn launch_with(desktop: &std::path::Path, file: Option<&std::path::Path>) -> bool {
    std::process::Command::new("gio").arg("launch").arg(desktop).args(file).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().is_ok()
}

/// The `Icon=` of a .desktop file when it is a plain file path.
fn icon_file(desktop: &std::path::Path) -> Option<PathBuf> {
    let t = std::fs::read_to_string(desktop).ok()?;
    let p = PathBuf::from(t.lines().find_map(|l| l.strip_prefix("Icon="))?.trim());
    p.is_absolute().then_some(p).filter(|p| p.is_file())
}

fn human(b: u64) -> String {
    if b >= 1 << 30 { format!("{:.1} ГБ", b as f64 / (1u64 << 30) as f64) } else { format!("{:.0} МБ", b as f64 / (1u64 << 20) as f64) }
}

/// Base/content snaps that were pulled in for other apps: sizes, who uses them, and a prune button.
fn runtime_group(runtimes: &[Manifest], bin: &adw::Bin, toasts: &adw::ToastOverlay) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Runtime-снапи")
        .description("Підтягнуті автоматично для інших програм. Те, що ніхто не використовує, можна прибрати.")
        .margin_bottom(18)
        .margin_start(18)
        .margin_end(18)
        .build();
    let any_unused = runtimes.iter().any(|m| m.used_by.is_empty());
    let prune = gtk::Button::builder().label("Прибрати невикористані").valign(gtk::Align::Center).sensitive(any_unused).build();
    group.set_header_suffix(Some(&prune));
    {
        let (bin, toasts) = (bin.clone(), toasts.clone());
        prune.connect_clicked(move |b| {
            b.set_sensitive(false);
            let (bin, toasts) = (bin.clone(), toasts.clone());
            glib::spawn_future_local(async move {
                let r = spawn(|| {
                    let d = Dirs::from_env().state;
                    let unused = Manifest::unused_runtimes(&d).map_err(|e| e.to_string())?;
                    let freed: u64 = unused.iter().map(|m| m.size()).sum();
                    for m in &unused {
                        m.uninstall(&d).map_err(|e| e.to_string())?;
                    }
                    Ok::<_, String>((unused.len(), freed))
                })
                .await;
                toasts.add_toast(adw::Toast::new(&match r {
                    Ok((0, _)) => "Нічого прибирати".to_string(),
                    Ok((n, freed)) => format!("Прибрано {n}, звільнено {}", human(freed)),
                    Err(e) => format!("Помилка: {e}"),
                }));
                refresh(&bin, &toasts, false, false);
            });
        });
    }
    for m in runtimes {
        let users = if m.used_by.is_empty() { "не використовується".to_string() } else { format!("використовують: {}", m.used_by.join(", ")) };
        let row = adw::ActionRow::builder().title(&m.name).subtitle(&users).build();
        let del = gtk::Button::builder().label("Видалити").valign(gtk::Align::Center).css_classes(["destructive-action"]).sensitive(m.used_by.is_empty()).build();
        row.add_suffix(&del);
        group.add(&row);
        // the size needs a walk over a big tree: do it off the UI thread
        {
            let (row, m2) = (row.clone(), m.clone());
            glib::spawn_future_local(async move {
                let size = spawn(move || m2.size()).await;
                let cur = row.subtitle().unwrap_or_default();
                row.set_subtitle(&format!("{} · {cur}", human(size)));
            });
        }
        let (bin, toasts, id) = (bin.clone(), toasts.clone(), m.id.clone());
        del.connect_clicked(move |b| {
            b.set_sensitive(false);
            let (bin, toasts, id) = (bin.clone(), toasts.clone(), id.clone());
            glib::spawn_future_local(async move {
                let id2 = id.clone();
                let r = spawn(move || {
                    let d = Dirs::from_env().state;
                    Manifest::load(&d, &id2).and_then(|m| m.uninstall(&d)).map_err(|e| e.to_string())
                })
                .await;
                toasts.add_toast(adw::Toast::new(&match r {
                    Ok(()) => format!("{id} видалено"),
                    Err(e) => format!("Помилка: {e}"),
                }));
                refresh(&bin, &toasts, false, false);
            });
        });
    }
    group
}

fn found_row(group: &adw::PreferencesGroup, f: &Found, check: bool, bin: &adw::Bin, toasts: &adw::ToastOverlay) {
    let ver = f.version.as_deref().unwrap_or("—");
    let row = adw::ActionRow::builder().title(&f.name).subtitle(format!("{ver} · {}", f.path.display())).subtitle_lines(1).build();
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_suffix(&actions);
    group.add(&row);
    // "Add": open the usual install dialog for this file (copies it under ustan, makes a launcher)
    {
        let add = gtk::Button::builder().label("Додати").tooltip_text("Встановити під керування ustan").valign(gtk::Align::Center).build();
        let path = f.path.display().to_string();
        add.connect_clicked(move |_| {
            if let Some(app) = gio::Application::default().and_then(|a| a.downcast::<adw::Application>().ok()) {
                install_window(&app, path.clone());
            }
        });
        actions.append(&add);
    }

    let show = {
        let (actions, row, f, bin, toasts) = (actions.clone(), row.clone(), f.clone(), bin.clone(), toasts.clone());
        move |label: String| {
            row.set_subtitle(&format!("{} · є оновлення: {label} · {}", f.version.as_deref().unwrap_or("—"), f.path.display()));
            let upd = gtk::Button::builder().label("Оновити").valign(gtk::Align::Center).css_classes(["suggested-action"]).build();
            actions.append(&upd);
            let (f, bin, toasts) = (f.clone(), bin.clone(), toasts.clone());
            upd.connect_clicked(move |b| {
                b.set_sensitive(false);
                b.set_label("Оновлюю…");
                let (f, bin, toasts) = (f.clone(), bin.clone(), toasts.clone());
                glib::spawn_future_local(async move {
                    let f2 = f.clone();
                    let r = spawn(move || update::apply_found(&f2, &Dirs::from_env()).map_err(|e| e.to_string())).await;
                    toasts.add_toast(adw::Toast::new(&match &r {
                        Ok(_) => format!("{} оновлено", f.name),
                        Err(e) => format!("Не вдалося оновити: {e}"),
                    }));
                    FOUND_UPD.with(|u| u.borrow_mut().remove(&f.path));
                    FOUND.with(|c| *c.borrow_mut() = None); // versions changed: rescan
                    refresh(&bin, &toasts, false, false);
                });
            });
        }
    };

    if let Some(label) = FOUND_UPD.with(|u| u.borrow().get(&f.path).cloned()) {
        show(label);
    } else if check && f.update_info.is_some() {
        let f = f.clone();
        glib::spawn_future_local(async move {
            let f2 = f.clone();
            if let Ok(update::Status::Available(label)) = spawn(move || update::check_found(&f2)).await {
                FOUND_UPD.with(|u| u.borrow_mut().insert(f.path.clone(), label.clone()));
                show(label);
            }
        });
    }
}

/// Fill the "found on disk" group from the cache, scanning in the background when needed.
fn fill_found(group: &adw::PreferencesGroup, bin: &adw::Bin, toasts: &adw::ToastOverlay, check: bool) {
    if let Some(list) = FOUND.with(|c| c.borrow().clone()) {
        for f in &list {
            found_row(group, f, check, bin, toasts);
        }
        group.set_visible(!list.is_empty());
        return;
    }
    let loading = adw::ActionRow::builder().title("Шукаю по диску…").build();
    loading.add_prefix(&adw::Spinner::new());
    group.add(&loading);
    let (group, bin, toasts) = (group.clone(), bin.clone(), toasts.clone());
    glib::spawn_future_local(async move {
        let list = spawn(|| {
            let d = Dirs::from_env();
            let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
            discover::find_appimages(&home, &[d.opt, d.state])
        })
        .await;
        group.remove(&loading);
        FOUND.with(|c| *c.borrow_mut() = Some(list.clone()));
        for f in &list {
            found_row(&group, f, true, &bin, &toasts);
        }
        group.set_visible(!list.is_empty());
    });
}

#[cfg(test)]
mod launch_tests {
    use super::*;
    use ustan_core::desktop::LaunchSpec;

    fn spec(script: &str) -> LaunchSpec {
        LaunchSpec { argv: vec!["sh".into(), "-c".into(), script.into()], cwd: None, terminal: false }
    }

    #[test]
    fn a_program_that_dies_right_away_is_reported_with_its_output() {
        let log = std::env::temp_dir().join(format!("ustan-launch-{}.log", std::process::id()));
        let msg = run_watched(&spec("echo first >&2; echo libfuse.so.2: cannot open >&2; exit 3"), &log).unwrap_err();
        assert!(msg.contains("з кодом 3") && msg.contains("libfuse.so.2"), "{msg}");
        assert!(run_watched(&spec("exit 0"), &log).is_ok());
        let missing = LaunchSpec { argv: vec!["/nonexistent/app".into()], cwd: None, terminal: false };
        assert!(run_watched(&missing, &log).unwrap_err().contains("не запустилась"));
        let _ = std::fs::remove_file(&log);
    }
}
