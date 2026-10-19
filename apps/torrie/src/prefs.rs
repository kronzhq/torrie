//! Settings window. Every change is applied and saved immediately.

use std::sync::Arc;

use gpui::{
    AnyElement, ClipboardItem, Context, Div, ElementId, FocusHandle, FontWeight, Hsla,
    KeyDownEvent, MouseButton, PathPromptOptions, Rgba, SharedString, Stateful, Subscription,
    Window, div, prelude::*, px, relative,
};
use serde_json::Value;

use crate::{
    engine::{Appearance, Engine, Settings},
    plugins::{self, Kind, Plugin, Setting},
    theme::{self, *},
    window::drag_region,
};

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Port,
    Peers,
    Down,
    Up,
    Ratio,
    AltDown,
    AltUp,
    AltBegin,
    AltEnd,
    RpcPort,
}

impl Field {
    const ALL: [Field; 10] = [
        Field::Port,
        Field::Peers,
        Field::Down,
        Field::Up,
        Field::Ratio,
        Field::AltDown,
        Field::AltUp,
        Field::AltBegin,
        Field::AltEnd,
        Field::RpcPort,
    ];

    fn get(self, s: &Settings) -> String {
        match self {
            Field::Port => s.peer_port.to_string(),
            Field::Peers => s.peer_limit_per_torrent.to_string(),
            Field::Down => s.speed_limit_down_kbps.to_string(),
            Field::Up => s.speed_limit_up_kbps.to_string(),
            Field::Ratio => format!("{:.2}", s.ratio_limit),
            Field::AltDown => s.alt_speed_down_kbps.to_string(),
            Field::AltUp => s.alt_speed_up_kbps.to_string(),
            Field::AltBegin => hhmm(s.alt_speed_begin),
            Field::AltEnd => hhmm(s.alt_speed_end),
            Field::RpcPort => s.rpc_port.to_string(),
        }
    }

    /// Parses typed input (`Settings::sanitize` clamps it); `None` leaves the setting unchanged.
    fn set(self, s: &mut Settings, text: &str) -> Option<()> {
        let kbps = || text.parse::<u32>().ok();
        match self {
            Field::Port => s.peer_port = text.parse::<u16>().ok().filter(|&p| p > 0)?,
            Field::Peers => s.peer_limit_per_torrent = text.parse::<usize>().ok()?,
            Field::Down => s.speed_limit_down_kbps = kbps()?,
            Field::Up => s.speed_limit_up_kbps = kbps()?,
            Field::Ratio => s.ratio_limit = text.parse::<f64>().ok()?,
            Field::AltDown => s.alt_speed_down_kbps = kbps()?,
            Field::AltUp => s.alt_speed_up_kbps = kbps()?,
            Field::AltBegin => s.alt_speed_begin = parse_time(text)?,
            Field::AltEnd => s.alt_speed_end = parse_time(text)?,
            Field::RpcPort => s.rpc_port = text.parse::<u16>().ok().filter(|&p| p > 0)?,
        }
        Some(())
    }
}

const TABS: [&str; 5] = ["General", "Appearance", "Bandwidth", "Network", "Plugins"];
pub const PLUGINS_TAB: usize = 4;

/// What a text field edits: one of Torrie's settings, or a plugin's (plugin id, setting key).
#[derive(Clone, PartialEq)]
enum Target {
    Builtin(Field),
    Plugin(String, String),
}

fn hhmm(minutes: u16) -> String {
    format!("{:02}:{:02}", minutes / 60, minutes % 60)
}

/// gpui's name for the platform UI font (SF Pro on macOS).
const SYSTEM_FONT: &str = ".SystemUIFont";

fn font_label(family: &str) -> &str {
    if family == SYSTEM_FONT {
        "System (SF Pro)"
    } else {
        family
    }
}

/// Minutes after midnight from `9`, `9:30`, `0930` or `21:05`.
fn parse_time(text: &str) -> Option<u16> {
    let (h, m) = match text.split_once(':') {
        Some((h, m)) => (h.parse::<u16>().ok()?, m.parse::<u16>().ok()?),
        None if text.len() <= 2 => (text.parse().ok()?, 0),
        None => {
            let n: u16 = text.parse().ok()?;
            (n / 100, n % 100)
        }
    };
    (h < 24 && m < 60).then_some(h * 60 + m)
}

pub struct Preferences {
    engine: Arc<Engine>,
    settings: Settings,
    /// Field being typed into, its text, and whether the next key replaces it (like a selected field).
    editing: Option<(Target, String, bool)>,
    /// Read when the window opens and on Reload.
    plugins: Vec<(String, Result<Plugin, String>)>,
    focus: Vec<FocusHandle>,
    /// Shared by plugins' text settings: clicking from one into another commits the first before focusing.
    plugin_focus: FocusHandle,
    /// Index into `TABS`.
    tab: usize,
    /// The selected tab's sliding fill, by index.
    tab_pill: Tween,
    /// System font families while the font list is open.
    fonts: Option<Vec<SharedString>>,
    _subs: Vec<Subscription>,
}

impl Preferences {
    pub fn new(
        engine: Arc<Engine>,
        tab: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus: Vec<FocusHandle> = Field::ALL.iter().map(|_| cx.focus_handle()).collect();
        // Only commits its own field: when clicking from one field into another, this fires after the new edit began.
        let mut _subs: Vec<Subscription> = (focus.iter().zip(Field::ALL))
            .map(|(h, field)| {
                cx.on_blur(h, window, move |this, _, cx| {
                    if this
                        .editing
                        .as_ref()
                        .is_some_and(|(f, _, _)| *f == Target::Builtin(field))
                    {
                        this.commit(cx);
                    }
                })
            })
            .collect();
        let plugin_focus = cx.focus_handle();
        _subs.push(cx.on_blur(&plugin_focus, window, |this, _, cx| {
            if matches!(this.editing, Some((Target::Plugin(..), _, _))) {
                this.commit(cx);
            }
        }));
        // Pick up theme files edited since launch.
        theme::apply(&engine.settings(), cx);
        Self {
            settings: engine.settings(),
            engine,
            editing: None,
            plugins: plugins::list(),
            focus,
            plugin_focus,
            tab,
            tab_pill: Tween::new(tab as f32),
            fonts: None,
            _subs,
        }
    }

    pub fn show_tab(&mut self, tab: usize, cx: &mut Context<Self>) {
        self.tab = tab;
        self.plugins = plugins::list();
        cx.notify();
    }

    fn update(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        self.engine.update_settings(f);
        self.settings = self.engine.settings();
        theme::apply(&self.settings, cx);
        crate::macos::apply(&self.settings, cx);
        cx.notify();
    }

    fn commit(&mut self, cx: &mut Context<Self>) {
        match self.editing.take() {
            Some((Target::Builtin(field), text, _)) => self.update(cx, |s| _ = field.set(s, &text)),
            // Untouched: committing would restart the plugin for nothing.
            Some((Target::Plugin(id, key), text, false)) => {
                self.set_plugin(id, key, Value::String(text), cx)
            }
            _ => {}
        }
        cx.notify();
    }

    /// Saves a plugin's setting and restarts its plugin process, which reads settings when it starts.
    fn set_plugin(&mut self, id: String, key: String, v: Value, cx: &mut Context<Self>) {
        if self
            .settings
            .plugin_settings
            .get(&id)
            .and_then(|m| m.get(&key))
            == Some(&v)
        {
            return;
        }
        self.update(cx, |s| {
            _ = s
                .plugin_settings
                .entry(id.clone())
                .or_default()
                .insert(key, v)
        });
        plugins::restart(&self.engine, &id);
    }

    fn key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some((target, text, fresh)) = &mut self.editing else {
            return;
        };
        match ev.keystroke.key.as_str() {
            "enter" | "tab" => window.blur(),
            "escape" => {
                self.editing = None;
                window.blur();
            }
            "backspace" => {
                if *fresh {
                    text.clear()
                } else {
                    _ = text.pop()
                }
                *fresh = false;
            }
            _ => {
                let plugin = matches!(target, Target::Plugin(..));
                let time = matches!(target, Target::Builtin(Field::AltBegin | Field::AltEnd));
                let ratio = *target == Target::Builtin(Field::Ratio);
                let ok = |c: char| {
                    (plugin && !c.is_control())
                        || c.is_ascii_digit()
                        || (c == '.' && ratio)
                        || (c == ':' && time)
                };
                let paste = ev.keystroke.modifiers.platform && ev.keystroke.key == "v";
                let typed = if paste {
                    cx.read_from_clipboard().and_then(|c| c.text())
                } else {
                    ev.keystroke.key_char.clone()
                };
                if let Some(ch) = typed.filter(|c| c.chars().all(ok)) {
                    if *fresh {
                        text.clear();
                        *fresh = false;
                    }
                    if text.chars().count() + ch.chars().count() <= if plugin { 1000 } else { 9 } {
                        text.push_str(&ch);
                    }
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn field(
        &self,
        field: Field,
        suffix: &'static str,
        enabled: bool,
        t: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let i = Field::ALL.iter().position(|f| *f == field).unwrap();
        let input = Input {
            target: Target::Builtin(field),
            value: field.get(&self.settings),
            id: ("field", i).into(),
            focus: self.focus[i].clone(),
        };
        self.input(input, suffix, enabled, t, cx)
    }

    fn input(
        &self,
        input: Input,
        suffix: &'static str,
        enabled: bool,
        t: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Input {
            target,
            value,
            id,
            focus,
        } = input;
        let editing = self.editing.as_ref().filter(|(f, _, _)| *f == target);
        let text = editing.map_or_else(|| value.clone(), |(_, s, _)| s.clone());
        let fresh = editing.is_some_and(|(_, _, fresh)| *fresh);
        // Plugins' text settings are wider, and show the end of what doesn't fit (where typing happens).
        let wide = matches!(target, Target::Plugin(..));
        div()
            .id(id)
            .track_focus(&focus)
            .on_key_down(cx.listener(Self::key))
            // Keeps the window's click-outside blur from ending the edit being clicked into.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .h(px(28.))
            .min_w(px(92.))
            .px_2()
            .flex()
            .flex_none()
            .items_center()
            .justify_end()
            .gap_1()
            .rounded(t.r_md())
            .bg(t.elevated)
            .border_1()
            .border_color(if editing.is_some() {
                t.accent
            } else {
                t.border
            })
            .cursor_text()
            .when(wide, |d| d.w(px(200.)).overflow_hidden())
            .when(!enabled, |d| d.opacity(0.45))
            .child(
                div()
                    .flex_none()
                    .whitespace_nowrap()
                    .px(px(2.))
                    .rounded(px(3.))
                    .when(fresh, |d| d.bg(t.tint(t.accent)))
                    .child(text),
            )
            .when(editing.is_some() && !fresh, |d| {
                d.child(div().w(px(1.5)).h(px(14.)).bg(t.accent))
            })
            .when(!suffix.is_empty(), |d| {
                d.child(div().text_color(t.faint).child(suffix))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.editing.as_ref().is_none_or(|(f, _, _)| *f != target) {
                    this.commit(cx);
                    this.editing = Some((target.clone(), value.clone(), true));
                    focus.focus(window);
                    cx.notify();
                }
            }))
            .into_any_element()
    }

    /// Direction marker next to a speed field, dimmed with it.
    fn arrow(&self, name: &str, enabled: bool, t: &Theme) -> impl IntoElement {
        div()
            .when(!enabled, |d| d.opacity(0.45))
            .child(icon(name, px(12.), t.faint))
    }

    fn switch(
        &self,
        id: &'static str,
        on: bool,
        t: &Theme,
        cx: &Context<Self>,
        f: fn(&mut Settings, bool),
    ) -> AnyElement {
        toggle(id, on, t)
            .on_click(cx.listener(move |this, _, _, cx| this.update(cx, |s| f(s, !on))))
            .into_any_element()
    }

    fn appearance(&self, t: &Theme, cx: &Context<Self>) -> Div {
        let s = &self.settings;
        let themes = cx.global::<Themes>();
        let modes = [
            (Appearance::System, "System"),
            (Appearance::Light, "Light"),
            (Appearance::Dark, "Dark"),
        ];
        let appearance = row("Appearance", "Light or dark mode; System follows macOS", t).child(
            div().flex().gap_1().children(modes.map(|(a, label)| {
                button(label, label, t, s.appearance == a).on_click(
                    cx.listener(move |this, _, _, cx| this.update(cx, |s| s.appearance = a)),
                )
            })),
        );
        // One row of cards per mode.
        let cards = |dark: bool| {
            let chosen = if dark { &s.dark_theme } else { &s.light_theme };
            div().flex().flex_wrap().gap_3().children(
                themes
                    .list
                    .iter()
                    .filter(|(_, th)| th.dark == dark)
                    .map(|(name, th)| {
                        let name = name.clone();
                        theme_card(&name, preview(th).size_full(), *chosen == name, t).on_click(
                            cx.listener(move |this, _, _, cx| {
                                let name = name.clone();
                                this.update(cx, |s| match dark {
                                    true => s.dark_theme = name,
                                    false => s.light_theme = name,
                                })
                            }),
                        )
                    }),
            )
        };
        let label = |text: &'static str| div().text_color(t.muted).child(text);

        let swatch = |id: &'static str, color: Option<Hsla>, on: bool| {
            div()
                .id(id)
                .size(px(22.))
                .p(px(2.))
                .rounded_full()
                .border_2()
                .border_color(if on {
                    t.text
                } else {
                    gpui::transparent_black()
                })
                .child(match color {
                    Some(c) => div().size_full().rounded_full().bg(c),
                    // Theme default: a ring in the theme's own accent.
                    None => div()
                        .size_full()
                        .rounded_full()
                        .border_2()
                        .border_color(t.faint),
                })
        };
        let accents = div()
            .flex()
            .items_center()
            .gap_1()
            .child(div().w(px(64.)).text_color(t.muted).child("Accent"))
            .child(
                swatch("accent-theme", None, s.accent.is_none())
                    .tooltip(tip("Theme default"))
                    .on_click(cx.listener(|this, _, _, cx| this.update(cx, |s| s.accent = None))),
            )
            .children(ACCENTS.map(|hex| {
                let color = Rgba::try_from(hex).ok().map(Hsla::from);
                let on = s
                    .accent
                    .as_deref()
                    .is_some_and(|a| a.eq_ignore_ascii_case(hex));
                swatch(hex, color, on).on_click(cx.listener(move |this, _, _, cx| {
                    this.update(cx, |s| s.accent = Some(hex.into()))
                }))
            }));

        let font = row(
            "Font",
            s.font.as_deref().map_or_else(
                || format!("Theme default ({})", font_label(&t.font)),
                |f| font_label(f).into(),
            ),
            t,
        )
        .child(
            button(
                "font-choose",
                if self.fonts.is_some() {
                    "Done"
                } else {
                    "Choose…"
                },
                t,
                false,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.fonts = match this.fonts {
                    Some(_) => None,
                    None => Some(
                        cx.text_system()
                            .all_font_names()
                            .into_iter()
                            // Dot-prefixed families are macOS internals.
                            .filter(|f| !f.starts_with('.'))
                            .map(Into::into)
                            .collect(),
                    ),
                };
                cx.notify();
            })),
        );
        let font_list = self.fonts.as_ref().map(|fonts| {
            let item = |id: usize, label: SharedString, family: Option<SharedString>| {
                let on = s.font.as_deref() == family.as_ref().map(|f| f.as_ref());
                div()
                    .id(("font", id))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .py_1()
                    .when_some(family.clone(), |d, f| d.font_family(f))
                    .hover(|d| d.bg(t.hover))
                    .child(div().flex_1().truncate().child(label))
                    .when(on, |d| d.child(icon("check", px(12.), t.accent)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let f = family.as_ref().map(|f| f.to_string());
                        this.update(cx, |s| s.font = f)
                    }))
            };
            div()
                .id("font-list")
                .max_h(px(240.))
                .py_1()
                .overflow_y_scroll()
                .child(item(0, "Theme default".into(), None))
                .child(item(
                    1,
                    font_label(SYSTEM_FONT).into(),
                    Some(SYSTEM_FONT.into()),
                ))
                .children(
                    fonts
                        .iter()
                        .enumerate()
                        .map(|(i, f)| item(i + 2, f.clone(), Some(f.clone()))),
                )
        });

        let errors = themes.errors.clone();
        section(
            "APPEARANCE",
            vec![
                appearance,
                div()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(label("Light theme"))
                    .child(cards(false))
                    .child(label("Dark theme"))
                    .child(cards(true))
                    .child(accents),
                font,
            ]
            .into_iter()
            .chain(font_list.map(|l| div().child(l)))
            .chain([
                row("Animations", "Also off while macOS Reduce motion is on", t)
                    .child(self.switch("animations", s.animations, t, cx, |s, v| s.animations = v)),
                row(
                    "Custom themes",
                    "JSON files; set any color, \"font\" or \"radius\" on top of \"extends\"",
                    t,
                )
                .child(
                    button("themes-folder", "Show in Finder", t, false)
                        .on_click(|_, _, cx| cx.reveal_path(&themes_dir().join("Custom.json"))),
                )
                .child(
                    button("themes-reload", "Reload", t, false).on_click(cx.listener(
                        |this, _, _, cx| {
                            theme::apply(&this.settings, cx);
                            cx.notify();
                        },
                    )),
                ),
            ])
            .chain((!errors.is_empty()).then(|| {
                div()
                    .px_4()
                    .py_3()
                    .flex()
                    .gap_2()
                    .text_color(t.error)
                    .child(icon("alert", px(14.), t.error))
                    .child(div().flex_1().flex().flex_col().gap_1().children(errors))
            }))
            .collect(),
            t,
        )
    }

    fn plugins(&self, t: &Theme, cx: &Context<Self>) -> Div {
        let enabled = &self.settings.plugins_enabled;
        let mut rows = vec![];
        for (i, (id, p)) in self.plugins.iter().enumerate() {
            let p = match p {
                Ok(p) => p,
                Err(e) => {
                    rows.push(row(id.clone(), format!("Can't load: {e}"), t).child(icon(
                        "alert",
                        px(14.),
                        t.error,
                    )));
                    continue;
                }
            };
            let m = &p.manifest;
            let on = enabled.contains(&p.id);
            let status = if plugins::running(&p.id) {
                Some("Running".into())
            } else {
                plugins::exited(&p.id).filter(|_| on)
            };
            let by = (!m.author.is_empty()).then(|| format!("by {}", m.author));
            let detail = [Some(m.description.clone()), by, status]
                .into_iter()
                .flatten()
                .filter(|d| !d.is_empty());
            let id = p.id.clone();
            let switch =
                toggle(("plugin", i), on, t).on_click(cx.listener(move |this, _, _, cx| {
                    let id = id.clone();
                    this.update(cx, move |s| {
                        if on {
                            s.plugins_enabled.retain(|p| *p != id)
                        } else {
                            s.plugins_enabled.push(id)
                        }
                    });
                    crate::plugins_changed(&this.engine, cx);
                }));
            rows.push(
                row(
                    format!("{} {}", m.name, m.version),
                    detail.collect::<Vec<_>>().join(" · "),
                    t,
                )
                .child(switch),
            );
            rows.extend(
                m.contributes
                    .settings
                    .iter()
                    .map(|set| self.plugin_setting(p, set, t, cx)),
            );
        }
        let empty = rows.is_empty().then(|| {
            row(
                "No plugins",
                "Open a .torrieplugin file or an install link to add one",
                t,
            )
        });
        let footer = row(
            "Plugins folder",
            "Plugins run as you; only switch on ones you trust",
            t,
        )
        .child(
            button("plugins-folder", "Show in Finder", t, false)
                .on_click(|_, _, cx| cx.reveal_path(&plugins::dir())),
        )
        .child(
            button("plugins-reload", "Reload", t, false).on_click(cx.listener(|this, _, _, cx| {
                // Restarts every plugin process too, so edits to a plugin take effect.
                plugins::stop_all();
                crate::plugins_changed(&this.engine, cx);
                this.plugins = plugins::list();
                cx.notify();
            })),
        );
        section(
            "PLUGINS",
            rows.into_iter().chain(empty).chain([footer]).collect(),
            t,
        )
    }

    /// One of a plugin's settings, indented under it.
    fn plugin_setting(&self, p: &Plugin, set: &Setting, t: &Theme, cx: &Context<Self>) -> Div {
        let value = plugins::text(&plugins::value(&p.id, set, &self.settings));
        let (id, key) = (p.id.clone(), set.key.clone());
        let el_id = SharedString::from(format!("setting-{id}-{key}"));
        match set.kind {
            Kind::Boolean => {
                let on = value == "true";
                row(set.title.clone(), set.description.clone(), t)
                    .pl_8()
                    .child(
                        toggle(el_id, on, t).on_click(cx.listener(move |this, _, _, cx| {
                            this.set_plugin(id.clone(), key.clone(), Value::Bool(!on), cx)
                        })),
                    )
            }
            Kind::String => {
                let input = Input {
                    target: Target::Plugin(id, key),
                    value,
                    id: el_id.into(),
                    focus: self.plugin_focus.clone(),
                };
                row(set.title.clone(), set.description.clone(), t)
                    .pl_8()
                    .child(self.input(input, "", true, t, cx))
            }
            Kind::Folder => {
                let detail = if value.is_empty() {
                    set.description.clone()
                } else {
                    value
                };
                row(set.title.clone(), detail, t).pl_8().child(
                    button(el_id, "Choose…", t, false).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.choose_folder(Some((id.clone(), key.clone())), cx)
                        },
                    )),
                )
            }
        }
    }

    /// Picks the download folder, or with `plugin` (plugin id, setting key), a plugin's folder setting.
    fn choose_folder(&mut self, plugin: Option<(String, String)>, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(mut paths))) = paths.await
                && let Some(dir) = paths.pop()
            {
                this.update(cx, |this, cx| match plugin {
                    Some((id, key)) => this.set_plugin(
                        id,
                        key,
                        Value::String(dir.to_string_lossy().into_owned()),
                        cx,
                    ),
                    None => this.update(cx, |s| s.download_dir = dir),
                })
                .ok();
            }
        })
        .detach();
    }
}

impl Render for Preferences {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::of(window, cx);
        // Follows `tab` wherever it's set (clicks, show_tab); set() is a no-op unless the target changes.
        self.tab_pill.set(self.tab as f32, 240, t.motion);
        if self.tab_pill.moving() {
            window.request_animation_frame();
        }
        let s = &self.settings;
        let port_detail = match self.engine.listen_port() {
            Some(p) if p != s.peer_port => {
                format!("Port {} was busy; listening on {p}", s.peer_port)
            }
            Some(p) => format!("Listening on {p}"),
            None => "Not listening".into(),
        };

        let general =
            || {
                section(
                    "GENERAL",
                    vec![
                        row(
                            "Show in menu bar",
                            "Keeps Torrie running when its window is closed",
                            &t,
                        )
                        .child(self.switch(
                            "menu-bar",
                            s.menu_bar,
                            &t,
                            cx,
                            |s, v| s.menu_bar = v,
                        )),
                        row(
                            "Open at login",
                            "Starts Torrie when you log in to your Mac",
                            &t,
                        )
                        .child(self.switch(
                            "login",
                            s.open_at_login,
                            &t,
                            cx,
                            |s, v| s.open_at_login = v,
                        )),
                        row(
                            "Menu bar mode",
                            "When opened at login, starts in the menu bar without its window",
                            &t,
                        )
                        .child(self.switch(
                            "menu-bar-mode",
                            s.menu_bar_mode,
                            &t,
                            cx,
                            |s, v| s.menu_bar_mode = v,
                        )),
                        row(
                            "Check for updates automatically",
                            format!("Torrie {}", env!("CARGO_PKG_VERSION")),
                            &t,
                        )
                        .child(button("check-updates", "Check Now", &t, false).on_click(
                            |_, _, cx| cx.dispatch_action(&crate::window::CheckForUpdates),
                        ))
                        .child(self.switch(
                            "updates",
                            s.check_updates,
                            &t,
                            cx,
                            |s, v| s.check_updates = v,
                        )),
                    ],
                    &t,
                )
            };
        let transfers = || {
            section(
                "TRANSFERS",
                vec![
                    row("Download to", s.download_dir.display().to_string(), &t).child(
                        button("choose", "Choose…", &t, false)
                            .on_click(cx.listener(|this, _, _, cx| this.choose_folder(None, cx))),
                    ),
                    row("Peers per torrent", "Applies to newly added torrents", &t)
                        .child(self.field(Field::Peers, "", true, &t, cx)),
                    row(
                        "Watch while downloading",
                        "Play buttons on video and audio in the Files tab",
                        &t,
                    )
                    .child(self.switch(
                        "streaming",
                        s.streaming,
                        &t,
                        cx,
                        |s, v| s.streaming = v,
                    )),
                    row(
                        "Keep Mac awake while downloading",
                        "Your Mac won't sleep on its own until downloads finish",
                        &t,
                    )
                    .child(self.switch(
                        "keep-awake",
                        s.keep_awake,
                        &t,
                        cx,
                        |s, v| s.keep_awake = v,
                    )),
                    row(
                        "Notify when downloads finish",
                        "Shows a notification for each finished download",
                        &t,
                    )
                    .child(self.switch(
                        "notify-done",
                        s.notify_done,
                        &t,
                        cx,
                        |s, v| s.notify_done = v,
                    )),
                ],
                &t,
            )
        };
        let bandwidth = || {
            section(
                "BANDWIDTH",
                vec![
                    row("Limit download speed", "Applies to all transfers", &t)
                        .child(self.field(Field::Down, "kB/s", s.speed_limit_down_enabled, &t, cx))
                        .child(
                            self.switch("down", s.speed_limit_down_enabled, &t, cx, |s, v| {
                                s.speed_limit_down_enabled = v
                            }),
                        ),
                    row("Limit upload speed", "Applies to all transfers", &t)
                        .child(self.field(Field::Up, "kB/s", s.speed_limit_up_enabled, &t, cx))
                        .child(self.switch("up", s.speed_limit_up_enabled, &t, cx, |s, v| {
                            s.speed_limit_up_enabled = v
                        })),
                    row(
                        "Alternative limits",
                        "Replace the limits above while on; also the gauge in the sidebar",
                        &t,
                    )
                    .child(self.arrow("arrow-down", s.alt_speed_enabled, &t))
                    .child(self.field(Field::AltDown, "kB/s", s.alt_speed_enabled, &t, cx))
                    .child(self.arrow("arrow-up", s.alt_speed_enabled, &t))
                    .child(self.field(Field::AltUp, "kB/s", s.alt_speed_enabled, &t, cx))
                    .child(self.switch(
                        "alt",
                        s.alt_speed_enabled,
                        &t,
                        cx,
                        |s, v| s.alt_speed_enabled = v,
                    )),
                    row(
                        "Schedule alternative limits",
                        "On from the first time, off at the second",
                        &t,
                    )
                    .child(self.field(Field::AltBegin, "", s.alt_speed_time_enabled, &t, cx))
                    .child(self.field(Field::AltEnd, "", s.alt_speed_time_enabled, &t, cx))
                    .child(self.switch(
                        "alt-time",
                        s.alt_speed_time_enabled,
                        &t,
                        cx,
                        |s, v| s.alt_speed_time_enabled = v,
                    )),
                    row("On these days", "", &t).children(
                        ["S", "M", "T", "W", "T", "F", "S"]
                            .into_iter()
                            .enumerate()
                            .map(|(i, day)| {
                                let on = s.alt_speed_days & (1 << i) != 0;
                                div()
                                    .id(("day", i))
                                    .size(px(26.))
                                    .flex()
                                    .flex_none()
                                    .items_center()
                                    .justify_center()
                                    .rounded_full()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .bg(if on { t.accent } else { t.elevated })
                                    .text_color(if on { gpui::white() } else { t.muted })
                                    .when(!s.alt_speed_time_enabled, |d| d.opacity(0.45))
                                    .child(day)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.update(cx, |s| s.alt_speed_days ^= 1 << i)
                                    }))
                            }),
                    ),
                ],
                &t,
            )
        };
        let seeding = || {
            section(
                "SEEDING",
                vec![
                    row(
                        "Stop seeding at ratio",
                        "Uploaded ÷ size, counted since launch",
                        &t,
                    )
                    .child(self.field(Field::Ratio, "", s.ratio_limit_enabled, &t, cx))
                    .child(self.switch(
                        "ratio",
                        s.ratio_limit_enabled,
                        &t,
                        cx,
                        |s, v| s.ratio_limit_enabled = v,
                    )),
                ],
                &t,
            )
        };
        let network = || {
            section(
                "NETWORK",
                vec![
                    row("Peer listening port", port_detail, &t).child(self.field(
                        Field::Port,
                        "",
                        true,
                        &t,
                        cx,
                    )),
                    row(
                        "Map port with UPnP",
                        "Lets peers reach you through your router",
                        &t,
                    )
                    .child(self.switch(
                        "upnp",
                        s.port_forwarding,
                        &t,
                        cx,
                        |s, v| s.port_forwarding = v,
                    )),
                    row(
                        "Remote control",
                        "Web remote and API for plugins, scripts and other apps",
                        &t,
                    )
                    .child(self.field(Field::RpcPort, "", s.rpc_enabled, &t, cx))
                    .child(self.switch(
                        "rpc",
                        s.rpc_enabled,
                        &t,
                        cx,
                        |s, v| s.rpc_enabled = v,
                    )),
                    row(
                        "Allow other devices",
                        "Phones, Docker and other computers on your network, with the password",
                        &t,
                    )
                    .child(self.switch("lan", s.rpc_lan, &t, cx, |s, v| s.rpc_lan = v)),
                ]
                .into_iter()
                .chain(s.rpc_lan.then(|| {
                    let password = s.rpc_password.clone();
                    row(
                        "Password",
                        format!(
                            "{password} · any username · sent unencrypted, so trusted networks only"
                        ),
                        &t,
                    )
                    .child(
                        button("rpc-copy", "Copy", &t, false).on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(password.clone()))
                        }),
                    )
                    .child(button("rpc-new", "New", &t, false).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.update(cx, |s| s.rpc_password = crate::rpc::new_password())
                        },
                    )))
                }))
                .chain(s.rpc_enabled.then(|| {
                    let host = s.rpc_lan.then(lan_ip).flatten();
                    let url = format!("http://{}:{}", host.as_deref().unwrap_or("localhost"), s.rpc_port);
                    let detail = if host.is_some() {
                        format!("{url} in any browser on your network")
                    } else {
                        format!("{url} in a browser on this Mac")
                    };
                    row("Web remote", detail, &t).child(
                        button("rpc-open", "Open", &t, false)
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                }))
                .collect(),
                &t,
            )
        };
        let body = match self.tab {
            0 => vec![general(), transfers(), seeding()],
            1 => vec![self.appearance(&t, cx)],
            2 => vec![bandwidth()],
            3 => vec![network()],
            _ => vec![self.plugins(&t, cx)],
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            // Clicking anywhere outside a field ends its edit (blur commits it).
            .on_mouse_down(MouseButton::Left, |_, window, _| window.blur())
            .bg(t.bg)
            .font_family(t.font.clone())
            .text_size(px(12.))
            .text_color(t.text)
            .child(
                drag_region(px(52.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(px(13.))
                    .child("Settings"),
            )
            .child(
                // Same as the inspector's: the selected fill slides inside the track.
                div()
                    .mx_6()
                    .mb_4()
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
                                    .left(relative(self.tab_pill.value() / TABS.len() as f32))
                                    .w(relative(1.0 / TABS.len() as f32))
                                    .rounded(t.r_md())
                                    .bg(t.surface)
                                    .shadow(t.lift()),
                            )
                            .children(TABS.iter().enumerate().map(|(i, &label)| {
                                let on = self.tab == i;
                                div()
                                    .id(label)
                                    .flex_1()
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
                                    .child(label)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        window.blur();
                                        this.tab = i;
                                        cx.notify();
                                    }))
                            })),
                    ),
            )
            .child(
                div()
                    .id(("prefs", self.tab))
                    .flex_1()
                    .overflow_y_scroll()
                    .px_6()
                    .pb_6()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .children(body)
                    .when(self.engine.needs_restart(), |d| {
                        d.child(
                            div()
                                .p_3()
                                .rounded(t.r_lg())
                                .flex()
                                .gap_2()
                                .bg(t.tint(t.checking))
                                .text_color(t.checking)
                                .child(icon("info", px(14.), t.checking))
                                .child("Restart Torrie to apply network changes."),
                        )
                    }),
            )
    }
}

/// A text field: what it edits, its current value, and its element id and focus.
struct Input {
    target: Target,
    value: String,
    id: ElementId,
    focus: FocusHandle,
}

fn section(title: &'static str, rows: Vec<Div>, t: &Theme) -> Div {
    let n = rows.len();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .pl_1()
                .text_size(px(10.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(t.faint)
                .child(title),
        )
        .child(
            div()
                .rounded(t.r_xl())
                .bg(t.surface)
                .border_1()
                .border_color(t.border)
                .shadow(t.lift())
                .children(
                    rows.into_iter()
                        .enumerate()
                        .map(|(i, r)| r.when(i + 1 < n, |r| r.border_b_1().border_color(t.border))),
                ),
        )
}

/// This Mac's address on the network: the interface a route out would use (connecting UDP sends nothing).
fn lan_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

fn row(label: impl Into<SharedString>, detail: impl Into<SharedString>, t: &Theme) -> Div {
    div().flex().items_center().gap_4().px_4().py_3().child(
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(3.))
            .child(div().text_size(px(13.)).child(label.into()))
            .child(
                div()
                    .text_size(px(11.5))
                    .line_height(px(15.))
                    .text_color(t.muted)
                    .child(detail.into()),
            ),
    )
}

/// A selectable theme: a miniature of the main window above its name.
fn theme_card(name: &str, preview: Div, on: bool, t: &Theme) -> Stateful<Div> {
    div()
        .id(SharedString::from(format!("theme-{name}")))
        .w(px(140.))
        .flex()
        .flex_col()
        .gap_1p5()
        .child(
            div()
                .h(px(64.))
                .p(px(2.))
                .rounded(t.r_lg())
                .border_2()
                .border_color(if on { t.accent } else { t.border })
                .shadow(t.lift())
                .child(
                    div()
                        .size_full()
                        .rounded(t.r_sm())
                        .overflow_hidden()
                        .child(preview),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_1()
                .text_size(px(11.5))
                .when(on, |d| {
                    d.font_weight(FontWeight::MEDIUM)
                        .child(icon("check", px(12.), t.accent))
                })
                .when(!on, |d| d.text_color(t.muted))
                .child(div().min_w_0().truncate().child(name.to_owned())),
        )
}

/// The theme's own colors (they are the content here, so no tokens): sidebar, two transfers, accent.
fn preview(th: &Theme) -> Div {
    let line = |w: f32, c: Hsla| div().h(px(3.)).w(px(w)).rounded_full().bg(c);
    div()
        .flex()
        .bg(th.bg)
        .child(
            div()
                .w(px(34.))
                .h_full()
                .p_1p5()
                .flex()
                .flex_col()
                .gap_1()
                .bg(th.sidebar)
                .child(line(16., th.accent))
                .child(line(20., th.faint))
                .child(line(14., th.faint)),
        )
        .child(div().flex_1().p_2().flex().flex_col().gap_2().children(
            [(0.7, th.down), (0.35, th.up)].map(|(f, c)| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(line(44., th.text.opacity(0.75)))
                    .child(
                        div()
                            .h(px(3.))
                            .w_full()
                            .rounded_full()
                            .bg(th.elevated)
                            .child(div().h_full().w(relative(f)).rounded_full().bg(c)),
                    )
            }),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(parse_time("9"), Some(540));
        assert_eq!(parse_time("9:30"), Some(570));
        assert_eq!(parse_time("0930"), Some(570));
        assert_eq!(parse_time("23:59"), Some(1439));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("9:60"), None);
        assert_eq!(hhmm(570), "09:30");
    }
}
