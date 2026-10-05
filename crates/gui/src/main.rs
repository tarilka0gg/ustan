use adw::prelude::*;
use adw::{gdk, gio, glib, gtk};
use std::path::PathBuf;
use ustan_core::backend::{self, Info, Opts};
use ustan_core::{dirs::Dirs, fetch, manifest::Manifest};

const APP_ID: &str = "io.github.tarilka0gg.Ustan";

fn main() -> glib::ExitCode {
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
}

fn prepare(src: &str) -> Result<Prepared, String> {
    let dirs = Dirs::from_env();
    let (path, downloaded) = if fetch::is_url(src) {
        (fetch::download(src, &dirs.state.join("cache"), None).map_err(|e| e.to_string())?, true)
    } else {
        (PathBuf::from(src), false)
    };
    let b = backend::pick(&path).ok_or("Цей тип файлу не підтримується")?;
    let info = b.inspect(&path).map_err(|e| e.to_string())?;
    Ok(Prepared { path, info, downloaded })
}

fn run_install(p: &Prepared, installer: bool) -> Result<String, String> {
    let b = backend::pick(&p.path).ok_or("Цей тип файлу не підтримується")?;
    let m = b.install(&p.path, &Dirs::from_env(), &Opts { installer }).map_err(|e| e.to_string())?;
    if p.downloaded {
        let _ = std::fs::remove_file(&p.path);
    }
    Ok(m.name)
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

fn pill(label: &str, class: &str) -> gtk::Button {
    gtk::Button::builder().label(label).css_classes(["pill", class]).halign(gtk::Align::Center).build()
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

fn install_window(app: &adw::Application, src: String) {
    let win = adw::ApplicationWindow::builder().application(app).default_width(900).default_height(720).title("Встановлення").build();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    let stack = gtk::Stack::new();
    view.set_content(Some(&stack));
    win.set_content(Some(&view));
    stack.add_named(&spinner_page("Читаю пакет…", &src), Some("loading"));
    win.present();

    let win2 = win.clone();
    glib::spawn_future_local(async move {
        let src2 = src.clone();
        match spawn(move || prepare(&src2)).await {
            Err(e) => {
                let p = page("Не вдалося відкрити файл", &e);
                p.set_icon_name(Some("dialog-error-symbolic"));
                stack.add_named(&p, Some("end"));
                stack.set_visible_child_name("end");
            }
            Ok(prep) => {
                let prep = std::sync::Arc::new(prep);
                let ready = page(&prep.info.name, &describe(&prep.info));
                show_icon(&ready, &prep.info);
                let col = gtk::Box::new(gtk::Orientation::Vertical, 12);
                let installer = adw::SwitchRow::builder().title("Це програма-установник").subtitle("Запустити через Wine і створити ярлики").build();
                let is_exe = prep.info.kind == "exe";
                if is_exe {
                    let g = adw::PreferencesGroup::new();
                    g.add(&installer);
                    col.append(&g);
                }
                let go = pill("Встановити", "suggested-action");
                let cancel = pill("Скасувати", "flat");
                col.append(&go);
                col.append(&cancel);
                ready.set_child(Some(&col));
                stack.add_named(&ready, Some("ready"));
                stack.add_named(&spinner_page("Встановлюю…", &prep.info.name), Some("busy"));
                stack.set_visible_child_name("ready");

                let w = win2.clone();
                cancel.connect_clicked(move |_| w.close());
                let (stack, prep2) = (stack.clone(), prep.clone());
                go.connect_clicked(move |_| {
                    stack.set_visible_child_name("busy");
                    let (stack, prep, inst) = (stack.clone(), prep2.clone(), installer.is_active());
                    glib::spawn_future_local(async move {
                        let r = spawn({
                            let prep = prep.clone();
                            move || run_install(&prep, inst)
                        })
                        .await;
                        let (end, close) = match r {
                            Ok(name) => {
                                let p = page("Встановлено", &name);
                                p.set_icon_name(Some("emblem-ok-symbolic"));
                                (p, pill("Готово", "suggested-action"))
                            }
                            Err(e) => {
                                let p = page("Не вдалося встановити", &e);
                                p.set_icon_name(Some("dialog-error-symbolic"));
                                (p, pill("Закрити", "flat"))
                            }
                        };
                        end.set_child(Some(&close));
                        let win = stack.root().and_downcast::<gtk::Window>();
                        close.connect_clicked(move |_| {
                            if let Some(w) = &win {
                                w.close()
                            }
                        });
                        stack.add_named(&end, Some("end"));
                        stack.set_visible_child_name("end");
                    });
                });
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
    view.add_top_bar(&header);
    let bin = adw::Bin::new();
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&gtk::ScrolledWindow::builder().child(&bin).hscrollbar_policy(gtk::PolicyType::Never).build()));
    view.set_content(Some(&toasts));
    win.set_content(Some(&view));
    refresh(&bin, &toasts);

    let (app, w) = (app.clone(), win.clone());
    open.connect_clicked(move |_| {
        let (app, w) = (app.clone(), w.clone());
        glib::spawn_future_local(async move {
            if let Ok(f) = gtk::FileDialog::new().open_future(Some(&w)).await {
                if let Some(p) = f.path() {
                    install_window(&app, p.display().to_string());
                }
            }
        });
    });
    win.present();
}

fn refresh(bin: &adw::Bin, toasts: &adw::ToastOverlay) {
    let state = Dirs::from_env().state;
    let apps = Manifest::list(&state).unwrap_or_default();
    let group = adw::PreferencesGroup::builder().title("Встановлені програми").margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
    if apps.is_empty() {
        group.set_description(Some("Поки нічого. Відкрий .deb, .AppImage, .exe чи .flatpakref подвійним кліком або кнопкою зверху."));
    }
    for m in apps {
        let row = adw::ActionRow::builder().title(&m.name).subtitle(format!("{} · {}", m.kind, m.version.as_deref().unwrap_or("—"))).build();
        let del = gtk::Button::builder().label("Видалити").valign(gtk::Align::Center).css_classes(["destructive-action"]).build();
        row.add_suffix(&del);
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
                refresh(&bin, &toasts);
            });
        });
        group.add(&row);
    }
    bin.set_child(Some(&group));
}
