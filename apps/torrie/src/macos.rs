//! macOS integration: the menu bar item, opening at login, notifications and the default torrent app.

use gpui::{App, DisplayId, Global, Pixels, Point, point, px};
use objc2::{
    MainThreadMarker, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject},
};
use objc2_foundation::{NSAppleEventManager, NSBundle, NSError, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNMutableNotificationContent, UNNotificationRequest,
    UNUserNotificationCenter,
};
use tokio::sync::mpsc::UnboundedSender;
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::engine::{Engine, Settings, home};

const LABEL: &str = "xyz.kronz.torrie";
/// Passed by the LaunchAgent (macOS 12).
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

/// `SMAppService.mainApp`; None before macOS 13 or when unbundled.
fn login_service() -> Option<Retained<AnyObject>> {
    NSBundle::mainBundle().bundleIdentifier()?;
    let class = AnyClass::get(c"SMAppService")?;
    // SAFETY: takes no arguments, returns an object.
    unsafe { msg_send![class, mainAppService] }
}

/// `SMAppServiceStatusEnabled`.
const ENABLED: isize = 1;

fn login_enabled(service: &AnyObject) -> bool {
    // SAFETY: returns an NSInteger.
    let status: isize = unsafe { msg_send![service, status] };
    status == ENABLED
}

/// Reads the login item state back from System Settings, migrating a LaunchAgent from older versions.
pub fn sync_login_item(engine: &Engine) {
    let Some(service) = login_service() else {
        return;
    };
    if std::fs::remove_file(launch_agent()).is_ok() {
        if let Err(e) = set_login(&service, true) {
            eprintln!("Torrie: login item: {e}");
        }
    }
    let on = login_enabled(&service);
    if on != engine.settings().open_at_login {
        engine.update_settings(|s| s.open_at_login = on);
    }
}

fn set_login(service: &AnyObject, on: bool) -> Result<(), String> {
    // SAFETY: both take an NSError** and return BOOL.
    let r: Result<(), Retained<NSError>> = unsafe {
        if on {
            msg_send![service, registerAndReturnError: _]
        } else {
            msg_send![service, unregisterAndReturnError: _]
        }
    };
    r.map_err(|e| e.localizedDescription().to_string())
}

/// Must be called during applicationDidFinishLaunching, while the launch event is current.
pub fn launched_at_login() -> bool {
    const PROP_DATA: u32 = u32::from_be_bytes(*b"prop");
    const LOGIN_ITEM: u32 = u32::from_be_bytes(*b"lgit");
    if std::env::args().any(|a| a == LOGIN_ARG) {
        return true;
    }
    let Some(event) = NSAppleEventManager::sharedAppleEventManager().currentAppleEvent() else {
        // Reports differ on whether an SMAppService login launch carries the login item marker or no launch
        // event at all. Finder, the Dock and `open` always send one, so none while registered means login.
        return login_service().is_some_and(|s| login_enabled(&s));
    };
    // SAFETY: takes an AEKeyword, returns a descriptor or nil.
    let prop: Option<Retained<objc2_foundation::NSAppleEventDescriptor>> =
        unsafe { msg_send![&event, paramDescriptorForKeyword: PROP_DATA] };
    prop.is_some_and(|d| d.enumCodeValue() == LOGIN_ITEM)
}

/// `SMAppServiceStatusRequiresApproval`: turned off in System Settings, which only the person can undo there.
const REQUIRES_APPROVAL: isize = 2;

fn login_item(on: bool) -> Result<(), String> {
    static ASKED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    match login_service() {
        Some(service) if login_enabled(&service) != on => {
            set_login(&service, on)?;
            // SAFETY: returns an NSInteger.
            let status: isize = unsafe { msg_send![&*service, status] };
            // Once per run: apply runs on every settings change.
            if on
                && status == REQUIRES_APPROVAL
                && !ASKED.swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                let class = AnyClass::get(c"SMAppService").expect("login_service found it");
                // SAFETY: class method, no arguments, no return value.
                let () = unsafe { msg_send![class, openSystemSettingsLoginItems] };
            }
            Ok(())
        }
        Some(_) => Ok(()),
        None => legacy_login_item(on).map_err(|e| e.to_string()),
    }
}

fn launch_agent() -> std::path::PathBuf {
    home().join(format!("Library/LaunchAgents/{LABEL}.plist"))
}

/// macOS 12 fallback. Rewritten on every apply so it follows the app if moved.
fn legacy_login_item(on: bool) -> std::io::Result<()> {
    let plist = launch_agent();
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

/// Posts a notification as Torrie; `id` replaces an earlier one with the same id. Asks permission the first time
/// (macOS remembers the answer). An unbundled build (`cargo run`) has no identity to post as, so it only logs.
pub fn notify(id: &str, title: &str, body: &str) {
    objc2::rc::autoreleasepool(|_| {
        if NSBundle::mainBundle().bundleIdentifier().is_none() {
            eprintln!("Torrie: {title}: {body} (notifications need the app bundle)");
            return;
        }
        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &NSString::from_str(id),
            &content,
            None,
        );
        let center = UNUserNotificationCenter::currentNotificationCenter();
        let post = block2::RcBlock::new(move |granted: objc2::runtime::Bool, _: *mut NSError| {
            if granted.as_bool() {
                UNUserNotificationCenter::currentNotificationCenter()
                    .addNotificationRequest_withCompletionHandler(&request, None);
            }
        });
        center.requestAuthorizationWithOptions_completionHandler(
            UNAuthorizationOptions::Alert,
            &post,
        );
    });
}

/// For a switch that turns notifications on: asks the first time; if they're already turned off for Torrie in
/// System Settings, where asking again does nothing, opens that page.
pub fn ensure_notifications() {
    let Some(id) = NSBundle::mainBundle().bundleIdentifier() else {
        return;
    };
    let id = id.to_string();
    let check = block2::RcBlock::new(
        move |s: std::ptr::NonNull<objc2_user_notifications::UNNotificationSettings>| {
            // SAFETY: the settings are valid for the duration of the call.
            if unsafe { s.as_ref() }.authorizationStatus()
                == objc2_user_notifications::UNAuthorizationStatus::Denied
            {
                _ = std::process::Command::new("/usr/bin/open")
                .arg(format!("x-apple.systempreferences:com.apple.Notifications-Settings.extension?id={id}"))
                .spawn();
            } else {
                let ok = block2::RcBlock::new(|_: objc2::runtime::Bool, _: *mut NSError| {});
                UNUserNotificationCenter::currentNotificationCenter()
                    .requestAuthorizationWithOptions_completionHandler(
                        UNAuthorizationOptions::Alert,
                        &ok,
                    );
            }
        },
    );
    UNUserNotificationCenter::currentNotificationCenter()
        .getNotificationSettingsWithCompletionHandler(&check);
}

#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    // CFStringRefs, toll-free bridged with NSString; the Create and Copy functions return them +1. Deprecated
    // since macOS 12 for NSWorkspace's async versions, but they still work, synchronously, without UTType bindings.
    fn UTTypeCreatePreferredIdentifierForTag(
        class: &NSString,
        tag: &NSString,
        conforming_to: *const NSString,
    ) -> *mut NSString;
    fn LSCopyDefaultHandlerForURLScheme(scheme: &NSString) -> *mut NSString;
    fn LSCopyDefaultRoleHandlerForContentType(uti: &NSString, role: u32) -> *mut NSString;
    fn LSSetDefaultHandlerForURLScheme(scheme: &NSString, bundle_id: &NSString) -> i32;
    fn LSSetDefaultRoleHandlerForContentType(
        uti: &NSString,
        role: u32,
        bundle_id: &NSString,
    ) -> i32;
}

/// `kLSRolesAll`.
const ROLES_ALL: u32 = 0xFFFF_FFFF;

/// The type Finder gives .torrent files: the `org.bittorrent.torrent` Info.plist declares, unless another app
/// declared its own first.
fn torrent_type() -> Retained<NSString> {
    // SAFETY: valid strings in, no conformance filter; the +1 result is owned by the Retained.
    unsafe {
        Retained::from_raw(UTTypeCreatePreferredIdentifierForTag(
            &NSString::from_str("public.filename-extension"),
            &NSString::from_str("torrent"),
            std::ptr::null(),
        ))
    }
    .unwrap_or_else(|| NSString::from_str("org.bittorrent.torrent"))
}

/// Whether Torrie opens magnet links and .torrent files; None for an unbundled build (`cargo run`).
pub fn is_default_app() -> Option<bool> {
    let id = NSBundle::mainBundle().bundleIdentifier()?.to_string();
    // Launch Services hands bundle ids back lowercased.
    let is_us = |handler: *mut NSString| {
        // SAFETY: a +1 CFString or null, from a Copy function.
        unsafe { Retained::from_raw(handler) }
            .is_some_and(|h| h.to_string().eq_ignore_ascii_case(&id))
    };
    // SAFETY: valid strings in.
    unsafe {
        Some(
            is_us(LSCopyDefaultHandlerForURLScheme(&NSString::from_str(
                "magnet",
            ))) && is_us(LSCopyDefaultRoleHandlerForContentType(
                &torrent_type(),
                ROLES_ALL,
            )),
        )
    }
}

/// Makes Torrie open magnet links and .torrent files; Err has Launch Services' error code.
pub fn make_default_app() -> Result<(), i32> {
    let Some(id) = NSBundle::mainBundle().bundleIdentifier() else {
        return Ok(());
    };
    // SAFETY: valid strings in.
    let codes = unsafe {
        [
            LSSetDefaultHandlerForURLScheme(&NSString::from_str("magnet"), &id),
            LSSetDefaultRoleHandlerForContentType(&torrent_type(), ROLES_ALL, &id),
        ]
    };
    codes.into_iter().find(|&c| c != 0).map_or(Ok(()), Err)
}
