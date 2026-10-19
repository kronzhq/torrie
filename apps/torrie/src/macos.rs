//! macOS integration: the menu bar item and opening at login.

use gpui::{App, DisplayId, Global, Pixels, Point, point, px};
use objc2::MainThreadMarker;
use objc2_foundation::NSString;
use tokio::sync::mpsc::UnboundedSender;
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::engine::{Settings, home};

const LABEL: &str = "xyz.kronz.torrie";
/// Passed by the login item, so a launch at login can be told apart.
pub const LOGIN_ARG: &str = "--login";

struct MenuBar(Option<TrayIcon>);

impl Global for MenuBar {}

/// Sends "popover" to `tx` when the menu bar item is clicked.
pub fn listen(tx: UnboundedSender<String>) {
    TrayIconEvent::set_event_handler(Some(move |e| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = e
        {
            _ = tx.send("popover".into());
        }
    }));
}

/// Where a panel under the menu bar item should hang from: the item's bottom center, relative to its display.
pub fn anchor(cx: &App) -> Option<(DisplayId, Point<Pixels>)> {
    let tray = cx.try_global::<MenuBar>()?.0.as_ref()?;
    let frame = tray
        .ns_status_item()?
        .button(MainThreadMarker::new()?)?
        .window()?
        .frame();
    // Cocoa measures up from the primary screen's bottom; gpui down from its top.
    let primary = cx.primary_display()?.bounds().size.height;
    let p = point(
        px((frame.origin.x + frame.size.width / 2.) as f32),
        primary - px(frame.origin.y as f32),
    );
    let display = cx
        .displays()
        .into_iter()
        .find(|d| d.bounds().contains(&point(p.x, p.y - px(1.))))?;
    Some((display.id(), p - display.bounds().origin))
}

/// Says why Torrie can't start, before the app (and its windows) exist; returns once dismissed.
pub fn fatal(error: &str) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
    // `activate` needs macOS 14.
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    let alert = objc2_app_kit::NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str("Torrie couldn't start"));
    alert.setInformativeText(&NSString::from_str(error));
    alert.runModal();
}

/// Gives `cargo run`'s bare binary the bundle's Dock icon, which it'd otherwise lack.
#[cfg(debug_assertions)]
pub fn dev_icon() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = objc2_foundation::NSData::with_bytes(include_bytes!("../macos/AppIcon.icns"));
    if let Some(img) = objc2_app_kit::NSImage::initWithData(mtm.alloc(), &data) {
        unsafe {
            objc2_app_kit::NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&img))
        };
    }
}

/// Shows or hides the menu bar item and adds or removes the login item to match `s`.
pub fn apply(s: &Settings, cx: &mut App) {
    if s.menu_bar != cx.try_global::<MenuBar>().is_some_and(|m| m.0.is_some()) {
        cx.set_global(MenuBar(s.menu_bar.then(tray).flatten()));
    }
    if let Err(e) = login_item(s.open_at_login) {
        eprintln!("Torrie: login item: {e}");
    }
}

fn tray() -> Option<TrayIcon> {
    TrayIconBuilder::new()
        .with_icon_templated(icon()?)
        .with_tooltip("Torrie")
        .build()
        .inspect_err(|e| eprintln!("Torrie: menu bar item: {e}"))
        .ok()
}

/// The mascot (`assets/menubar.svg`, traced from its line art), rasterized for the menu bar (macOS caps it at
/// 22pt and tints it as a template).
fn icon() -> Option<Icon> {
    const PX: u32 = 44;
    let svg = include_bytes!("../assets/menubar.svg");
    let tree = resvg::usvg::Tree::from_data(svg, &Default::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(PX, PX)?;
    let scale = PX as f32 / tree.size().width();
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    Icon::from_rgba(pixmap.take(), PX, PX).ok()
}

/// A LaunchAgent that starts this binary at login. Rewritten on every apply, so it follows the app if moved.
fn login_item(on: bool) -> std::io::Result<()> {
    let plist = home().join(format!("Library/LaunchAgents/{LABEL}.plist"));
    if !on {
        return match std::fs::remove_file(&plist) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe()?
        .to_string_lossy()
        .replace('&', "&amp;")
        .replace('<', "&lt;");
    std::fs::create_dir_all(plist.parent().unwrap())?;
    std::fs::write(
        &plist,
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array><string>{exe}</string><string>{LOGIN_ARG}</string></array>
	<key>RunAtLoad</key>
	<true/>
</dict>
</plist>
"#
        ),
    )
}
