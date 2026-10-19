//! The main window: filter sidebar, transfer list, inspector.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    ops::Range,
    path::Path,
    rc::Rc,
    sync::{Arc, mpsc::Receiver},
    time::{Duration, Instant},
};

use anyhow::Context as _;
use gpui::{
    AnyWindowHandle, App, ClickEvent, Context, Div, ExternalPaths, FocusHandle, FontWeight,
    MouseButton, PathPromptOptions, PromptLevel, ScrollStrategy, SharedString, Stateful,
    UniformListScrollHandle, Window, actions, div, prelude::*, px, relative, uniform_list,
};
use librqbit::AddTorrent;
use serde_json::{Value, json};

use crate::{
    engine::{Details, Engine, FileRow, Settings},
    format::*,
    plugins::{self, Block},
    theme::{self, *},
};

actions!(
    torrie,
    [
        OpenTorrent,
        AddFromClipboard,
        TogglePause,
        Remove,
        RemoveAndDeleteData,
        VerifyLocalData,
        AskForMorePeers,
        ToggleInspector,
        SelectNext,
        SelectPrev,
        OpenSettings,
        ManagePlugins,
        ShowMainWindow,
        CheckForUpdates,
        About,
        Quit
    ]
);

/// Runs a plugin's command; the Plugins menu's items.
#[derive(Clone, PartialEq, gpui::Action)]
#[action(namespace = torrie, no_json)]
pub struct RunCommand {
    pub plugin: String,
    pub command: String,
}

/// How long a plugin may take to render a panel, and to run a command.
const RENDER_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq)]
enum Filter {
    All,
    Active,
    Downloading,
    Seeding,
    Paused,
    Errors,
}

impl Filter {
    fn ix(self) -> usize {
        Filter::ALL.iter().position(|f| *f == self).unwrap()
    }

    const ALL: [Filter; 6] = [
        Filter::All,
        Filter::Active,
        Filter::Downloading,
        Filter::Seeding,
        Filter::Paused,
        Filter::Errors,
    ];

    fn label(self) -> &'static str {
        match self {
            Filter::All => "All Transfers",
            Filter::Active => "Active",
            Filter::Downloading => "Downloading",
            Filter::Seeding => "Seeding",
            Filter::Paused => "Paused",
            Filter::Errors => "Errors",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Filter::All => "layers",
            Filter::Active => "activity",
            Filter::Downloading => "arrow-down",
            Filter::Seeding => "arrow-up",
            Filter::Paused => "pause",
            Filter::Errors => "alert",
        }
    }

    fn matches(self, r: &Row) -> bool {
        match self {
            Filter::All => true,
            Filter::Active => r.active(),
            Filter::Downloading => matches!(
                r.state,
                State::Metadata | State::Downloading | State::Checking
            ),
            Filter::Seeding => r.state == State::Seeding,
            Filter::Paused => r.state == State::Paused,
            Filter::Errors => r.state == State::Error,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Overview,
    Files,
    Peers,
    Trackers,
    /// A plugin's panel, by index into `MainWindow::panels`.
    Panel(usize),
}

const TABS: [(Tab, &str); 4] = [
    (Tab::Overview, "Overview"),
    (Tab::Files, "Files"),
    (Tab::Peers, "Peers"),
    (Tab::Trackers, "Trackers"),
];

/// Sidebar item height plus gap, for the sliding selection.
const FILTER_STRIDE: f32 = 32.;

const HISTORY: usize = 60;

pub struct MainWindow {
    engine: Arc<Engine>,
    urls: Rc<Receiver<String>>,
    rows: Vec<Row>,
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    /// For alerts raised outside an event handler.
    handle: AnyWindowHandle,
    filter: Filter,
    tab: Tab,
    /// Inspector tabs added by running plugins: (plugin id, panel).
    panels: Vec<(String, plugins::Panel)>,
    /// The view of the plugin panel last on show: which (plugin id, panel id, torrent id), and the view or why
    /// there's none. Re-rendered once a second while on show.
    view: Option<((String, String, usize), Result<Vec<Block>, String>)>,
    view_loading: bool,
    inspector: bool,
    details: Option<Details>,
    /// Per-peer byte counters from the previous tick, to derive speeds.
    peer_prev: HashMap<String, (u64, u64)>,
    peer_speed: HashMap<String, (f64, f64)>,
    /// Last minute of total ↓/↑ speeds for the sidebar graph.
    history: VecDeque<(f64, f64)>,
    // Motion state: the selection pills (by index), the inspector (0 hidden, 1 shown), each row's progress bar,
    // and rows that just appeared. Rendering requests frames while any of these move.
    filter_pill: Tween,
    tab_pill: Tween,
    panel: Tween,
    bars: HashMap<usize, Tween>,
    appear: HashMap<usize, Tween>,
    loaded: bool,
}

impl MainWindow {
    pub fn new(
        engine: Arc<Engine>,
        urls: Rc<Receiver<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window);
        // Tick once a second (speed graph, peer speeds), and refresh in between whenever the engine finishes
        // something (pause, remove, add, error) so the list never lags behind a click.
        let e = engine.clone();
        cx.spawn(async move |this, cx| {
            let mut next_tick = Instant::now() + Duration::from_secs(1);
            loop {
                let changed = e.changed_before(next_tick).await.unwrap_or(false);
                let ok = this.update(cx, |this, cx| {
                    if changed {
                        this.refresh(cx)
                    } else {
                        this.tick(cx)
                    }
                });
                if ok.is_err() {
                    break;
                }
                if changed {
                    // A burst of finishing operations (a remote's "start all") refreshes at most 10 times a second;
                    // the ones finishing meanwhile leave one pending notification behind.
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                } else {
                    next_tick = Instant::now() + Duration::from_secs(1);
                }
            }
        })
        .detach();
        let mut this = Self {
            engine,
            urls,
            rows: vec![],
            selected: None,
            scroll: UniformListScrollHandle::new(),
            focus,
            handle: window.window_handle(),
            filter: Filter::All,
            tab: Tab::Overview,
            panels: vec![],
            view: None,
            view_loading: false,
            inspector: true,
            details: None,
            peer_prev: HashMap::new(),
            peer_speed: HashMap::new(),
            history: VecDeque::from(vec![(0.0, 0.0); HISTORY]),
            filter_pill: Tween::new(0.0),
            tab_pill: Tween::new(0.0),
            panel: Tween::new(0.0),
            bars: HashMap::new(),
            appear: HashMap::new(),
            loaded: false,
        };
        this.snapshot(true, cx);
        this
    }

    /// `measure` derives per-peer speeds, so only the once-a-second tick passes it.
    fn snapshot(&mut self, measure: bool, cx: &mut App) {
        while let Ok(url) = self.urls.try_recv() {
            self.open_url(&url, cx);
        }
        self.panels = plugins::panels();
        if matches!(self.tab, Tab::Panel(i) if i >= self.panels.len()) {
            self.tab = Tab::Overview;
            self.tab_pill.set(0.0, 240, theme::motion(cx));
        }
        self.rows = self.engine.rows();
        for r in self.engine.take_reviews() {
            let engine = self.engine.clone();
            cx.defer(move |cx| crate::add::open(engine, r, cx));
        }
        for id in self.engine.take_duplicates() {
            self.point_at_duplicate(id, cx);
        }
        if self
            .selected
            .is_some_and(|id| !self.rows.iter().any(|r| r.id == id))
        {
            self.selected = None;
        }
        self.load_details(measure);
        self.track_motion(theme::motion(cx));
    }

    /// A torrent added again: show the copy already here, open on its files, and say nothing changed.
    fn point_at_duplicate(&mut self, id: usize, cx: &mut App) {
        let Some(name) = self
            .rows
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.name.clone())
        else {
            return;
        };
        let motion = theme::motion(cx);
        self.filter = Filter::All;
        self.filter_pill.set(Filter::All.ix() as f32, 240, motion);
        self.selected = Some(id);
        self.reveal();
        self.inspector = true;
        self.tab = Tab::Files;
        self.tab_pill.set(
            TABS.iter().position(|(t, _)| *t == Tab::Files).unwrap() as f32,
            240,
            motion,
        );
        let handle = self.handle;
        cx.defer(move |cx| {
            cx.activate(true);
            _ = handle.update(cx, |_, window, cx| {
                window.activate_window();
                _ = window.prompt(
                    PromptLevel::Info,
                    &format!("“{name}” is already in your transfers"),
                    Some("Nothing was changed. You can choose which files to download in its Files tab."),
                    &["OK"],
                    cx,
                );
            });
        });
    }

    /// Scrolls the selected row into view, if it's out of view.
    fn reveal(&self) {
        if let Some(i) = self
            .selected
            .and_then(|id| self.visible().iter().position(|r| r.id == id))
        {
            self.scroll.scroll_to_item(i, ScrollStrategy::Top);
        }
    }

    fn track_motion(&mut self, motion: bool) {
        let ids: HashSet<usize> = self.rows.iter().map(|r| r.id).collect();
        self.bars.retain(|id, _| ids.contains(id));
        self.appear.retain(|id, t| ids.contains(id) && t.moving());
        for r in &self.rows {
            let f = r.fraction();
            // Bars draw in 3px columns, so a step under 1% moves no pixels: snap it rather than redraw for 600 ms.
            let step = self
                .bars
                .get(&r.id)
                .is_some_and(|b| (f - b.value()).abs() >= 0.01);
            if !self.bars.contains_key(&r.id) && self.loaded {
                let mut t = Tween::new(0.0);
                t.set(1.0, 280, motion);
                self.appear.insert(r.id, t);
            }
            self.bars
                .entry(r.id)
                .or_insert_with(|| Tween::new(f))
                .set(f, 600, motion && step);
        }
        self.loaded = true;
    }

    fn bar(&self, r: &Row) -> f32 {
        self.bars.get(&r.id).map_or(r.fraction(), Tween::value)
    }

    fn animating(&self) -> bool {
        [self.filter_pill, self.tab_pill, self.panel]
            .iter()
            .chain(self.bars.values())
            .chain(self.appear.values())
            .any(Tween::moving)
    }

    fn load_details(&mut self, measure: bool) {
        self.details = self
            .selected
            .filter(|_| self.inspector)
            .and_then(|id| self.engine.details(id));
        let Some(d) = self.details.as_ref().filter(|_| measure) else {
            return;
        };
        let prev = std::mem::take(&mut self.peer_prev);
        self.peer_speed.clear();
        for p in &d.peers {
            if let Some((down, up)) = prev.get(&p.addr) {
                let speed = (
                    p.downloaded.saturating_sub(*down) as f64,
                    p.uploaded.saturating_sub(*up) as f64,
                );
                self.peer_speed.insert(p.addr.clone(), speed);
            }
            self.peer_prev
                .insert(p.addr.clone(), (p.downloaded, p.uploaded));
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        self.snapshot(true, cx);
        self.fetch_view(cx);
        let total = self
            .rows
            .iter()
            .fold((0.0, 0.0), |(d, u), r| (d + r.down_bps, u + r.up_bps));
        self.history.pop_front();
        self.history.push_back(total);
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.snapshot(false, cx);
        cx.notify();
    }

    fn select(&mut self, id: Option<usize>, cx: &mut Context<Self>) {
        if self.selected != id {
            self.selected = id;
            self.reveal();
            self.peer_prev.clear();
            self.peer_speed.clear();
            self.load_details(true);
            self.fetch_view(cx);
        }
        cx.notify();
    }

    /// The inspector's tabs: the built-in ones, then plugins' panels.
    fn tabs(&self) -> Vec<(Tab, SharedString)> {
        let panels = self
            .panels
            .iter()
            .enumerate()
            .map(|(i, (_, p))| (Tab::Panel(i), p.title.clone().into()));
        TABS.iter()
            .map(|&(tab, label)| (tab, label.into()))
            .chain(panels)
            .collect()
    }

    /// A transfer as the API and plugins see it; `None` for magnets still fetching metadata.
    fn torrent(&self, id: usize) -> Option<Value> {
        crate::rpc::select(&self.engine, Some(&json!(id)))
            .first()
            .map(|(id, h)| crate::rpc::torrent(&self.engine, *id, h))
    }

    /// Asks a plugin for the view of its panel on show, unless the last ask is still out.
    fn fetch_view(&mut self, cx: &mut Context<Self>) {
        let Tab::Panel(i) = self.tab else { return };
        let (Some((plugin, panel)), Some(id)) = (self.panels.get(i).cloned(), self.selected) else {
            return;
        };
        if self.view_loading || !self.inspector {
            return;
        }
        let Some(torrent) = self.torrent(id) else {
            return;
        };
        self.view_loading = true;
        let params = json!({ "panel": panel.id, "torrent": torrent });
        let req = self.engine.runtime().spawn(plugins::request(
            plugin.clone(),
            "panel.render",
            params,
            RENDER_TIMEOUT,
        ));
        cx.spawn(async move |this, cx| {
            let view = req.await.map_err(anyhow::Error::from).and_then(|r| r);
            let view = view
                .and_then(|v| serde_json::from_value(v).context("not a view"))
                .map_err(|e| format!("{e:#}"));
            this.update(cx, |this, cx| {
                this.view_loading = false;
                this.view = Some(((plugin, panel.id, id), view));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn run_command(&mut self, a: &RunCommand, _: &mut Window, cx: &mut Context<Self>) {
        let c = plugins::commands()
            .into_iter()
            .find(|(p, c)| *p == a.plugin && c.id == a.command);
        self.command(
            a.plugin.clone(),
            a.command.clone(),
            c.is_some_and(|(_, c)| c.torrent),
            cx,
        );
    }

    /// Sends `command.run` to a plugin, with the selected transfer if `torrent`. Errors show in the window.
    fn command(&mut self, plugin: String, command: String, torrent: bool, cx: &mut Context<Self>) {
        let t = match self.selected.and_then(|id| self.torrent(id)) {
            Some(t) if torrent => t,
            None if torrent => {
                self.engine.report("Select a transfer first".into());
                return cx.notify();
            }
            _ => Value::Null,
        };
        let params = json!({ "command": command, "torrent": t });
        let req = self.engine.runtime().spawn(plugins::request(
            plugin,
            "command.run",
            params,
            COMMAND_TIMEOUT,
        ));
        cx.spawn(async move |this, cx| {
            let res = req.await.map_err(anyhow::Error::from).and_then(|r| r);
            this.update(cx, |this, cx| {
                if let Err(e) = res {
                    this.engine.report(format!("{e:#}"));
                }
                // A panel may show what the command changed.
                this.fetch_view(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Installs the plugin package `url` points at once the user agrees, then opens Settings → Plugins, where it
    /// waits switched off.
    fn install(&self, url: String, cx: &mut App) {
        let (engine, handle) = (self.engine.clone(), self.handle);
        let fetch = engine.runtime().spawn(plugins::fetch(url));
        cx.spawn(async move |cx| {
            let report = |e: anyhow::Error| engine.report(format!("{e:#}"));
            let pkg = match fetch.await.map_err(anyhow::Error::from).and_then(|r| r) {
                Ok(pkg) => pkg,
                Err(e) => return report(e),
            };
            let m = &pkg.manifest;
            let title = match &pkg.replaces {
                Some(old) => format!("Replace {old} with {} {}?", m.name, m.version),
                None => format!("Install the plugin “{}”?", m.name),
            };
            let by = (!m.author.is_empty()).then(|| format!("By {}.", m.author));
            let warning = "Plugins run as you, with your files and network: only install ones you trust. \
                It stays off until you switch it on in Settings.";
            let detail = [Some(m.description.clone()), by, Some(warning.into())].into_iter().flatten().filter(|s| !s.is_empty());
            let detail = detail.collect::<Vec<_>>().join("\n\n");
            let answer = handle.update(cx, |_, window, cx| {
                cx.activate(true);
                window.activate_window();
                window.prompt(PromptLevel::Warning, &title, Some(&detail), &["Install", "Cancel"], cx)
            });
            let Ok(answer) = answer else { return };
            if answer.await != Ok(0) {
                return;
            }
            match engine.runtime().spawn(plugins::install(pkg)).await.map_err(anyhow::Error::from).and_then(|r| r) {
                Ok(id) => {
                    _ = cx.update(|cx| {
                        engine.update_settings(|s| s.plugins_enabled.retain(|p| *p != id));
                        crate::plugins_changed(&engine, cx);
                        cx.dispatch_action(&ManagePlugins);
                    });
                }
                Err(e) => report(e.context("Installing the plugin")),
            }
        })
        .detach();
    }

    /// `magnet:`, `http(s)://…torrent` or `file://…torrent` (Finder double-click, browser magnet link), or a plugin
    /// package or install link.
    fn open_url(&mut self, url: &str, cx: &mut App) {
        if plugins::is_package(url) {
            self.install(url.to_owned(), cx);
        } else if url.starts_with("file://") {
            match url::Url::parse(url)
                .ok()
                .and_then(|u| u.to_file_path().ok())
            {
                Some(path) => self.add_file(&path),
                None => self.engine.report(format!("Can't open {url}")),
            }
        } else if ["magnet:", "http://", "https://"]
            .iter()
            .any(|p| url.starts_with(p))
        {
            let s = self.engine.settings();
            self.engine.add(
                AddTorrent::from_url(url.to_owned()),
                url_name(url),
                None,
                !s.start_added,
                s.add_window,
            );
        } else {
            self.engine
                .report("Not a magnet link or torrent URL".into());
        }
    }

    fn add_file(&mut self, path: &Path) {
        match std::fs::read(path) {
            Ok(bytes) => {
                let name = path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let s = self.engine.settings();
                self.engine.add(
                    AddTorrent::from_bytes(bytes),
                    name,
                    None,
                    !s.start_added,
                    s.add_window,
                )
            }
            Err(e) => self.engine.report(format!("{}: {e}", path.display())),
        }
    }

    /// The oldest error waiting, and how many more are behind it. Dismissing it shows the next.
    fn error_line(&self) -> Option<String> {
        let errors = self.engine.errors.lock().unwrap();
        errors.first().map(|e| match errors.len() {
            1 => e.clone(),
            n => format!("{e} · {} more", n - 1),
        })
    }

    fn visible(&self) -> Vec<&Row> {
        self.rows
            .iter()
            .filter(|r| self.filter.matches(r))
            .collect()
    }

    fn selected_row(&self) -> Option<&Row> {
        self.rows.iter().find(|r| Some(r.id) == self.selected)
    }

    // --- actions ---

    fn open_torrent(&mut self, _: &OpenTorrent, _: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                this.update(cx, |this, cx| {
                    paths.iter().for_each(|p| this.add_file(p));
                    this.refresh(cx);
                })
                .ok();
            }
        })
        .detach();
    }

    fn add_from_clipboard(&mut self, _: &AddFromClipboard, _: &mut Window, cx: &mut Context<Self>) {
        let text = cx
            .read_from_clipboard()
            .and_then(|c| c.text())
            .unwrap_or_default();
        self.open_url(text.trim(), cx);
        self.refresh(cx);
    }

    fn toggle_pause(&mut self, _: &TogglePause, _: &mut Window, _: &mut Context<Self>) {
        if let Some(r) = self.selected_row() {
            self.engine.set_paused(r.id, !r.resumable());
        }
    }

    fn remove(&mut self, _: &Remove, _: &mut Window, _: &mut Context<Self>) {
        if let Some(r) = self.selected_row() {
            self.engine.remove(r.id, false);
        }
    }

    fn verify_local_data(&mut self, _: &VerifyLocalData, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(r) = self.selected_row().filter(|r| can_verify(r)) {
            self.engine.verify(r.id);
            cx.notify();
        }
    }

    fn ask_for_more_peers(&mut self, _: &AskForMorePeers, _: &mut Window, _: &mut Context<Self>) {
        if let Some(r) = self.selected_row().filter(|r| can_reannounce(r)) {
            self.engine.reannounce(r.id);
        }
    }

    fn remove_and_delete_data(
        &mut self,
        _: &RemoveAndDeleteData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(r) = self.selected_row().cloned() {
            self.confirm_delete(r, window, cx);
        }
    }

    fn confirm_delete(&self, r: Row, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Delete the downloaded data for “{}”?", r.name),
            Some("The files are permanently deleted from disk, not moved to the Trash."),
            &["Delete", "Cancel"],
            cx,
        );
        let engine = self.engine.clone();
        cx.spawn(async move |_, _| {
            if answer.await == Ok(0) {
                engine.remove(r.id, true);
            }
        })
        .detach();
    }

    fn toggle_inspector(&mut self, _: &ToggleInspector, _: &mut Window, cx: &mut Context<Self>) {
        self.inspector = !self.inspector;
        // Closing keeps the details so the panel can fade out; the next snapshot drops them.
        if self.inspector {
            self.load_details(false);
        }
        cx.notify();
    }

    fn step_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let ids: Vec<usize> = self.visible().iter().map(|r| r.id).collect();
        let pos = self
            .selected
            .and_then(|id| ids.iter().position(|&i| i == id));
        let next = match pos {
            Some(p) => p
                .saturating_add_signed(delta)
                .min(ids.len().saturating_sub(1)),
            None => 0,
        };
        self.select(ids.get(next).copied(), cx);
    }

    // --- views ---

    fn sidebar(&self, t: &Theme, cx: &Context<Self>) -> Div {
        let (down, up) = self.history.back().copied().unwrap_or_default();
        let s = self.engine.settings();
        let alt = s.alt_speed_enabled;
        let pending = self.engine.pending.lock().unwrap().len();
        // Columns stack ↓ under ↑, so the scale is their sum. With alt limits on it also takes in the cap so its
        // line stays in view; either way a quarter of headroom keeps the peak off the top edge.
        let cap = alt.then(|| s.alt_speed_down_kbps as f64 * 1000.0);
        let max = self
            .history
            .iter()
            .map(|(d, u)| d + u)
            .fold(cap.unwrap_or(1.0), f64::max)
            * 1.25;

        div()
            .w(px(220.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(t.sidebar)
            .child(drag_region(px(52.)))
            .child(
                div()
                    .px_4()
                    .pb_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(logo(px(24.)))
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::BOLD)
                            .child("Torrie"),
                    ),
            )
            .child(
                div()
                    .px_4()
                    .pb_1()
                    .text_size(px(10.5))
                    .text_color(t.faint)
                    .child("LIBRARY"),
            )
            .child(
                div().px_2().child(
                    div()
                        .relative()
                        .flex()
                        .flex_col()
                        .gap(px(FILTER_STRIDE - 30.))
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .h(px(30.))
                                .top(px(self.filter_pill.value() * FILTER_STRIDE))
                                .rounded(t.r_md())
                                .bg(t.elevated)
                                .shadow(t.lift()),
                        )
                        .children(Filter::ALL.map(|f| self.filter_item(f, t, cx))),
                ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .m_3()
                    .p_3()
                    .rounded(t.r_lg())
                    .bg(t.surface.opacity(0.7))
                    .border_1()
                    .border_color(t.border)
                    .shadow(t.lift())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .child(speed_label("arrow-down", down, t.down, t))
                            .child(speed_label("arrow-up", up, t.up, t)),
                    )
                    .child(
                        div()
                            .relative()
                            .h(px(36.))
                            .child(speed_graph(
                                self.history.iter().copied().collect(),
                                max,
                                (!alt).then_some(t.faint.opacity(0.6)),
                                t,
                            ))
                            .when_some(cap, |d, c| {
                                d.child(theme::ceiling(px((c / max * 36.0) as f32), t.accent))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_size(px(10.5))
                            .text_color(t.faint)
                            .child(div().flex_1().min_w_0().truncate().child(if pending > 0 {
                                format!("Fetching metadata for {pending} magnet link(s)…")
                            } else if alt {
                                "Alt speed limits on".into()
                            } else if self.engine.listen_port().is_none() {
                                "Not listening for peers".into()
                            } else {
                                match self.rows.iter().map(|r| r.peers_live).sum::<u32>() {
                                    0 => "No peers connected".into(),
                                    1 => "1 peer connected".into(),
                                    n => format!("{n} peers connected"),
                                }
                            }))
                            .child(
                                div()
                                    .id("alt-speed")
                                    .flex_none()
                                    .p_1()
                                    .rounded(t.r_sm())
                                    .when(alt, |d| d.bg(t.elevated).shadow(t.lift()))
                                    .when(!alt, |d| d.hover(|s| s.bg(t.hover)))
                                    .child(icon(
                                        "gauge",
                                        px(14.),
                                        if alt { t.accent } else { t.faint },
                                    ))
                                    .tooltip(tip("Alternative speed limits"))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.engine.update_settings(|s| {
                                            s.alt_speed_enabled = !s.alt_speed_enabled
                                        });
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
    }

    fn filter_item(&self, f: Filter, t: &Theme, cx: &Context<Self>) -> Stateful<Div> {
        let count = self.rows.iter().filter(|r| f.matches(r)).count();
        let on = self.filter == f;
        div()
            .id(f.label())
            .h(px(30.))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .rounded(t.r_md())
            .when(on, |d| d.font_weight(FontWeight::MEDIUM))
            .when(!on, |d| d.text_color(t.muted).hover(|s| s.bg(t.hover)))
            .child(icon(f.icon(), px(14.), if on { t.accent } else { t.faint }))
            .child(div().flex_1().child(f.label()))
            .when(count > 0, |d| {
                d.child(
                    div()
                        .text_size(px(11.))
                        .text_color(t.faint)
                        .child(count.to_string()),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.filter = f;
                this.filter_pill.set(f.ix() as f32, 240, theme::motion(cx));
                cx.notify();
            }))
    }

    fn header(&self, t: &Theme, cx: &Context<Self>) -> Div {
        let count = self.visible().len();
        div()
            .h(px(52.))
            .flex_none()
            .flex()
            .items_center()
            .gap_1()
            .pl_5()
            .pr_3()
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(self.filter.label()),
                    )
                    .child(div().pl_2().text_color(t.faint).child(count.to_string())),
            )
            .child(drag_region(px(52.)).flex_1())
            .child(
                icon_button("open", "plus", t)
                    .tooltip(tip("Open Torrent… ⌘O"))
                    .on_click(cx.listener(|this, _, w, cx| this.open_torrent(&OpenTorrent, w, cx))),
            )
            .child(
                icon_button("magnet", "magnet", t)
                    .tooltip(tip("Add Magnet from Clipboard ⌘U"))
                    .on_click(cx.listener(|this, _, w, cx| {
                        this.add_from_clipboard(&AddFromClipboard, w, cx)
                    })),
            )
            .child(
                icon_button("inspector", "panel", t)
                    .when(self.inspector, |d| d.bg(t.elevated).shadow(t.lift()))
                    .tooltip(tip("Inspector ⌘I"))
                    .on_click(
                        cx.listener(|this, _, w, cx| {
                            this.toggle_inspector(&ToggleInspector, w, cx)
                        }),
                    ),
            )
            .child(
                icon_button("settings", "sliders", t)
                    .tooltip(tip("Settings ⌘,"))
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
            )
    }

    fn list(&self, t: &Theme, cx: &Context<Self>) -> Stateful<Div> {
        let accent = t.accent;
        let tint = t.tint(t.accent);
        let count = self.visible().len();
        // Only rows in view are built: the window redraws every frame while anything animates.
        let rows = uniform_list(
            "rows",
            count,
            cx.processor(|this, range: Range<usize>, window, cx| {
                let t = Theme::of(window, cx);
                this.visible()[range]
                    .iter()
                    .map(|r| div().pb_1().child(this.row(r, &t, cx)))
                    .collect::<Vec<_>>()
            }),
        )
        .flex_1()
        .p_2()
        .track_scroll(self.scroll.clone());
        div()
            .id("list")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .border_2()
            .border_color(gpui::transparent_black())
            .drag_over::<ExternalPaths>(move |s, _, _, _| s.bg(tint).border_color(accent))
            .map(|d| {
                if count == 0 {
                    d.child(self.empty(t, cx))
                } else {
                    d.child(rows)
                }
            })
    }

    fn empty(&self, t: &Theme, cx: &Context<Self>) -> Div {
        let filtered = !self.rows.is_empty();
        div()
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .size(px(64.))
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(t.tint(t.accent))
                    .child(icon(
                        if filtered {
                            self.filter.icon()
                        } else {
                            "magnet"
                        },
                        px(28.),
                        t.accent,
                    )),
            )
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if filtered {
                        format!("Nothing in {}", self.filter.label())
                    } else {
                        "No transfers yet".into()
                    }),
            )
            .when(!filtered, |d| {
                d.child(
                    div()
                        .text_color(t.muted)
                        .child("Drop a .torrent file here, or add one below."),
                )
                .child(
                    div()
                        .pt_2()
                        .flex()
                        .gap_2()
                        .child(button("empty-open", "Open Torrent…", t, true).on_click(
                            cx.listener(|this, _, w, cx| this.open_torrent(&OpenTorrent, w, cx)),
                        ))
                        .child(
                            button("empty-magnet", "Paste Magnet Link", t, false).on_click(
                                cx.listener(|this, _, w, cx| {
                                    this.add_from_clipboard(&AddFromClipboard, w, cx)
                                }),
                            ),
                        ),
                )
            })
    }

    fn row(&self, r: &Row, t: &Theme, cx: &Context<Self>) -> Stateful<Div> {
        let id = r.id;
        let paused = r.resumable();
        let selected = self.selected == Some(id);
        let color = t.state(r.state);
        let appear = self.appear.get(&id).map(Tween::value);
        let glyph = icon(state_icon(r.state), px(16.), color);

        div()
            .id(("row", id))
            .when_some(appear, |d, v| {
                d.opacity(v).relative().top(px((1.0 - v) * 6.0))
            })
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .px_3()
            .py(px(10.))
            .rounded(t.r_lg())
            .when(selected, |d| d.bg(t.elevated).shadow(t.lift()))
            .when(!selected, |d| d.hover(|s| s.bg(t.hover)))
            .on_click(cx.listener(move |this, ev: &ClickEvent, _, cx| {
                if ev.click_count() == 2 {
                    this.inspector = true;
                }
                this.select(Some(id), cx);
            }))
            .child(
                div()
                    .size(px(34.))
                    .flex_none()
                    .rounded(t.r_lg())
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(t.tint(color))
                    .child(if r.state == State::Checking {
                        spin(glyph, ("spin", id), t)
                    } else {
                        glyph.into_any_element()
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(r.name.clone()),
                            )
                            // Fixed slots, always present (dimmed at zero), so nothing reflows as speeds come and go.
                            .child(speed_slot(speed_label("arrow-down", r.down_bps, t.down, t)))
                            .child(speed_slot(speed_label("arrow-up", r.up_bps, t.up, t))),
                    )
                    .child(if r.state == State::Metadata {
                        busy_bar(&id.to_string(), color, t, px(5.))
                    } else {
                        progress_bar(self.bar(r), color, t, px(5.)).into_any_element()
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .text_size(px(11.))
                            .child(
                                // Both shrink (with an ellipsis) as the window narrows instead of being clipped.
                                div()
                                    .flex_grow()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(if r.state == State::Error {
                                        t.error
                                    } else {
                                        t.muted
                                    })
                                    .child(state_label(r)),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(t.faint)
                                    .child(progress_line(r)),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .when(r.state == State::Metadata, |d| {
                        d.child(
                            icon_button(("retry", id), "refresh", t)
                                .tooltip(tip("Retry Now"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.engine.retry_now(id);
                                    cx.stop_propagation();
                                })),
                        )
                    })
                    .when(r.state != State::Metadata, |d| {
                        d.child(
                            icon_button(("pause", id), if paused { "play" } else { "pause" }, t)
                                .tooltip(tip(match r.state {
                                    State::Error => "Retry",
                                    State::Paused => "Resume",
                                    _ => "Pause",
                                }))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.engine.set_paused(id, !paused);
                                    cx.stop_propagation();
                                })),
                        )
                    })
                    .child(
                        icon_button(("remove", id), "x", t)
                            .tooltip(tip("Remove (keeps data)"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.engine.remove(id, false);
                                cx.stop_propagation();
                            })),
                    )
                    .child(
                        icon_button(("delete", id), "trash", t)
                            .tooltip(tip("Remove and delete data"))
                            .on_click({
                                let r = r.clone();
                                cx.listener(move |this, _, window, cx| {
                                    this.confirm_delete(r.clone(), window, cx);
                                    cx.stop_propagation();
                                })
                            }),
                    ),
            )
    }

    fn inspector(&self, r: &Row, d: &Details, t: &Theme, cx: &Context<Self>) -> Div {
        let color = t.state(r.state);
        let body = match self.tab {
            Tab::Overview => self.overview(r, d, t, cx),
            Tab::Files => self.files(r, d, t, cx),
            Tab::Peers => self.peers(d, t),
            Tab::Trackers => trackers(d, &self.engine.settings(), t),
            Tab::Panel(i) => self.panel(i, r, t, cx),
        };
        let tabs = self.tabs();
        let n = tabs.len() as f32;
        div()
            .w(px(340.))
            .flex_none()
            .h_full()
            .flex()
            .flex_col()
            .bg(t.surface)
            .border_l_1()
            .border_color(t.border)
            .child(drag_region(px(12.)))
            .child(
                div()
                    .px_4()
                    .pt_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .line_clamp(2)
                            .text_ellipsis()
                            .child(r.name.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(26.))
                                    .font_weight(FontWeight::BOLD)
                                    .line_height(px(30.))
                                    .child(pct(r.fraction())),
                            )
                            .child(div().flex_1())
                            .child(pill(format!("{:?}", r.state), color, t).mb_1()),
                    )
                    .child(progress_bar(self.bar(r), color, t, px(6.))),
            )
            .child(
                // One silhouette: the selected fill slides inside the track, concentric with its corners.
                div()
                    .m_4()
                    .p(px(3.))
                    .rounded(t.r_lg())
                    .bg(t.elevated)
                    .child(
                        div()
                            .relative()
                            .flex()
                            .child(
                                div()
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left(relative(self.tab_pill.value() / n))
                                    .w(relative(1.0 / n))
                                    .rounded(t.r_md())
                                    .bg(t.surface)
                                    .shadow(t.lift()),
                            )
                            .children(tabs.into_iter().enumerate().map(|(i, (tab, label))| {
                                let on = self.tab == tab;
                                div()
                                    .id(("tab", i))
                                    .flex_1()
                                    .min_w_0()
                                    .px_1()
                                    .h(px(26.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(t.r_md())
                                    .text_size(px(11.5))
                                    .when(on, |d| d.font_weight(FontWeight::MEDIUM))
                                    .when(!on, |d| {
                                        d.text_color(t.muted).hover(|s| s.text_color(t.text))
                                    })
                                    .child(div().truncate().child(label))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.tab = tab;
                                        this.tab_pill.set(i as f32, 240, theme::motion(cx));
                                        this.fetch_view(cx);
                                        cx.notify();
                                    }))
                            })),
                    ),
            )
            .child(
                div()
                    .id("inspector-body")
                    .flex_1()
                    .min_h_0()
                    // Files scrolls its own virtual list; the other tabs scroll here.
                    .map(|d| match self.tab {
                        Tab::Files => d.flex().flex_col(),
                        _ => d.overflow_y_scroll(),
                    })
                    .px_4()
                    .pb_4()
                    .child(body),
            )
    }

    fn overview(&self, r: &Row, d: &Details, t: &Theme, cx: &Context<Self>) -> Div {
        let kv = |k: &'static str, v: String| field_row(k, v, t);
        let folder = d.folder.clone();
        div()
            .flex()
            .flex_col()
            .when_some(r.error.clone(), |el, e| el.child(error_note(e, t)))
            // A finished torrent's map is a solid square; it only tells you something while pieces arrive.
            .when(!r.finished() || r.state == State::Checking, |el| {
                el.child(piece_matrix(d.haves.clone(), r.fraction(), t.state(r.state), t).mb_2())
            })
            .child(kv("Status", state_label(r)))
            .child(kv("Size", fmt_bytes(r.total)))
            .child(kv("Downloaded", fmt_bytes(r.done)))
            .child(kv("Uploaded", fmt_bytes(r.uploaded)))
            .child(kv("Ratio", format!("{:.2}", r.ratio())))
            .child(field_row(
                "Speed",
                div()
                    .flex()
                    .gap_3()
                    .child(speed_label("arrow-down", r.down_bps, t.down, t))
                    .child(speed_label("arrow-up", r.up_bps, t.up, t)),
                t,
            ))
            .child(kv("Remaining", eta(r).unwrap_or_else(|| "—".into())))
            .child(kv(
                "Pieces",
                format!("{} × {}", d.pieces, fmt_bytes(d.piece_len as u64)),
            ))
            .child(kv("Info hash", d.info_hash.clone()).text_size(px(11.)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py(px(7.))
                    .child(
                        div()
                            .w(px(104.))
                            .flex_none()
                            .text_color(t.muted)
                            .child("Location"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(d.folder.display().to_string()),
                    )
                    .child(
                        icon_button("reveal", "folder", t)
                            .tooltip(tip("Show in Finder"))
                            .on_click(cx.listener(move |_, _, _, cx| cx.reveal_path(&folder))),
                    ),
            )
            .child(
                div()
                    .pt_3()
                    .flex()
                    .gap_2()
                    .when(can_verify(r), |d| {
                        d.child(button("verify", "Verify Local Data", t, false).on_click(
                            cx.listener(|this, _, w, cx| {
                                this.verify_local_data(&VerifyLocalData, w, cx)
                            }),
                        ))
                    })
                    .when(can_reannounce(r), |d| {
                        d.child(
                            button("reannounce", "Ask for More Peers", t, false).on_click(
                                cx.listener(|this, _, w, cx| {
                                    this.ask_for_more_peers(&AskForMorePeers, w, cx)
                                }),
                            ),
                        )
                    }),
            )
    }

    /// A plugin's panel: the view it last rendered for this transfer.
    fn panel(&self, i: usize, r: &Row, t: &Theme, cx: &Context<Self>) -> Div {
        let Some((plugin, panel)) = self.panels.get(i) else {
            return div();
        };
        let view = self
            .view
            .as_ref()
            .filter(|((p, n, id), _)| p == plugin && *n == panel.id && *id == r.id);
        let blocks = match view.map(|(_, v)| v) {
            None => return placeholder("refresh", "Loading…", t),
            Some(Err(e)) => return error_note(e.clone(), t),
            Some(Ok(blocks)) => blocks,
        };
        div()
            .flex()
            .flex_col()
            .children(blocks.iter().enumerate().map(|(n, b)| {
                match b {
                    Block::Heading { text } => div()
                        .pt_3()
                        .pb_1()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(text.clone())
                        .into_any_element(),
                    Block::Text { text } => div()
                        .py_1()
                        .text_color(t.muted)
                        .child(text.clone())
                        .into_any_element(),
                    Block::Field { label, value } => {
                        field_row(label.clone(), value.clone(), t).into_any_element()
                    }
                    Block::Button { label, command } => {
                        let (plugin, command) = (plugin.clone(), command.clone());
                        let click = cx.listener(move |this, _, _, cx| {
                            this.command(plugin.clone(), command.clone(), true, cx)
                        });
                        div()
                            .pt_2()
                            .flex()
                            .child(
                                button(("panel-button", n), label.clone(), t, false)
                                    .on_click(click),
                            )
                            .into_any_element()
                    }
                    Block::Unknown => div().into_any_element(),
                }
            }))
    }

    fn files(&self, r: &Row, d: &Details, t: &Theme, cx: &Context<Self>) -> Div {
        let id = r.id;
        let wanted: u64 = d.files.iter().filter(|f| f.wanted).map(|f| f.len).sum();
        let all = d.files.iter().all(|f| f.wanted);
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .pb_2()
                    .child(div().flex_1().text_color(t.muted).child(format!(
                        "{} files · {} selected",
                        d.files.len(),
                        fmt_bytes(wanted)
                    )))
                    .when(!all, |el| {
                        el.child(
                            button("all-files", "Select All", t, false).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.engine.set_all_files_wanted(id);
                                    if let Some(d) = &mut this.details {
                                        d.files.iter_mut().for_each(|f| f.wanted = true);
                                    }
                                    cx.notify();
                                },
                            )),
                        )
                    }),
            )
            // Only files in view are built: a torrent can hold tens of thousands, and this redraws while bars move.
            .child(
                uniform_list(
                    "files",
                    d.files.len(),
                    cx.processor(move |this, range: Range<usize>, window, cx| {
                        let t = Theme::of(window, cx);
                        let streaming = this.engine.settings().streaming;
                        let files = this.details.as_ref().map_or(&[][..], |d| &d.files[..]);
                        let range = range.start.min(files.len())..range.end.min(files.len());
                        files[range.clone()]
                            .iter()
                            .zip(range)
                            .map(|(f, pos)| this.file_row(id, pos, f, streaming, &t, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .w_full(),
            )
    }

    fn file_row(
        &self,
        id: usize,
        pos: usize,
        f: &FileRow,
        streaming: bool,
        t: &Theme,
        cx: &Context<Self>,
    ) -> Div {
        let (index, wanted) = (f.index, f.wanted);
        let play = (streaming && crate::stream::playable(&f.name)).then(|| f.name.clone());
        let path = Path::new(&f.name);
        let name = path
            .file_name()
            .map_or(f.name.clone(), |n| n.to_string_lossy().into_owned());
        let dir = path
            .parent()
            .map(|p| p.display().to_string())
            .filter(|p| !p.is_empty());
        let frac = if f.len == 0 {
            1.0
        } else {
            f.done as f32 / f.len as f32
        };
        div()
            .w_full()
            .flex()
            .gap_2()
            .py_2()
            .border_t_1()
            .border_color(t.border)
            .child(
                checkbox(("file", index), wanted, t)
                    .mt(px(1.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.engine.set_file_wanted(id, index, !wanted);
                        if let Some(f) = this.details.as_mut().and_then(|d| d.files.get_mut(pos)) {
                            f.wanted = !wanted;
                        }
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .when(!wanted, |el| el.opacity(0.5))
                    // Play sits beside the name, not in its own column, so every row's bar runs full width.
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().flex_1().min_w_0().truncate().child(name))
                            .when_some(play, |el, name| {
                                el.child(
                                    icon_button(("play-file", index), "play", t)
                                        .size(px(22.))
                                        // Overhangs the text line so rows with and without it stay the same height.
                                        .my(px(-4.))
                                        .tooltip(tip("Play while downloading"))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if !wanted {
                                                this.engine.set_file_wanted(id, index, true);
                                            }
                                            this.engine.play(id, index, &name);
                                            cx.notify();
                                        })),
                                )
                            }),
                    )
                    // Always there, at a fixed height, so every row is as tall as the list assumes.
                    .child(
                        div()
                            .h(px(14.))
                            .line_height(px(14.))
                            .truncate()
                            .text_size(px(10.5))
                            .text_color(t.faint)
                            .child(dir.unwrap_or_default()),
                    )
                    .child(progress_bar(
                        frac,
                        if frac >= 1.0 { t.up } else { t.down },
                        t,
                        px(3.),
                    ))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .text_size(px(10.5))
                            .text_color(t.muted)
                            .child(fmt_bytes(f.len))
                            .child(pct(frac)),
                    ),
            )
    }

    fn peers(&self, d: &Details, t: &Theme) -> Div {
        if d.peers.is_empty() {
            return placeholder("users", "No connected peers", t);
        }
        let mut peers: Vec<_> = d
            .peers
            .iter()
            .map(|p| (p, self.peer_speed.get(&p.addr).copied().unwrap_or_default()))
            .collect();
        // Most bytes exchanged first, then by address: per-second speeds (and the snapshot's HashMap order)
        // would reshuffle the list every tick.
        peers.sort_by(|(a, _), (b, _)| {
            (b.downloaded + b.uploaded)
                .cmp(&(a.downloaded + a.uploaded))
                .then_with(|| a.addr.cmp(&b.addr))
        });
        let radar = peer_radar(peers.iter().map(|(p, (d, u))| (p.addr.as_str(), *d, *u)), t);
        let speed = |color, name, bps| {
            div()
                .flex()
                .items_center()
                .gap_0p5()
                .text_color(color)
                .child(icon(name, px(10.), color))
                .child(fmt_speed(bps))
        };
        div()
            .flex()
            .flex_col()
            .child(radar)
            .children(peers.into_iter().map(|(p, (down, up))| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .py_2()
                    .border_b_1()
                    .border_color(t.border)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(!p.flag.is_empty(), |el| el.child(p.flag.clone()))
                            .child(div().flex_1().min_w_0().truncate().child(p.addr.clone()))
                            .when(!p.kind.is_empty(), |el| {
                                el.child(pill(p.kind.clone(), t.accent, t))
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .text_size(px(10.5))
                            .text_color(t.muted)
                            .child(div().flex_1().min_w_0().truncate().child(
                                if p.client.is_empty() {
                                    "Unknown client".to_string()
                                } else {
                                    p.client.clone()
                                },
                            ))
                            .child(speed(t.down, "arrow-down", down))
                            .child(speed(t.up, "arrow-up", up)),
                    )
            }))
    }
}

fn trackers(d: &Details, s: &Settings, t: &Theme) -> Div {
    let list = div().flex().flex_col().children(d.trackers.iter().map(|u| {
        let host = url::Url::parse(u)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_else(|| u.clone());
        div()
            .flex()
            .gap_2()
            .py_2()
            .border_b_1()
            .border_color(t.border)
            .child(icon("globe", px(14.), t.faint).mt(px(1.)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().truncate().font_weight(FontWeight::MEDIUM).child(host))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(10.5))
                            .text_color(t.faint)
                            .child(u.clone()),
                    ),
            )
    }));
    div()
        .flex()
        .flex_col()
        .when(d.trackers.is_empty(), |el| {
            el.child(placeholder("globe", "No trackers", t))
        })
        .child(list)
        .child(
            div()
                .pt_3()
                .text_size(px(10.5))
                .text_color(t.faint)
                .child(match (s.dht, s.lsd) {
                    (true, true) => "DHT and local peer discovery are on.",
                    (true, false) => "DHT is on; local peer discovery is off.",
                    (false, true) => "DHT is off; local peer discovery is on.",
                    (false, false) => "DHT and local peer discovery are off.",
                }),
        )
}

/// A label and its value, as in the Overview tab.
fn field_row(k: impl Into<SharedString>, v: impl IntoElement, t: &Theme) -> Div {
    div()
        .flex()
        .gap_3()
        .py(px(7.))
        .border_b_1()
        .border_color(t.border)
        .child(
            div()
                .w(px(104.))
                .flex_none()
                .text_color(t.muted)
                .child(k.into()),
        )
        .child(div().flex_1().min_w_0().child(v))
}

fn error_note(e: String, t: &Theme) -> Div {
    div()
        .mb_3()
        .p_3()
        .rounded(t.r_lg())
        .bg(t.tint(t.error))
        .text_color(t.error)
        .flex()
        .gap_2()
        .child(icon("alert", px(14.), t.error))
        .child(div().flex_1().child(e))
}

/// Not while it's still being fetched or already checking.
fn can_verify(r: &Row) -> bool {
    !matches!(r.state, State::Metadata | State::Checking)
}

/// Only running torrents talk to trackers.
fn can_reannounce(r: &Row) -> bool {
    matches!(r.state, State::Downloading | State::Seeding)
}

fn placeholder(name: &str, text: &'static str, t: &Theme) -> Div {
    div()
        .py_8()
        .flex()
        .flex_col()
        .items_center()
        .gap_2()
        .text_color(t.faint)
        .child(icon(name, px(22.), t.faint))
        .child(text)
}

/// A magnet's `dn` (display name), else its info hash, else the URL itself.
fn url_name(url: &str) -> String {
    let Ok(u) = url::Url::parse(url) else {
        return url.to_owned();
    };
    let param = |key: &str| {
        u.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
    };
    param("dn")
        .or_else(|| param("xt").map(|xt| xt.trim_start_matches("urn:btih:").to_owned()))
        .unwrap_or_else(|| url.to_owned())
}

pub fn speed_label(name: &str, bps: f64, color: gpui::Hsla, t: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .text_size(px(11.))
        .text_color(if bps >= 1.0 { t.text } else { t.faint })
        .child(icon(name, px(11.), color))
        .child(fmt_speed(bps))
}

/// Right-aligned, wide enough for "999.9 MB/s".
pub fn speed_slot(label: Div) -> Div {
    div()
        .w(px(84.))
        .flex_none()
        .flex()
        .justify_end()
        .child(label)
}

/// Empty area under the transparent title bar that moves the window (double-click zooms).
pub fn drag_region(height: gpui::Pixels) -> Stateful<Div> {
    div()
        .id("drag")
        .h(height)
        .flex_none()
        .on_mouse_down(MouseButton::Left, |ev, window, _| {
            if ev.click_count == 1 {
                window.start_window_move();
            }
        })
        .on_click(|ev, window, _| {
            if ev.click_count() == 2 {
                window.titlebar_double_click();
            }
        })
}

impl Render for MainWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::of(window, cx);
        // Derived from what the panel can show; set() is a no-op unless the target changes.
        let shown = self.inspector && self.selected_row().is_some() && self.details.is_some();
        self.panel.set(if shown { 1.0 } else { 0.0 }, 220, t.motion);
        let v = self.panel.value();
        let inspector = self
            .selected_row()
            .zip(self.details.as_ref())
            .filter(|_| v > 0.01)
            .map(|(r, d)| {
                self.inspector(r, d, &t, cx)
                    .opacity(v)
                    .relative()
                    .left(px((1.0 - v) * 16.0))
            });
        if self.animating() {
            window.request_animation_frame();
        }

        div()
            .key_context("MainWindow")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::open_torrent))
            .on_action(cx.listener(Self::add_from_clipboard))
            .on_action(cx.listener(Self::toggle_pause))
            .on_action(cx.listener(Self::remove))
            .on_action(cx.listener(Self::remove_and_delete_data))
            .on_action(cx.listener(Self::verify_local_data))
            .on_action(cx.listener(Self::ask_for_more_peers))
            .on_action(cx.listener(Self::toggle_inspector))
            .on_action(cx.listener(Self::run_command))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.step_selection(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.step_selection(-1, cx)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                paths.paths().iter().for_each(|p| this.add_file(p));
                this.refresh(cx);
            }))
            .size_full()
            .flex()
            .font_family(t.font.clone())
            .text_size(px(12.))
            .text_color(t.text)
            .child(self.sidebar(&t, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(t.bg)
                    .border_l_1()
                    .border_color(t.border)
                    .child(self.header(&t, cx))
                    .child(self.list(&t, cx))
                    .when_some(self.error_line(), |d, e| {
                        d.child(
                            div()
                                .id("error")
                                .m_2()
                                .mt_0()
                                .px_3()
                                .py_2()
                                .rounded(t.r_lg())
                                .flex()
                                .items_center()
                                .gap_2()
                                .bg(t.tint(t.error))
                                .text_color(t.error)
                                .child(icon("alert", px(14.), t.error))
                                .child(div().flex_1().min_w_0().truncate().child(e))
                                .child(icon("x", px(13.), t.error))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let mut errors = this.engine.errors.lock().unwrap();
                                    if !errors.is_empty() {
                                        errors.remove(0);
                                    }
                                    cx.notify();
                                })),
                        )
                    }),
            )
            .children(inspector)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn url_name() {
        let h = "magnet:?xt=urn:btih:dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c";
        assert_eq!(
            super::url_name(&format!("{h}&dn=Big+Buck%20Bunny")),
            "Big Buck Bunny"
        );
        assert_eq!(
            super::url_name(h),
            "dd8255ecdc7ca55fb0bbf81323d87062db1f6d1c"
        );
        assert_eq!(
            super::url_name("https://x.org/a.torrent"),
            "https://x.org/a.torrent"
        );
    }
}
