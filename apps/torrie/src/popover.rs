//! The panel under the menu bar item: total speeds, plus what's transferring or paused, with pause/resume.

use std::{sync::Arc, time::Duration};

use gpui::{
    App, Bounds, Context, Subscription, Window, WindowBackgroundAppearance, WindowBounds,
    WindowKind, WindowOptions, div, point, prelude::*, px, size,
};

use crate::{
    engine::Engine,
    format::*,
    macos,
    theme::*,
    window::{OpenSettings, Quit, ShowMainWindow, speed_label, speed_slot},
};

const WIDTH: f32 = 300.;
const HEADER: f32 = 40.;
const ITEM: f32 = 46.;
const FOOTER: f32 = 40.;
/// Transfers listed at most; the rest are counted in the header.
const MAX: usize = 5;

/// Active transfers first, then paused or failed ones so they can be resumed from here.
fn listed(rows: &[Row]) -> Vec<&Row> {
    let mut list: Vec<&Row> = rows
        .iter()
        .filter(|r| r.active() || r.resumable())
        .collect();
    list.sort_by_key(|r| !r.active());
    list
}

pub struct Popover {
    engine: Arc<Engine>,
    /// Refreshed once a second, not per render: hovering re-renders.
    rows: Vec<Row>,
    /// List height in items, fixed when opened since the window can't grow.
    slots: usize,
    _close: Subscription,
}

/// Opens the panel under the menu bar item, or closes it if it's open.
pub fn toggle(engine: &Arc<Engine>, cx: &mut App) {
    if let Some(w) = cx
        .windows()
        .into_iter()
        .find_map(|w| w.downcast::<Popover>())
    {
        _ = w.update(cx, |_, window, _| window.remove_window());
        return;
    }
    let Some((display, anchor)) = macos::anchor(cx) else {
        return;
    };
    let rows = engine.rows();
    let slots = listed(&rows).len().clamp(1, MAX);
    let size = size(px(WIDTH), px(HEADER + ITEM * slots as f32 + FOOTER));
    let right = cx.find_display(display).map_or(px(f32::MAX), |d| {
        d.bounds().size.width - size.width - px(8.)
    });
    let origin = point(
        (anchor.x - size.width / 2.).clamp(px(8.), right),
        anchor.y + px(6.),
    );
    let engine = engine.clone();
    _ = cx.open_window(
        WindowOptions {
            titlebar: None,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            display_id: Some(display),
            window_bounds: Some(WindowBounds::Windowed(Bounds { origin, size })),
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        |window, cx| cx.new(|cx| Popover::new(engine, rows, slots, window, cx)),
    );
}

impl Popover {
    fn new(
        engine: Arc<Engine>,
        rows: Vec<Row>,
        slots: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Clicking anywhere else dismisses it, like a native menu bar extra.
        let _close = cx.observe_window_activation(window, |_, window, _| {
            if !window.is_window_active() {
                window.remove_window();
            }
        });
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let tick = |this: &mut Self, cx: &mut Context<Self>| {
                    this.rows = this.engine.rows();
                    cx.notify();
                };
                if this.update(cx, tick).is_err() {
                    break;
                }
            }
        })
        .detach();
        Self {
            engine,
            rows,
            slots,
            _close,
        }
    }
}

fn detail(r: &Row) -> String {
    match r.state {
        State::Metadata => "Fetching info".into(),
        State::Checking => "Checking".into(),
        State::Seeding => fmt_speed(r.up_bps),
        State::Paused => "Paused".into(),
        State::Error => "Error".into(),
        _ => eta(r).unwrap_or_else(|| pct(r.fraction())),
    }
}

impl Render for Popover {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::of(window, cx);
        let rows = &self.rows;
        let (down, up) = rows
            .iter()
            .fold((0.0, 0.0), |(d, u), r| (d + r.down_bps, u + r.up_bps));
        let active = rows.iter().filter(|r| r.active()).count();
        let list = listed(rows);
        let close = |window: &mut Window| window.remove_window();

        let item = |r: &Row| {
            let (id, paused, engine) = (r.id, r.resumable(), self.engine.clone());
            div()
                .h(px(ITEM))
                .pl_4()
                .pr_2()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(
                            div()
                                .flex()
                                .gap_3()
                                .text_size(px(11.5))
                                .child(div().flex_1().min_w_0().truncate().child(r.name.clone()))
                                // Fixed width so the name's truncation point doesn't move as this ticks.
                                .child(
                                    div()
                                        .w(px(80.))
                                        .flex_none()
                                        .flex()
                                        .items_center()
                                        .justify_end()
                                        .gap_1()
                                        .text_color(t.faint)
                                        .when(r.state == State::Seeding, |d| {
                                            d.child(icon("arrow-up", px(10.), t.up))
                                        })
                                        .child(div().min_w_0().truncate().child(detail(r))),
                                ),
                        )
                        .child(progress_bar(r.fraction(), t.state(r.state), &t, px(3.))),
                )
                // A magnet still fetching its info can't be paused yet; keep its slot so the row doesn't reflow.
                .map(|d| match r.state {
                    State::Metadata => d.child(div().size(px(28.)).flex_none()),
                    _ => d.child(
                        icon_button(("pause", id), if paused { "play" } else { "pause" }, &t)
                            .tooltip(tip(if paused { "Resume" } else { "Pause" }))
                            .on_click(move |_, _, _| engine.set_paused(id, !paused)),
                    ),
                })
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(t.r_xl())
            .bg(t.bg)
            .border_1()
            .border_color(t.border)
            .font_family(t.font.clone())
            .text_color(t.text)
            .child(
                div()
                    .h(px(HEADER))
                    .flex_none()
                    .px_4()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(speed_slot(speed_label("arrow-down", down, t.down, &t)).justify_start())
                    .child(speed_slot(speed_label("arrow-up", up, t.up, &t)).justify_start())
                    .child(
                        div()
                            .ml_auto()
                            .text_size(px(11.))
                            .text_color(t.faint)
                            .child(format!("{active} of {} active", rows.len())),
                    ),
            )
            .child(
                div()
                    .h(px(ITEM * self.slots as f32))
                    .flex_none()
                    .overflow_hidden()
                    .border_t_1()
                    .border_color(t.border)
                    .map(|d| match list.is_empty() {
                        true => d
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(11.5))
                            .text_color(t.faint)
                            .child(if rows.is_empty() {
                                "No transfers"
                            } else {
                                "Nothing transferring"
                            }),
                        false => d.children(list.iter().take(self.slots).map(|r| item(r))),
                    }),
            )
            .child(
                div()
                    .h(px(FOOTER))
                    .flex_none()
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .border_t_1()
                    .border_color(t.border)
                    .child(
                        icon_button("open", "window", &t)
                            .tooltip(tip("Open Torrie"))
                            .on_click(move |_, window, cx| {
                                close(window);
                                cx.dispatch_action(&ShowMainWindow);
                            }),
                    )
                    .child(
                        icon_button("settings", "sliders", &t)
                            .tooltip(tip("Settings"))
                            .on_click(move |_, window, cx| {
                                close(window);
                                cx.dispatch_action(&OpenSettings);
                            }),
                    )
                    .child(
                        icon_button("quit", "power", &t)
                            .tooltip(tip("Quit Torrie"))
                            .on_click(|_, _, cx| cx.dispatch_action(&Quit)),
                    ),
            )
    }
}
