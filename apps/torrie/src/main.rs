mod add;
#[cfg(test)]
mod bench;
mod engine;
mod format;
mod macos;
mod plugins;
mod popover;
mod prefs;
mod rpc;
mod stream;
mod theme;
mod update;
mod window;

use std::{
    rc::Rc,
    sync::{Arc, mpsc},
    time::Duration,
};

use gpui::{
    App, Application, Bounds, KeyBinding, Menu, MenuItem, PromptLevel, SystemMenuType,
    TitlebarOptions, WindowBounds, WindowOptions, point, prelude::*, px, size,
};

use window::*;

fn window_options(title: &str, w: f32, h: f32, min: (f32, f32), cx: &App) -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some(title.to_owned().into()),
            appears_transparent: true,
            traffic_light_position: Some(point(px(16.), px(18.))),
        }),
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(w), px(h)),
            cx,
        ))),
        window_min_size: Some(size(px(min.0), px(min.1))),
        ..Default::default()
    }
}

/// Brings Settings forward, opening it if needed, on `tab` if given.
fn open_settings(engine: &Arc<Engine>, tab: Option<usize>, cx: &mut App) {
    cx.activate(true);
    if let Some(w) = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<prefs::Preferences>())
    {
        _ = w.update(cx, |p, window, cx| {
            window.activate_window();
            if let Some(tab) = tab {
                p.show_tab(tab, cx);
            }
        });
        return;
    }
    let engine = engine.clone();
    _ = cx.open_window(
        window_options("Settings", 540., 640., (460., 420.), cx),
        |window, cx| cx.new(|cx| prefs::Preferences::new(engine, tab.unwrap_or(0), window, cx)),
    );
}

/// Brings the About window forward, opening it if needed.
fn open_about(cx: &mut App) {
    cx.activate(true);
    if let Some(w) = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<update::AboutWindow>())
    {
        _ = w.update(cx, |_, window, _| window.activate_window());
        return;
    }
    _ = cx.open_window(
        WindowOptions {
            is_resizable: false,
            is_minimizable: false,
            ..window_options("About Torrie", 300., 380., (300., 380.), cx)
        },
        |_, cx| cx.new(|_| update::AboutWindow),
    );
}

/// Starts and stops plugin processes to match Settings, and lists their commands in the Plugins menu.
pub fn plugins_changed(engine: &Arc<Engine>, cx: &mut App) {
    plugins::sync(engine);
    set_menus(cx);
}

fn set_menus(cx: &mut App) {
    let commands = plugins::commands().into_iter().map(|(plugin, c)| {
        MenuItem::action(
            c.title,
            RunCommand {
                plugin,
                command: c.id,
            },
        )
    });
    cx.set_menus(vec![
        Menu {
            name: "Torrie".into(),
            items: vec![
                MenuItem::action("About Torrie", About),
                MenuItem::separator(),
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::action("Check for Updates…", CheckForUpdates),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Quit Torrie", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("Open Torrent…", OpenTorrent),
                MenuItem::action("Add Magnet from Clipboard", AddFromClipboard),
            ],
        },
        Menu {
            name: "Transfers".into(),
            items: vec![
                MenuItem::action("Pause / Resume", TogglePause),
                MenuItem::action("Verify Local Data", VerifyLocalData),
                MenuItem::action("Ask for More Peers", AskForMorePeers),
                MenuItem::separator(),
                MenuItem::action("Remove", Remove),
                MenuItem::action("Remove and Delete Data…", RemoveAndDeleteData),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![MenuItem::action("Toggle Inspector", ToggleInspector)],
        },
        Menu {
            name: "Plugins".into(),
            items: commands
                .chain([
                    MenuItem::separator(),
                    MenuItem::action("Manage Plugins…", ManagePlugins),
                ])
                .collect(),
        },
    ]);
}

/// Quits, first asking in the main window if anything is downloading and Settings say to.
fn quit(engine: &Arc<Engine>, urls: &Rc<mpsc::Receiver<String>>, cx: &mut App) {
    let downloading = (engine.rows().iter())
        .filter(|r| r.state == format::State::Downloading)
        .count();
    if !engine.settings().confirm_quit || downloading == 0 {
        return cx.quit();
    }
    open_main(engine, urls, cx);
    let answer = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<MainWindow>())
        .and_then(|w| {
            w.update(cx, |_, window, cx| {
                let detail = match downloading {
                    1 => "1 transfer is still downloading.".into(),
                    n => format!("{n} transfers are still downloading."),
                };
                window.prompt(
                    PromptLevel::Warning,
                    "Quit Torrie?",
                    Some(&format!(
                        "{detail} They pick up where they left off next time."
                    )),
                    &["Quit", "Cancel"],
                    cx,
                )
            })
            .ok()
        });
    let Some(answer) = answer else {
        return cx.quit();
    };
    cx.spawn(async move |cx| {
        if answer.await == Ok(0) {
            _ = cx.update(|cx| cx.quit());
        }
    })
    .detach();
}

/// Brings the main window forward, reopening it if it was closed.
fn open_main(engine: &Arc<Engine>, urls: &Rc<mpsc::Receiver<String>>, cx: &mut App) {
    cx.activate(true);
    if let Some(w) = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<MainWindow>())
    {
        _ = w.update(cx, |_, window, _| window.activate_window());
        return;
    }
    let (engine, urls) = (engine.clone(), urls.clone());
    _ = cx.open_window(
        window_options("Torrie", 1120., 700., (820., 480.), cx),
        |window, cx| cx.new(|cx| MainWindow::new(engine, urls, window, cx)),
    );
}

use engine::Engine;

fn main() {
    let engine = match Engine::start() {
        Ok(e) => Arc::new(e),
        Err(e) => {
            eprintln!("Torrie failed to start: {e:#}");
            macos::fatal(&format!("{e:#}"));
            std::process::exit(1);
        }
    };

    rpc::start(&engine);
    let e = engine.clone();
    std::thread::spawn(move || {
        loop {
            e.add_watched();
            std::thread::sleep(Duration::from_secs(5));
        }
    });

    // Finder double-clicks and browser magnet links can arrive before the window exists, so buffer them.
    let (tx, rx) = mpsc::channel();
    let urls = Rc::new(rx);
    // Menu bar item clicks, plus "show" for each opened link so a closed window comes back.
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    macos::listen(cmd_tx.clone());
    let app = Application::new().with_assets(theme::Assets);
    app.on_open_urls(move |list| {
        list.into_iter().for_each(|u| _ = tx.send(u));
        _ = cmd_tx.send("show".into());
    });
    let (e, u) = (engine.clone(), urls.clone());
    app.on_reopen(move |cx| open_main(&e, &u, cx));

    app.run(move |cx: &mut App| {
        theme::load_fonts(cx);
        #[cfg(debug_assertions)]
        macos::dev_icon();
        theme::apply(&engine.settings(), cx);
        macos::apply(&engine.settings(), cx);
        let (e, u) = (engine.clone(), urls.clone());
        cx.spawn(async move |cx| {
            while let Some(cmd) = cmd_rx.recv().await {
                _ = cx.update(|cx| match cmd.as_str() {
                    "show" => open_main(&e, &u, cx),
                    "popover" => popover::toggle(&e, cx),
                    _ => {}
                });
            }
        })
        .detach();
        let (e, u) = (engine.clone(), urls.clone());
        cx.on_action(move |_: &Quit, cx| quit(&e, &u, cx));
        cx.on_action(|_: &About, cx| open_about(cx));
        let e = engine.clone();
        cx.on_action(move |_: &OpenSettings, cx| open_settings(&e, None, cx));
        let e = engine.clone();
        cx.on_action(move |_: &ManagePlugins, cx| open_settings(&e, Some(prefs::PLUGINS_TAB), cx));
        let (e, u) = (engine.clone(), urls.clone());
        cx.on_action(move |_: &ShowMainWindow, cx| open_main(&e, &u, cx));
        let e = engine.clone();
        cx.on_action(move |_: &CheckForUpdates, cx| update::check_now(&e, true, cx));
        cx.bind_keys([
            KeyBinding::new("cmd-o", OpenTorrent, None),
            KeyBinding::new("cmd-u", AddFromClipboard, None),
            KeyBinding::new("cmd-i", ToggleInspector, None),
            KeyBinding::new("cmd-,", OpenSettings, None),
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("space", TogglePause, Some("MainWindow")),
            KeyBinding::new("backspace", Remove, Some("MainWindow")),
            KeyBinding::new("cmd-alt-backspace", RemoveAndDeleteData, Some("MainWindow")),
            KeyBinding::new("down", SelectNext, Some("MainWindow")),
            KeyBinding::new("up", SelectPrev, Some("MainWindow")),
        ]);
        plugins_changed(&engine, cx);

        let e = engine.clone();
        cx.on_app_quit(move |_| {
            plugins::stop_all();
            e.stop();
            engine::save_uploaded();
            async {}
        })
        .detach();
        // Closing the main window quits, like a single-window utility, unless the menu bar item keeps it running.
        let e = engine.clone();
        cx.on_window_closed(move |cx| {
            if !e.settings().menu_bar
                && !cx
                    .windows()
                    .iter()
                    .any(|w| w.downcast::<MainWindow>().is_some())
            {
                cx.quit();
            }
        })
        .detach();

        let s = engine.settings();
        let at_login = std::env::args().any(|a| a == macos::LOGIN_ARG);
        if !(at_login && s.menu_bar && s.menu_bar_mode) {
            open_main(&engine, &urls, cx);
        }
        update::watch(&engine, cx);
    });
}
