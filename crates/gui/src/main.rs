use adw::prelude::*;
use adw::{gdk, gio, glib, gtk};
use std::path::PathBuf;
use ustan_core::backend::{self, Info, Opts};
use ustan_core::{dirs::Dirs, discover::{self, Found}, fetch, manifest::Manifest, update};

const APP_ID: &str = "io.github.tarilka0gg.Ustan";

fn main() -> glib::ExitCode {
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
}

fn prepare(src: &str) -> Result<Prepared, String> {
    let dirs = Dirs::from_env();
    let (path, downloaded, origin) = if fetch::is_url(src) {
        let (p, etag) = fetch::download_with_validator(src, &dirs.state.join("cache"), None).map_err(|e| e.to_string())?;
        (p, true, Some((src.to_string(), etag)))
    } else {
        (PathBuf::from(src), false, None)
    };
    let b = backend::pick(&path).ok_or("Цей тип файлу не підтримується")?;
    let info = b.inspect(&path).map_err(|e| e.to_string())?;
    let existing = Manifest::load(&dirs.state, &info.id).ok();
    Ok(Prepared { path, info, downloaded, existing, origin })
}

fn cleanup(p: &Prepared) {
    if p.downloaded {
        let _ = std::fs::remove_file(&p.path);
    }
}

/// Install; with `replace` the existing copy is removed first (reinstall).
fn run_install(p: &Prepared, installer: bool, replace: bool) -> Result<String, String> {
    let dirs = Dirs::from_env();
    if let (true, Some(old)) = (replace, &p.existing) {
        old.uninstall(&dirs.state).map_err(|e| e.to_string())?;
    }
    let b = backend::pick(&p.path).ok_or("Цей тип файлу не підтримується")?;
    let mut m = b.install(&p.path, &dirs, &Opts { installer }).map_err(|e| e.to_string())?;
    if let Some((url, etag)) = &p.origin {
        m.url = Some(url.clone());
        m.etag = etag.clone();
        m.save(&dirs.state).map_err(|e| e.to_string())?;
    }
    cleanup(p);
    Ok(m.name)
}

fn run_remove(p: &Prepared) -> Result<String, String> {
    let m = p.existing.as_ref().ok_or("Програма не встановлена")?;
    m.uninstall(&Dirs::from_env().state).map_err(|e| e.to_string())?;
    cleanup(p);
    Ok(m.name.clone())
}

fn spawn<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> impl std::future::Future<Output = T> {
    let h = gio::spawn_blocking(f);
    async move { h.await.expect("worker thread panicked") }
}

// ---------- install dialog ----------

fn page(title: &str, desc: &str) -> adw::StatusPage {
    adw::StatusPage::builder().title(title).description(desc).build()
}

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
}

impl Bar {
    fn new() -> Self {
        let btn = |label: &str, class: &str| gtk::Button::builder().label(label).css_classes([class]).margin_top(6).margin_bottom(6).build();
        let root = gtk::ActionBar::new();
        let cancel = btn("Скасувати", "flat");
        let remove = btn("Видалити", "destructive-action");
        let main = btn("Встановити", "suggested-action");
        let close = btn("Готово", "suggested-action");
        root.pack_start(&cancel);
        root.pack_start(&remove);
        root.pack_end(&main);
        root.pack_end(&close);
        let revealer = gtk::Revealer::builder().transition_type(gtk::RevealerTransitionType::SlideUp).transition_duration(260).child(&root).build();
        Bar { revealer, cancel, remove, main, close }
    }

    fn show_ready(&self, has_existing: bool) {
        self.revealer.set_reveal_child(true);
        self.cancel.set_visible(true);
        self.remove.set_visible(has_existing);
        self.main.set_visible(true);
        self.close.set_visible(false);
    }

    fn show_busy(&self) {
        self.revealer.set_reveal_child(false);
    }

    fn show_end(&self, label: &str, ok: bool) {
        self.revealer.set_reveal_child(true);
        for b in [&self.cancel, &self.remove, &self.main] {
            b.set_visible(false);
        }
        self.close.set_label(label);
        self.close.set_css_classes(&[if ok { "suggested-action" } else { "flat" }]);
        self.close.set_visible(true);
    }
}

fn install_window(app: &adw::Application, src: String) {
    let win = adw::ApplicationWindow::builder().application(app).default_width(900).default_height(720).title("Встановлення").build();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    let bar = std::rc::Rc::new(Bar::new());
    view.add_bottom_bar(&bar.revealer);
    let stack = gtk::Stack::builder().transition_type(gtk::StackTransitionType::SlideLeft).transition_duration(280).build();
    view.set_content(Some(&stack));
    win.set_content(Some(&view));
    stack.add_named(&spinner_page("Читаю пакет…", &src), Some("loading"));
    win.present();

    let w = win.clone();
    bar.cancel.connect_clicked({
        let w = w.clone();
        move |_| w.close()
    });
    bar.close.connect_clicked(move |_| w.close());

    glib::spawn_future_local(async move {
        let src2 = src.clone();
        match spawn(move || prepare(&src2)).await {
            Err(e) => {
                let p = page("Не вдалося відкрити файл", &e);
                p.set_icon_name(Some("dialog-error-symbolic"));
                stack.add_named(&p, Some("end"));
                stack.set_visible_child_name("end");
                bar.show_end("Закрити", false);
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
                stack.add_named(&spinner_page("Зачекай…", &prep.info.name), Some("busy"));
                stack.set_visible_child_name("ready");
                bar.main.set_label(if existing { "Перевстановити" } else { "Встановити" });
                bar.show_ready(existing);

                // Run `job` off-thread, then show a result page.
                let run = std::rc::Rc::new({
                    let (stack, prep, bar) = (stack.clone(), prep.clone(), bar.clone());
                    move |job: Box<dyn FnOnce(&Prepared) -> Result<String, String> + Send>, ok_title: &'static str, err_title: &'static str| {
                        stack.set_visible_child_name("busy");
                        bar.show_busy();
                        let (stack, prep, bar) = (stack.clone(), prep.clone(), bar.clone());
                        glib::spawn_future_local(async move {
                            let r = spawn({
                                let prep = prep.clone();
                                move || job(&prep)
                            })
                            .await;
                            let ok = r.is_ok();
                            let end = match r {
                                Ok(name) => {
                                    let p = page(ok_title, &name);
                                    p.set_icon_name(Some("emblem-ok-symbolic"));
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
                            bar.show_end(if ok { "Готово" } else { "Закрити" }, ok);
                        });
                    }
                });
                {
                    let run = run.clone();
                    let installer = installer.clone();
                    bar.main.connect_clicked(move |_| {
                        let inst = installer.is_active();
                        run(Box::new(move |p| run_install(p, inst, existing)), if existing { "Перевстановлено" } else { "Встановлено" }, "Не вдалося встановити");
                    });
                }
                bar.remove.connect_clicked(move |_| run(Box::new(run_remove), "Видалено", "Не вдалося видалити"));
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
    let apps = Manifest::list(&state).unwrap_or_default();
    UPDATES.with(|u| u.borrow_mut().retain(|id, _| apps.iter().any(|m| &m.id == id)));
    let group = adw::PreferencesGroup::builder().title("Встановлені програми").margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
    if apps.is_empty() {
        group.set_description(Some("Поки нічого. Відкрий .deb, .AppImage, .exe чи .flatpakref подвійним кліком або кнопкою зверху."));
    }
    for (i, m) in apps.into_iter().enumerate() {
        let row = adw::ActionRow::builder().title(&m.name).subtitle(format!("{} · {}", m.kind, m.version.as_deref().unwrap_or("—"))).build();
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let del = gtk::Button::builder().label("Видалити").valign(gtk::Align::Center).css_classes(["destructive-action"]).build();
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
    col.append(&found);
    bin.set_child(Some(&col));
}

fn found_row(group: &adw::PreferencesGroup, f: &Found, check: bool, bin: &adw::Bin, toasts: &adw::ToastOverlay) {
    let ver = f.version.as_deref().unwrap_or("—");
    let row = adw::ActionRow::builder().title(&f.name).subtitle(format!("{ver} · {}", f.path.display())).subtitle_lines(1).build();
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.add_suffix(&actions);
    group.add(&row);

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
