mod auth;
mod instance;
mod installed;
mod download;
mod artwork;
mod preferences;
mod library;
mod store;
mod runner;
mod sync;
mod tray;
mod webview_login;

use slint::{ComponentHandle, Image, Model, ModelRc, VecModel};
use slint::winit_030::{WinitWindowAccessor, winit};
use winit::platform::startup_notify::WindowAttributesExtStartupNotify;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;


slint::include_modules!();

fn refresh_library_views(ui: &MainWindow) {
    let prefs = preferences::Preferences::load();
    let query = ui.get_library_query().to_lowercase();
    let platform = ui.get_platform_filter();
    let imported = match installed::load() {
        Ok(games) => games,
        Err(e) => { ui.set_sync_status(e.into()); Vec::new() }
    };
    let mut games: Vec<_> = ui.get_games().iter().collect();
    for game in &mut games { game.installed = imported.iter().any(|g| g.id == game.id); }
    let imported: Vec<_> = imported.into_iter().map(|g| {
        games.iter().find(|item| item.id == g.id).cloned().unwrap_or_else(|| GameItem {
            id: g.id, title: g.title.into(), category: "Imported installation".into(),
            installed: true, windows: g.windows, linux: !g.windows, ..Default::default()
        })
    }).collect();
    ui.set_installed_games(ModelRc::new(VecModel::from(imported.clone())));
    ui.set_games(ModelRc::new(VecModel::from(games.clone())));
    ui.set_recent(ModelRc::new(VecModel::from(games.iter().take(6).cloned().collect::<Vec<_>>())));
    let mut selected = ui.get_selected_game();
    let is_installed = imported.iter().any(|g| g.id == selected.id);
    if selected.installed != is_installed {
        selected.installed = is_installed;
        ui.set_selected_game(selected);
    }
    let extra: Vec<_> = imported.into_iter().filter(|g| !games.iter().any(|item| item.id == g.id)).collect();
    ui.set_filtered_games(ModelRc::new(VecModel::from(games.iter().filter(|g| {
        g.title.to_lowercase().contains(&query) && match platform {
            1 => g.linux, 2 => g.windows, _ => true,
        }
    }).cloned().collect::<Vec<_>>())));
    ui.set_bookmarks(ModelRc::new(VecModel::from(games.into_iter().chain(extra).filter(|g| prefs.bookmarks.contains(&g.id)).collect::<Vec<_>>())));
    ui.set_selected_bookmarked(prefs.bookmarks.contains(&ui.get_selected_game().id));
}

fn show_games(weak: &slint::Weak<MainWindow>, games: &[library::Game]) {
    let games: Vec<_> = games.iter().map(|g| {
        (g.clone(), artwork::load(&library::cover_path(g.id), 640, 360),
         artwork::load(&library::blur_path(g.id), 96, 54))
    }).collect();
    let _ = weak.upgrade_in_event_loop(move |ui| {
        let items: Vec<GameItem> = games
            .into_iter()
            .map(|(g, cover, blur)| {
                GameItem {
                    id: g.id as i32,
                    title: g.title.clone().into(),
                    slug: g.slug.clone().into(),
                    category: g.category.clone().into(),
                    cover: cover.map(artwork::ui_image).unwrap_or_default(),
                    blur: blur.map(artwork::ui_image).unwrap_or_default(),
                    windows: g.windows,
                    linux: g.linux,
                    installed: false,
                }
            })
            .collect();
        let selected_id = ui.get_selected_game().id;
        if let Some(selected) = items.iter().find(|g| g.id == selected_id) {
            ui.set_selected_game(selected.clone());
        }
        ui.set_recent(ModelRc::new(VecModel::from(items.iter().take(6).cloned().collect::<Vec<_>>())));
        ui.set_games(ModelRc::new(VecModel::from(items)));
        refresh_library_views(&ui);
    });
}

/// Shows the cached library instantly, then refreshes it from GOG and fetches covers.
fn spawn_library(rt: &Arc<tokio::runtime::Runtime>, weak: slint::Weak<MainWindow>) {
    let Some(session) = auth::Session::load() else { return };
    let _ = weak.upgrade_in_event_loop(|ui| ui.set_library_loading(true));
    let weak_covers = weak.clone();
    rt.spawn(async move {
        let cached = library::load_cached();
        if !cached.is_empty() { show_games(&weak, &cached); }
        let session = match session.ensure_fresh().await {
            Ok(s) => s,
            Err(_) => {
                auth::Session::delete();
                let _ = weak.upgrade_in_event_loop(|ui| {
                    ui.set_library_loading(false);
                    ui.set_logged_in(false);
                });
                return;
            }
        };
        let avatar_weak = weak.clone();
        let avatar_session = session.clone();
        tokio::spawn(async move {
            if let Some(url) = auth::avatar_url(&avatar_session.access_token).await {
                if let Ok(client) = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build() {
                    if let Some(pixels) = artwork::fetch(&client, &url).await {
                        let username = avatar_session.username;
                        let _ = avatar_weak.upgrade_in_event_loop(move |ui| {
                            if ui.get_logged_in() && ui.get_username() == username.as_str() {
                                ui.set_avatar(artwork::ui_image(pixels));
                            }
                        });
                    }
                }
            }
        });
        if let Ok(games) = library::fetch(&session).await {
            show_games(&weak, &games);
            let games_for_dl = games.clone();
            let weak_prog = weak_covers.clone();
            library::download_covers(&games_for_dl, move |done, total| {
                let _ = weak_prog.upgrade_in_event_loop(move |ui| {
                    ui.set_download_done(done as i32);
                    ui.set_download_total(total as i32);
                });
            }).await;
            show_games(&weak, &games);
        }
        let _ = weak.upgrade_in_event_loop(|ui| {
            ui.set_library_loading(false);
            ui.set_download_total(0);
        });
    });
}

fn main() -> Result<(), slint::PlatformError> {
    // Child-process mode: show the embedded GOG login window and print the auth code.
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--login-webview") {
        let provider = args
            .iter()
            .position(|a| a == "--provider")
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_default();
        webview_login::run(provider, args.iter().any(|a| a == "--creds"));
    }

    let instance = if args.iter().any(|a| a == "--preview-page") { None } else {
        match instance::Instance::acquire().map_err(|e| slint::PlatformError::Other(format!("Cannot register Nebula instance: {e}")))? {
            Some(instance) => Some(instance),
            None => return Ok(()),
        }
    };

    let startup_token = std::env::var("XDG_ACTIVATION_TOKEN").ok()
        .or_else(|| std::env::var("DESKTOP_STARTUP_ID").ok());
    std::env::remove_var("XDG_ACTIVATION_TOKEN");
    std::env::remove_var("DESKTOP_STARTUP_ID");
    let pending_activation = Arc::new(std::sync::Mutex::new(startup_token));
    let hook_activation = pending_activation.clone();
    slint::BackendSelector::new().backend_name("winit".into())
        .with_winit_window_attributes_hook(move |attributes| {
            if let Some(token) = hook_activation.lock().unwrap().take() {
                attributes.with_activation_token(winit::window::ActivationToken::from_raw(token))
            } else { attributes }
        }).select()?;
    // This requires an initialized Slint context; previously the early call failed silently.
    slint::set_xdg_app_id(if std::env::var("FLATPAK_ID").is_ok() { "org.nebula_launcher.Nebula" } else { "nebula" })?;

    let rt = Arc::new(tokio::runtime::Runtime::new().expect("tokio runtime"));
    let ui = MainWindow::new()?;
    let weak = ui.as_weak();
    ui.window().on_winit_window_event(move |_, event| {
        if let winit::event::WindowEvent::Focused(active) = event {
            if let Some(ui) = weak.upgrade() { ui.set_window_active(*active); }
        }
        slint::winit_030::EventResult::Propagate
    });

    // Offline screenshot mode for layout checks, with no account or network access.
    if let Some(index) = args.iter().position(|a| a == "--preview-page") {
        let page = args.get(index + 1).and_then(|v| v.parse().ok()).unwrap_or(2);
        ui.set_logged_in(true);
        ui.set_username("Preview".into());
        ui.set_current_page(page);
        if [0, 1, 9, 10, 12, 20].contains(&page) {
            let artwork_path = args.iter().position(|a| a == "--preview-artwork")
                .and_then(|i| args.get(i + 1)).map(String::as_str).unwrap_or("assets/icon-256.png");
            let pixels = artwork::load(std::path::Path::new(artwork_path), 960, 540);
            let blur = pixels.as_ref().map(|p| artwork::ui_image(image::imageops::blur(
                &image::imageops::thumbnail(p, 96, 64), 8.0))).unwrap_or_default();
            let cover = pixels.map(artwork::ui_image).unwrap_or_default();
            let games = vec![GameItem { id: 1, title: "Preview game".into(), cover: cover.clone(), blur, linux: true, ..Default::default() }];
            ui.set_recent(ModelRc::new(VecModel::from(games.clone())));
            ui.set_games(ModelRc::new(VecModel::from(games.clone())));
            ui.set_filtered_games(ModelRc::new(VecModel::from(games.clone())));
            ui.set_bookmarks(ModelRc::new(VecModel::from(games.clone())));
            ui.set_selected_game(games[0].clone());
            ui.set_store_deals(ModelRc::new(VecModel::from((0..8).map(|i| StoreDealItem {
                id: format!("preview-{i}").into(), title: format!("Preview offer {}", i + 1).into(),
                price: "$9.99".into(), base_price: "$19.99".into(), discount: "-50%".into(), cover: cover.clone(),
            }).collect::<Vec<_>>())));
        }

        ui.set_glass_enabled(!args.iter().any(|a| a == "--preview-no-glass"));
        ui.set_import_open(args.iter().any(|a| a == "--preview-import"));
        ui.set_preview_tile_hover(args.iter().any(|a| a == "--preview-hover"));
        if args.iter().any(|a| a == "--preview-hover") {
            let weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(100), move || {
                if let Some(ui) = weak.upgrade() {
                    ui.window().with_winit_window(|window| {
                        let _ = window.set_cursor_position(winit::dpi::LogicalPosition::new(350.0, 220.0));
                    });
                    ui.window().dispatch_event(slint::platform::WindowEvent::PointerMoved { position: slint::LogicalPosition::new(350.0, 220.0) });
                }
            });
        }
        if args.iter().any(|a| a == "--preview-lifecycle-check") {
            for (delay, visible) in [(100, false), (250, true)] {
                let weak = ui.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(delay), move || {
                    if let Some(ui) = weak.upgrade() {
                        if visible { ui.show().expect("restore window"); eprintln!("Window restore passed"); }
                        else { ui.hide().expect("hide window"); }
                    }
                });
            }
        }

        if args.iter().any(|a| a == "--preview-menu-check") {
            for (delay, x, y) in [(100, 100.0, 72.0), (350, 1000.0, 300.0)] {
                let weak = ui.as_weak();
                slint::Timer::single_shot(std::time::Duration::from_millis(delay), move || {
                    if let Some(ui) = weak.upgrade() {
                        let position = slint::LogicalPosition::new(x, y);
                        let button = slint::platform::PointerEventButton::Left;
                        ui.window().dispatch_event(slint::platform::WindowEvent::PointerPressed { position, button });
                        ui.window().dispatch_event(slint::platform::WindowEvent::PointerReleased { position, button });
                    }
                });
            }
            let weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(250), move || {
                if let Some(ui) = weak.upgrade() {
                    if let Ok(buffer) = ui.window().take_snapshot() {
                        image::save_buffer("/tmp/nebula-menu-open.png", buffer.as_bytes(), buffer.width(), buffer.height(), image::ColorType::Rgba8).expect("save menu preview");
                    }
                }
            });
        }

        let carousel_check = args.iter().any(|a| a == "--preview-carousel-check");
        ui.set_preview_autoplay(carousel_check);
        if carousel_check {
            let weak = ui.as_weak();
            slint::Timer::single_shot(std::time::Duration::from_millis(100), move || {
                if let Some(ui) = weak.upgrade() { ui.set_window_active(true); }
            });
        }
        let weak = ui.as_weak();
        slint::Timer::single_shot(std::time::Duration::from_millis(if carousel_check { 6500 } else { 500 }), move || {
            if let Some(ui) = weak.upgrade() {
                if carousel_check {
                    assert_eq!(ui.get_carousel_idx(), 1, "carousel must advance after six seconds");
                    eprintln!("Carousel auto-advance passed");
                }
                match ui.window().take_snapshot() {
                    Ok(buffer) => {
                        let path = format!("/tmp/nebula-page-{page}.png");
                        image::save_buffer(&path, buffer.as_bytes(), buffer.width(), buffer.height(), image::ColorType::Rgba8).expect("save preview");
                        eprintln!("Saved {path}");
                    }
                    Err(e) => eprintln!("Preview failed: {e}"),
                }
            }
            slint::quit_event_loop().ok();
        });
        ui.show()?;
        return slint::run_event_loop_until_quit();
    }

    if auth::Session::load().is_some() {
        ui.set_logged_in(true);
        if let Some(s) = auth::Session::load() {
            ui.set_username(s.username.into());
        }
        spawn_library(&rt, ui.as_weak());
    }

    ui.set_synced_games(sync::load_timestamps().len() as i32);
    refresh_library_views(&ui);
    let weak = ui.as_weak();
    ui.on_import_game(move |title, path| {
        if let Some(ui) = weak.upgrade() {
            match installed::import(&title, &path) {
                Ok(()) => {
                    refresh_library_views(&ui);
                    ui.set_import_open(false);
                    ui.set_import_error("".into());
                    ui.set_current_page(2);
                    ui.set_sync_status("Imported installed game. Cloud saves ready.".into());
                }
                Err(error) => ui.set_import_error(error.into()),
            }
        }
    });
    let weak = ui.as_weak();
    let browse_rt = rt.clone();
    ui.on_browse_game(move || {
        let weak = weak.clone();
        browse_rt.spawn(async move {
            let result = tokio::process::Command::new("zenity")
                .args(["--file-selection", "--title=Select installed game executable"])
                .output().await;
            let _ = weak.upgrade_in_event_loop(move |ui| {
                match result {
                    Ok(output) if output.status.success() => {
                        let path = String::from_utf8_lossy(&output.stdout).trim_end().to_owned();
                        if ui.get_import_title().is_empty() {
                            let title = std::path::Path::new(&path).file_stem().unwrap_or_default().to_string_lossy();
                            ui.set_import_title(title.as_ref().into());
                        }
                        ui.set_import_path(path.into());
                    }
                    Ok(_) => {}
                    Err(_) => ui.set_import_error("File picker unavailable. Paste the executable path below.".into()),
                }
            });
        });
    });
    let weak = ui.as_weak();
    ui.on_remove_import(move |id| {
        if let Some(ui) = weak.upgrade() {
            match installed::remove(id) {
                Ok(()) => {
                    let mut prefs = preferences::Preferences::load();
                    prefs.bookmarks.remove(&id);
                    if let Err(e) = prefs.save() { ui.set_sync_status(e.to_string().into()); }
                    refresh_library_views(&ui); ui.set_current_page(2);
                }
                Err(e) => ui.set_sync_status(e.into()),
            }
        }
    });

    ui.on_open_link(|url| auth::open_url(&url));
    let weak = ui.as_weak();
    ui.on_filter_games(move |query, platform| {
        if let Some(ui) = weak.upgrade() {
            ui.set_library_query(query);
            ui.set_platform_filter(platform);
            refresh_library_views(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_selection_changed(move || {
        if let Some(ui) = weak.upgrade() { refresh_library_views(&ui); }
    });
    let weak = ui.as_weak();
    ui.on_toggle_bookmark(move |id| {
        if let Some(ui) = weak.upgrade() {
            let mut prefs = preferences::Preferences::load();
            if !prefs.bookmarks.remove(&id) { prefs.bookmarks.insert(id); }
            match prefs.save() {
                Ok(()) => { ui.set_bookmark_status("".into()); refresh_library_views(&ui); }
                Err(e) => ui.set_bookmark_status(format!("Could not save bookmark: {e}").into()),
            }
        }
    });


    // Shared by the email/password button and the social buttons.
    let start_login = {
        let weak = ui.as_weak();
        let rt = rt.clone();
        move |provider: String, creds: Option<(String, String)>| {
            if let Some(ui) = weak.upgrade() {
                ui.set_login_busy(true);
                ui.set_login_error("".into());
            }
            let weak = weak.clone();
            let rt2 = rt.clone();
            rt.spawn(async move {
                let res = auth::login_via_webview(&provider, creds).await;
                let ok = res.is_ok();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_login_busy(false);
                    match res {
                        Ok(s) => {
                            ui.set_username(s.username.into());
                            ui.set_logged_in(true);
                        }
                        Err(auth::AuthError::Cancelled) => {}
                        Err(e) => ui.set_login_error(e.to_string().into()),
                    }
                });
                if ok {
                    let w = weak.clone();
                    let _ = weak.upgrade_in_event_loop(move |_| spawn_library(&rt2, w));
                }
            });
        }
    };

    let sl = start_login.clone();
    ui.on_login(move |email, pass| {
        let creds = (!email.is_empty() && !pass.is_empty()).then(|| (email.to_string(), pass.to_string()));
        sl(String::new(), creds);
    });
    ui.on_login_provider(move |name| start_login(name.to_string(), None));

    let weak = ui.as_weak();
    ui.on_logout(move || {
        auth::Session::delete();
        if let Some(ui) = weak.upgrade() {
            ui.set_logged_in(false);
            ui.set_avatar(Image::default());
            ui.set_games(ModelRc::default());
            ui.set_recent(ModelRc::default());
            refresh_library_views(&ui);
        }
    });

    let weak = ui.as_weak();
    let sync_rt = rt.clone();
    ui.on_sync_clicked(move || {
        if let Some(ui) = weak.upgrade() {
            ui.set_sync_status("Syncing cloud saves…".into());
            let selected_id = ui.get_selected_game().id;
            let weak = weak.clone();
            sync_rt.spawn(async move {
                let res = if selected_id > 0 {
                    sync::sync_game(selected_id, sync::SyncDirection::TwoWay).await
                } else {
                    match sync::sync_all_installed().await {
                        Ok(count) => Ok(format!("Cloud sync complete ({count} games synced).")),
                        Err(e) => Err(e),
                    }
                };
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    match res {
                        Ok(msg) => {
                            ui.set_sync_status(msg.into());
                            let count = sync::load_timestamps().len() as i32;
                            ui.set_synced_games(count);
                        }
                        Err(e) => ui.set_sync_status(format!("Sync error: {e}").into()),
                    }
                });
            });
        }
    });

    let weak = ui.as_weak();
    let play_rt = rt.clone();
    ui.on_play_game(move |id| {
        if let Some(ui) = weak.upgrade() { ui.set_sync_status("Syncing cloud saves before launch…".into()); }
        let weak = weak.clone();
        play_rt.spawn(async move {
            let _ = sync::sync_game(id, sync::SyncDirection::DownloadOnly).await;
            let _ = weak.upgrade_in_event_loop(|ui| ui.set_sync_status("Starting game…".into()));

            let result = installed::launch(id).await;

            if result.is_ok() {
                let _ = weak.upgrade_in_event_loop(|ui| ui.set_sync_status("Uploading cloud saves…".into()));
                let sync_res = sync::sync_game(id, sync::SyncDirection::UploadOnly).await;
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    match sync_res {
                        Ok(_) => ui.set_sync_status("Game closed. Cloud saves synced.".into()),
                        Err(e) => ui.set_sync_status(format!("Game closed. Sync notice: {e}").into()),
                    }
                    let count = sync::load_timestamps().len() as i32;
                    ui.set_synced_games(count);
                });
            } else {
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    ui.set_sync_status(result.unwrap_err().into());
                });
            }
        });
    });

    let weak = ui.as_weak();
    let install_rt = rt.clone();
    let installing = Arc::new(AtomicBool::new(false));
    let install_busy = installing.clone();
    let cancel_install = Arc::new(AtomicBool::new(false));
    let current_plan = Arc::new(std::sync::Mutex::new(None::<download::Plan>));

    let plan_store = current_plan.clone();
    let weak_prepare = weak.clone();
    let rt_prepare = install_rt.clone();
    let installing_prepare = installing.clone();
    ui.on_install_game(move |id| {
        if installing_prepare.load(Ordering::SeqCst) {
            if let Some(ui) = weak_prepare.upgrade() {
                ui.set_install_open(true);
            }
            return;
        }
        if let Some(ui) = weak_prepare.upgrade() {
            ui.set_sync_status("Reading installation information…".into());
        }
        let weak = weak_prepare.clone();
        let plan_store = plan_store.clone();
        rt_prepare.spawn(async move {
            let res = download::prepare(id).await;
            let _ = weak.upgrade_in_event_loop(move |ui| match res {
                Ok(plan) => {
                    let download_gb = format!("{:.1} GB", plan.download_size as f64 / 1_073_741_824.0);
                    let disk_gb = format!("{:.1} GB", plan.disk_size as f64 / 1_073_741_824.0);
                    let req_gb = format!("{:.1} GB", download::required_space(&plan) as f64 / 1_073_741_824.0);
                    let runner_note = if plan.platform == "windows" {
                        let name = runner::choose(preferences::Preferences::load().runner.as_deref())
                            .map(|r| r.name)
                            .unwrap_or_else(|| "Wine/Proton".into());
                        format!("Windows build — runs with {name}")
                    } else {
                        "Native Linux build".to_string()
                    };
                    let home = dirs::home_dir().unwrap_or_default();
                    let default_dest = home.join("Games/Nebula");

                    ui.set_install_game_id(id);
                    ui.set_install_game_title(plan.game.title.clone().into());
                    ui.set_install_runner_note(runner_note.into());
                    ui.set_install_download_size(download_gb.into());
                    ui.set_install_disk_size(disk_gb.into());
                    ui.set_install_req_space(req_gb.into());
                    ui.set_install_destination(default_dest.to_string_lossy().as_ref().into());
                    ui.set_install_error("".into());
                    ui.set_install_in_progress(false);
                    ui.set_install_progress_percent(0);
                    ui.set_install_open(true);
                    ui.set_sync_status(format!("Ready to install {}", plan.game.title).into());

                    let mut store = plan_store.lock().unwrap();
                    *store = Some(plan);
                }
                Err(err) => {
                    ui.set_sync_status(format!("Install error: {err}").into());
                }
            });
        });
    });

    let plan_start = current_plan.clone();
    let cancel_start = cancel_install.clone();
    let weak_start = weak.clone();
    let rt_start = install_rt.clone();
    let installing_start = installing.clone();
    ui.on_start_install(move |id, destination| {
        if installing_start.swap(true, Ordering::SeqCst) {
            return;
        }
        let plan = {
            let store = plan_start.lock().unwrap();
            store.clone()
        };
        let plan = match plan {
            Some(p) if p.game.id as i32 == id => p,
            _ => {
                installing_start.store(false, Ordering::SeqCst);
                if let Some(ui) = weak_start.upgrade() {
                    ui.set_install_error("Installation plan expired. Please try again.".into());
                }
                return;
            }
        };

        cancel_start.store(false, Ordering::SeqCst);
        if let Some(ui) = weak_start.upgrade() {
            ui.set_install_in_progress(true);
            ui.set_install_progress_percent(0);
            ui.set_install_progress_text("Starting download…".into());
            ui.set_install_error("".into());
        }

        let weak_prog = weak_start.clone();
        let weak_done = weak_start.clone();
        let cancel = cancel_start.clone();
        let installing = installing_start.clone();
        let dest_path = std::path::PathBuf::from(destination.trim());
        let game_title = plan.game.title.clone();

        rt_start.spawn(async move {
            let title_clone = game_title.clone();
            let result = download::install(plan, dest_path, cancel, move |p| {
                let weak = weak_prog.clone();
                let title = title_clone.clone();
                let _ = weak.upgrade_in_event_loop(move |ui| {
                    let pct = (p.clamp(0.0, 100.0).round() as i32).clamp(0, 100);
                    ui.set_install_progress_percent(pct);
                    ui.set_install_progress_text(format!("Downloading and extracting… {pct}%").into());
                    ui.set_sync_status(format!("Downloading {title}… {pct}%", title = title).into());
                });
            }).await;

            installing.store(false, Ordering::SeqCst);
            let _ = weak_done.upgrade_in_event_loop(move |ui| {
                match result {
                    Ok(()) => {
                        ui.set_install_in_progress(false);
                        ui.set_install_open(false);
                        refresh_library_views(&ui);
                        ui.set_sync_status(format!("Installation of {game_title} complete! Click Play to start.").into());
                    }
                    Err(err) => {
                        ui.set_install_in_progress(false);
                        ui.set_install_error(err.clone().into());
                        ui.set_sync_status(format!("Install error: {err}").into());
                    }
                }
            });
        });
    });

    let cancel_click = cancel_install.clone();
    let weak_cancel = weak.clone();
    ui.on_cancel_install(move || {
        cancel_click.store(true, Ordering::SeqCst);
        if let Some(ui) = weak_cancel.upgrade() {
            ui.set_install_progress_text("Cancelling installation…".into());
        }
    });

    let weak_browse = weak.clone();
    let rt_browse = install_rt.clone();
    ui.on_browse_install_dir(move || {
        let weak = weak_browse.clone();
        rt_browse.spawn(async move {
            let result = tokio::process::Command::new("zenity")
                .args(["--file-selection", "--directory", "--title=Select Installation Folder"])
                .output().await;
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if let Ok(output) = result {
                    if output.status.success() {
                        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                        if !path.is_empty() {
                            ui.set_install_destination(path.into());
                        }
                    }
                }
            });
        });
    });

    let weak = ui.as_weak();
    let rt_deals = rt.clone();
    let request_generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
    ui.on_fetch_deals(move |only_discounted, sort, query| {
        let generation = request_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let latest = request_generation.clone();
        if let Some(ui) = weak.upgrade() {
            ui.set_store_loading(true);
            ui.set_store_error("".into());
            ui.set_carousel_idx(0);
            ui.set_store_deals(ModelRc::default());
        }
        let weak = weak.clone();
        let sort = sort.to_string();
        let query = query.to_string();
        rt_deals.spawn(async move {
            let deals = match store::fetch_deals(only_discounted, &sort, &query).await {
                Ok(deals) => deals,
                Err(e) => {
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        if latest.load(Ordering::SeqCst) == generation {
                            ui.set_store_loading(false);
                            ui.set_store_error(format!("Could not load GOG store: {e}").into());
                        }
                    });
                    return;
                }
            };
            
            // First render immediately with default icons
            let weak_fast = weak.clone();
            let deals_clone = deals.clone();
            let latest_fast = latest.clone();
            let _ = weak_fast.upgrade_in_event_loop(move |ui| {
                if latest_fast.load(Ordering::SeqCst) != generation { return; }
                ui.set_store_loading(false);
                let items: Vec<StoreDealItem> = deals_clone
                    .into_iter()
                    .map(|d| StoreDealItem {
                        id: d.id.into(),
                        title: d.title.into(),
                        price: d.price.into(),
                        base_price: d.base_price.into(),
                        discount: d.discount.into(),
                        cover: slint::Image::default(),
                    })
                    .collect();
                ui.set_store_deals(ModelRc::new(VecModel::from(items)));
            });

            // Small concurrent batches update each row immediately, without waiting for all artwork.
            let Ok(client) = reqwest::Client::builder().timeout(std::time::Duration::from_secs(15)).build() else { return; };
            let permits = Arc::new(tokio::sync::Semaphore::new(6));
            let mut jobs = tokio::task::JoinSet::new();
            for (index, deal) in deals.into_iter().enumerate() {
                let client = client.clone();
                let permits = permits.clone();
                let latest = latest.clone();
                jobs.spawn(async move {
                    let _permit = permits.acquire_owned().await.ok()?;
                    if latest.load(Ordering::SeqCst) != generation { return None; }
                    artwork::fetch(&client, &deal.cover).await.map(|pixels| (index, pixels))
                });
            }
            while let Some(result) = jobs.join_next().await {
                if latest.load(Ordering::SeqCst) != generation { jobs.abort_all(); break; }
                if let Ok(Some((index, pixels))) = result {
                    let latest = latest.clone();
                    let _ = weak.upgrade_in_event_loop(move |ui| {
                        if latest.load(Ordering::SeqCst) != generation { return; }
                        let model = ui.get_store_deals();
                        if let Some(mut row) = model.row_data(index) {
                            row.cover = artwork::ui_image(pixels);
                            model.set_row_data(index, row);
                        }
                    });
                }
            }
        });
    });

    // Tray icon: closing the window hides it to the tray (only if a tray host exists).
    let tray_ok = Arc::new(AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel();
    if let Some(instance) = &instance {
        instance.listen(tx.clone()).map_err(|e| slint::PlatformError::Other(e.to_string()))?;
    }
    {
        let tray_ok = tray_ok.clone();
        rt.spawn(async move {
            let handle = tray::spawn(tx).await;
            tray_ok.store(handle.is_some(), Ordering::SeqCst);
            if handle.is_some() {
                std::future::pending::<()>().await;
            }
        });
    }
    {
        let weak = ui.as_weak();
        let install_busy = install_busy.clone();
        std::thread::spawn(move || {
            while let Ok(ev) = rx.recv() {
                let pending_activation = pending_activation.clone();
                let install_busy = install_busy.clone();
                let _ = weak.upgrade_in_event_loop(move |ui| match ev {
                    tray::TrayEvent::Show(token) => {
                        // Wayland applies host-provided focus tokens when a surface is mapped.
                        if std::env::var_os("WAYLAND_DISPLAY").is_some() && token.is_some() {
                            let _ = ui.hide();
                            *pending_activation.lock().unwrap() = token;
                        }
                        let _ = ui.show();
                        ui.window().with_winit_window(|window| {
                            window.set_minimized(false);
                            window.focus_window();
                            if !window.has_focus() {
                                window.request_user_attention(Some(winit::window::UserAttentionType::Informational));
                            }
                        });
                    }
                    tray::TrayEvent::Sync => ui.invoke_sync_clicked(),
                    tray::TrayEvent::Quit => {
                        if install_busy.load(Ordering::SeqCst) {
                            ui.set_sync_status("Finish or cancel the installation dialog before quitting.".into());
                            let _ = ui.show();
                            return;
                        }
                        let _ = slint::quit_event_loop();
                    }
                });
            }
        });
    }
    ui.window().on_close_requested(move || {
        if install_busy.load(Ordering::SeqCst) { return slint::CloseRequestResponse::KeepWindowShown; }
        if tray_ok.load(Ordering::SeqCst) {
            slint::CloseRequestResponse::HideWindow
        } else {
            slint::quit_event_loop().ok();
            slint::CloseRequestResponse::HideWindow
        }
    });

    ui.show()?;
    // ksni is external to Slint: its tray icon does not keep Slint's default loop alive.
    slint::run_event_loop_until_quit()
}
