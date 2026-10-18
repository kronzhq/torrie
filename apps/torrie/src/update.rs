//! In-app updates: asks dl.kronz.xyz for the latest Torrie and swaps this app's bundle for it in place. Also the
//! About window, which shares the logo-centred look.
//!
//! A download is only installed if it's notarized, signed by the same team as the running app (read from this
//! app's own signature, so nothing here names it) and newer than it. Development builds (unbundled or ad-hoc
//! signed) have no team, so they never update.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, ensure};
use gpui::{
    App, Context, FocusHandle, FontWeight, KeyDownEvent, MouseButton, Window, div, prelude::*, px,
};
use serde::Deserialize;
use tokio::process::Command;

use crate::{
    engine::Engine,
    theme::{Theme, button, icon, logo, pill, progress_bar, spin},
    window::drag_region,
};

/// Written by `scripts/release.sh`.
const FEED: &str = "https://dl.kronz.xyz/torrie/latest.json";
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// How often a Torrie left running (in the menu bar) checks again.
const EVERY: Duration = Duration::from_secs(24 * 3600);

#[derive(Clone, Deserialize)]
pub struct Release {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    /// The notarized `Torrie.app`, zipped with `ditto`.
    pub url: String,
    /// Bytes, for the progress bar.
    pub size: u64,
}

/// Dotted numbers compared in order: "0.10.0" is newer than "0.9.1".
fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| {
        v.split('.')
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parts(a) > parts(b)
}

/// The bundle this binary runs from (`…/Torrie.app/Contents/MacOS/Torrie`).
fn bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent()?.parent()?.parent()?;
    (app.extension() == Some(OsStr::new("app"))).then(|| app.to_owned())
}

/// Runs a system tool; its stdout, or its stderr as the error.
async fn run(
    tool: &str,
    args: impl IntoIterator<Item = impl AsRef<OsStr>>,
) -> anyhow::Result<String> {
    let out = Command::new(tool)
        .args(args)
        .output()
        .await
        .context(tool.to_owned())?;
    ensure!(
        out.status.success(),
        "{tool}: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The running bundle and the Team ID it's signed with.
async fn signed_bundle() -> anyhow::Result<(PathBuf, String)> {
    let not_release = "Updates come with release builds of Torrie";
    let app = bundle().context(not_release)?;
    // `codesign -d` reports on stderr.
    let out = Command::new("/usr/bin/codesign")
        .args(["-dv", "--verbose=2"])
        .arg(&app)
        .output()
        .await
        .context("codesign")?;
    let team = String::from_utf8_lossy(&out.stderr)
        .lines()
        .find_map(|l| l.strip_prefix("TeamIdentifier="))
        .filter(|t| *t != "not set")
        .map(str::to_owned)
        .context(not_release)?;
    Ok((app, team))
}

/// The latest release, if it's newer than this one.
pub async fn check() -> anyhow::Result<Option<Release>> {
    signed_bundle().await?;
    let json = run(
        "/usr/bin/curl",
        [
            "-fsSL",
            "--proto",
            "=https",
            "--max-time",
            "30",
            "-H",
            "Cache-Control: no-cache",
            FEED,
        ],
    )
    .await
    .context("Couldn't reach dl.kronz.xyz. Check your internet connection")?;
    let r: Release = serde_json::from_str(&json).context("The update feed is unreadable")?;
    Ok(newer(&r.version, VERSION).then_some(r))
}

/// Downloads `r` next to this app (so the swap is a rename on one volume), checks it, and swaps it in. Returns
/// the bundle path and the folder holding the old copy, for [`relaunch`]. `percent` follows the download.
async fn install(r: Release, percent: Arc<AtomicU64>) -> anyhow::Result<(PathBuf, PathBuf)> {
    let (app, team) = signed_bundle().await?;
    let staging = app.with_file_name(".Torrie-update");
    _ = std::fs::remove_dir_all(&staging);
    // Fails inside a read-only disk image or a translocated copy, as well as in a folder this user can't write.
    std::fs::create_dir(&staging).with_context(|| {
        format!(
            "Torrie can't update itself in {}. Move it to your Applications folder and try again",
            app.parent().unwrap_or(&app).display()
        )
    })?;
    let res = async {
        let zip = staging.join("Torrie.zip");
        let mut curl = Command::new("/usr/bin/curl")
            .args([
                "-fsSL",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "-o",
            ])
            .arg(&zip)
            .arg(&r.url)
            .kill_on_drop(true)
            .spawn()
            .context("curl")?;
        let status = loop {
            tokio::select! {
                s = curl.wait() => break s?,
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    let got = std::fs::metadata(&zip).map_or(0, |m| m.len());
                    percent.store((got * 100 / r.size.max(1)).min(100), Ordering::Relaxed);
                }
            }
        };
        ensure!(
            status.success(),
            "Downloading the update failed. Check your internet connection"
        );
        run(
            "/usr/bin/ditto",
            [
                OsStr::new("-x"),
                OsStr::new("-k"),
                zip.as_os_str(),
                staging.as_os_str(),
            ],
        )
        .await?;
        let new = staging.join("Torrie.app");
        let version = run(
            "/usr/bin/plutil",
            [
                OsStr::new("-extract"),
                OsStr::new("CFBundleShortVersionString"),
                OsStr::new("raw"),
                OsStr::new("-o"),
                OsStr::new("-"),
                new.join("Contents/Info.plist").as_os_str(),
            ],
        )
        .await?;
        // An old release replayed from the feed would be signed too.
        ensure!(
            version == r.version && newer(&version, VERSION),
            "The download is Torrie {version}, not {}",
            r.version
        );
        let requirement =
            format!("-R=anchor apple generic and certificate leaf[subject.OU] = \"{team}\"");
        run(
            "/usr/bin/codesign",
            [
                OsStr::new("--verify"),
                OsStr::new("--deep"),
                OsStr::new("--strict"),
                OsStr::new(&requirement),
                new.as_os_str(),
            ],
        )
        .await
        .context("The download isn't signed by Torrie's developer")?;
        run(
            "/usr/sbin/spctl",
            [
                OsStr::new("--assess"),
                OsStr::new("--type"),
                OsStr::new("execute"),
                new.as_os_str(),
            ],
        )
        .await
        .context("The download isn't notarized by Apple")?;
        let old = staging.join("Old.app");
        std::fs::rename(&app, &old).context("Moving the old version aside")?;
        if let Err(e) = std::fs::rename(&new, &app) {
            _ = std::fs::rename(&old, &app);
            return Err(e).context("Putting the new version in place");
        }
        Ok(())
    }
    .await;
    match res {
        Ok(()) => Ok((app, staging)),
        Err(e) => {
            // The app is only ever in `staging` between the two renames above, and put back if the second fails.
            if app.exists() {
                _ = std::fs::remove_dir_all(&staging);
            }
            Err(e)
        }
    }
}

/// Once this process has exited (and saved its session), removes the old copy and opens the new one.
fn relaunch(app: &Path, staging: &Path) {
    _ = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(r#"while kill -0 "$0" 2>/dev/null; do sleep 0.2; done; rm -rf "$1"; open "$2""#)
        .arg(std::process::id().to_string())
        .arg(staging)
        .arg(app)
        .spawn();
}

/// Checks in the background now and every [`EVERY`] while automatic checks are on. Only a newer release
/// opens the window.
pub fn watch(engine: &Arc<Engine>, cx: &mut App) {
    let engine = engine.clone();
    cx.spawn(async move |cx| {
        loop {
            if engine.settings().check_updates {
                _ = cx.update(|cx| check_now(&engine, false, cx));
            }
            cx.background_executor().timer(EVERY).await;
        }
    })
    .detach();
}

/// `manual` (menu, Settings) opens the window right away and also shows "up to date" and errors.
pub fn check_now(engine: &Arc<Engine>, manual: bool, cx: &mut App) {
    if manual {
        show(engine, Status::Checking, true, cx);
    }
    let (engine, req) = (engine.clone(), engine.runtime().spawn(check()));
    cx.spawn(async move |cx| {
        let res = req.await.map_err(anyhow::Error::from).and_then(|r| r);
        _ = cx.update(|cx| {
            let open = window(cx).is_some_and(|w| {
                w.read(cx)
                    .is_ok_and(|w| matches!(w.status, Status::Checking))
            });
            match res {
                Ok(Some(r)) if !manual || open => show(&engine, Status::Available(r), manual, cx),
                Ok(None) if open => show(&engine, Status::Latest, manual, cx),
                Err(e) if open => show(&engine, Status::Failed(format!("{e:#}")), manual, cx),
                Err(e) => eprintln!("Torrie: update check: {e:#}"),
                _ => {}
            }
        });
    })
    .detach();
}

enum Status {
    Checking,
    Latest,
    Failed(String),
    Available(Release),
    Installing(Release, Arc<AtomicU64>),
}

pub struct UpdateWindow {
    engine: Arc<Engine>,
    status: Status,
    focus: FocusHandle,
}

fn window(cx: &App) -> Option<gpui::WindowHandle<UpdateWindow>> {
    cx.windows()
        .into_iter()
        .find_map(|w| w.downcast::<UpdateWindow>())
}

/// Shows `status` in the update window, opening it if needed; an install in progress is left alone. Only
/// `activate` (a check by hand) brings Torrie forward: a background check shouldn't take over another app.
fn show(engine: &Arc<Engine>, status: Status, activate: bool, cx: &mut App) {
    if activate {
        cx.activate(true);
    }
    if let Some(w) = window(cx) {
        _ = w.update(cx, |this, window, cx| {
            if !matches!(this.status, Status::Installing(..)) {
                this.status = status;
            }
            if activate {
                window.activate_window();
            }
            cx.notify();
        });
        return;
    }
    let engine = engine.clone();
    _ = cx.open_window(
        crate::window_options("Software Update", 400., 360., (360., 320.), cx),
        |window, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(window);
                UpdateWindow {
                    engine,
                    status,
                    focus,
                }
            })
        },
    );
}

impl UpdateWindow {
    fn install(&mut self, cx: &mut Context<Self>) {
        let Status::Available(r) = &self.status else {
            return;
        };
        let (r, percent) = (r.clone(), Arc::new(AtomicU64::new(0)));
        self.status = Status::Installing(r.clone(), percent.clone());
        let (task, engine) = (
            self.engine.runtime().spawn(install(r, percent)),
            self.engine.clone(),
        );
        // Sees the install through even if the window is closed meanwhile.
        cx.spawn(async move |this, cx| {
            let res = loop {
                if task.is_finished() {
                    break task.await.map_err(anyhow::Error::from).and_then(|r| r);
                }
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
                _ = this.update(cx, |_, cx| cx.notify());
            };
            match res {
                Ok((app, staging)) => {
                    relaunch(&app, &staging);
                    _ = cx.update(|cx| cx.quit());
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    let shown = this.update(cx, |this, cx| {
                        this.status = Status::Failed(msg.clone());
                        cx.notify();
                    });
                    if shown.is_err() {
                        engine.report(format!("Updating Torrie: {msg}"));
                    }
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.status {
            Status::Available(_) => self.install(cx),
            Status::Failed(_) => check_now(&self.engine, true, cx),
            Status::Installing(..) => {}
            _ => window.remove_window(),
        }
    }

    fn key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "enter" => self.primary(window, cx),
            "escape" if !matches!(self.status, Status::Installing(..)) => window.remove_window(),
            _ => {}
        }
    }
}

impl Render for UpdateWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::of(window, cx);
        let (glyph, title, body) = match &self.status {
            Status::Checking => (
                spin(icon("refresh", px(15.), t.muted), "spin", &t),
                "Checking for updates…".into(),
                "Asking dl.kronz.xyz for the latest Torrie.".into(),
            ),
            Status::Latest => (
                icon("check", px(15.), t.accent).into_any_element(),
                "You're up to date".into(),
                format!("Torrie {VERSION} is the latest version."),
            ),
            Status::Failed(e) => (
                icon("alert", px(15.), t.error).into_any_element(),
                "Couldn't update Torrie".into(),
                e.clone(),
            ),
            Status::Available(r) => (
                icon("arrow-down", px(15.), t.accent).into_any_element(),
                format!("Torrie {} is available", r.version),
                format!(
                    "You have {VERSION}. Torrie restarts into the new version; your transfers pick up where they left off."
                ),
            ),
            Status::Installing(r, _) => (
                spin(icon("refresh", px(15.), t.muted), "spin", &t),
                format!("Updating to Torrie {}…", r.version),
                "Torrie restarts when it's done.".into(),
            ),
        };
        let notes = match &self.status {
            Status::Available(r) if !r.notes.is_empty() => Some(r.notes.clone()),
            _ => None,
        };
        let percent = match &self.status {
            Status::Installing(_, p) => Some(p.load(Ordering::Relaxed)),
            _ => None,
        };
        let close = |_: &gpui::ClickEvent, window: &mut Window, _: &mut App| window.remove_window();
        let footer = match &self.status {
            Status::Available(_) => vec![
                button("later", "Later", &t, false).on_click(close),
                button("install", "Update & Restart", &t, true)
                    .on_click(cx.listener(|this, _, _, cx| this.install(cx))),
            ],
            Status::Failed(_) => vec![
                button("close", "Close", &t, false).on_click(close),
                button("retry", "Try Again", &t, true)
                    .on_click(cx.listener(|this, _, _, cx| check_now(&this.engine, true, cx))),
            ],
            Status::Installing(..) => vec![],
            Status::Checking => vec![button("cancel", "Cancel", &t, false).on_click(close)],
            Status::Latest => vec![button("ok", "OK", &t, true).on_click(close)],
        };

        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .font_family(t.font.clone())
            .text_size(px(12.))
            .text_color(t.text)
            .child(drag_region(px(28.)))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_6()
                    .flex()
                    .flex_col()
                    .items_center()
                    .when(notes.is_none(), |d| d.justify_center())
                    .text_center()
                    .child(
                        // The status sits on the logo as a badge, like the system's own update dialogs.
                        div()
                            .relative()
                            .flex_none()
                            .size(px(72.))
                            .child(logo(px(72.)))
                            .child(
                                div()
                                    .absolute()
                                    .right(px(-4.))
                                    .bottom(px(-2.))
                                    .size(px(28.))
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(t.bg)
                                    .border_1()
                                    .border_color(t.border)
                                    .shadow(t.lift())
                                    .child(glyph),
                            ),
                    )
                    .child(
                        div()
                            .mt_4()
                            .text_size(px(16.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    .child(div().mt_1().text_color(t.muted).child(body))
                    .when_some(percent, |d, p| {
                        d.child(
                            div()
                                .w_full()
                                .mt_5()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(progress_bar(p as f32 / 100., t.down, &t, px(5.)))
                                .child(div().text_size(px(11.)).text_color(t.faint).child(
                                    if p < 100 {
                                        format!("Downloading… {p}%")
                                    } else {
                                        "Checking and installing…".into()
                                    },
                                )),
                        )
                    })
                    .when_some(notes, |d, notes| {
                        d.child(
                            div()
                                .id("notes")
                                .w_full()
                                .flex_1()
                                .min_h_0()
                                .mt_4()
                                .overflow_y_scroll()
                                .p_3()
                                .rounded(t.r_lg())
                                .bg(t.surface)
                                .border_1()
                                .border_color(t.border)
                                .text_left()
                                .text_color(t.muted)
                                .child(
                                    div()
                                        .pb_1()
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(t.text)
                                        .child("What's new"),
                                )
                                .children(notes.lines().map(|l| div().child(l.to_owned()))),
                        )
                    }),
            )
            .child(
                div()
                    .px_5()
                    .py_4()
                    .flex()
                    .justify_center()
                    .gap_2()
                    .children(footer),
            )
    }
}

/// Torrie > About Torrie.
pub struct AboutWindow;

impl Render for AboutWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::of(window, cx);
        let link = |id: &'static str, label: &'static str, url: &'static str| {
            pill(label, t.accent, &t)
                .id(id)
                .cursor_pointer()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(move |_, _, cx| cx.open_url(url))
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px_6()
            .text_center()
            .bg(t.bg)
            .font_family(t.font.clone())
            .text_size(px(12.))
            .text_color(t.text)
            // No title bar to grab, so the whole window drags.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
            .child(logo(px(112.)))
            .child(
                div()
                    .mt_3()
                    .text_size(px(28.))
                    .font_weight(FontWeight::BOLD)
                    .child("Torrie"),
            )
            .child(
                div()
                    .text_color(t.muted)
                    .child(format!("Version {VERSION}")),
            )
            .child(
                div()
                    .mt_3()
                    .text_size(px(14.))
                    .child("BitTorrent, made simple")
                    .child("for your Mac."),
            )
            .child(
                div()
                    .mt_4()
                    .flex()
                    .gap_2()
                    .child(link("site", "torrie.download", "https://torrie.download"))
                    .child(link("source", "Source code", env!("CARGO_PKG_REPOSITORY"))),
            )
            .child(
                div()
                    .mt_4()
                    .text_size(px(11.))
                    .text_color(t.faint)
                    .child("Free and open source under the MIT License.")
                    .child("© Kronz"),
            )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_compare_as_numbers() {
        use super::newer;
        assert!(newer("0.10.0", "0.9.1") && newer("1.0.0", "0.99.99") && newer("0.1.1", "0.1.0"));
        assert!(!newer("0.1.0", "0.1.0") && !newer("0.0.9", "0.1.0"));
    }
}
