//! The Add Torrent window: where to save a new torrent, which files to download, and whether to start it.

use std::{path::Path, sync::Arc};

use gpui::{
    App, ClickEvent, Context, FocusHandle, FontWeight, KeyDownEvent, PathPromptOptions, Window,
    div, prelude::*, px,
};

use crate::{
    engine::{Engine, Review},
    format::fmt_bytes,
    theme::{Theme, button, checkbox, icon},
    window::drag_region,
};

/// One window per torrent. Closing it cancels the add.
pub fn open(engine: Arc<Engine>, review: Review, cx: &mut App) {
    cx.activate(true);
    _ = cx.open_window(
        crate::window_options("Add Torrent", 520., 560., (420., 360.), cx),
        |window, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                focus.focus(window);
                AddWindow {
                    engine,
                    review: Some(review),
                    focus,
                }
            })
        },
    );
}

pub struct AddWindow {
    engine: Arc<Engine>,
    /// Taken when the torrent is added.
    review: Option<Review>,
    focus: FocusHandle,
}

impl AddWindow {
    fn add(&mut self, window: &mut Window) {
        if self.review.as_ref().is_some_and(|r| !r.wanted.is_empty())
            && let Some(r) = self.review.take()
        {
            self.engine.confirm(r);
            window.remove_window();
        }
    }

    fn choose_folder(&mut self, cx: &mut Context<Self>) {
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
                this.update(cx, |this, cx| {
                    if let Some(r) = &mut this.review {
                        r.dir = dir;
                    }
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn key(&mut self, ev: &KeyDownEvent, window: &mut Window, _: &mut Context<Self>) {
        match ev.keystroke.key.as_str() {
            "enter" => self.add(window),
            "escape" => window.remove_window(),
            _ => {}
        }
    }
}

impl Render for AddWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::of(window, cx);
        let Some(r) = &self.review else { return div() };
        let total: u64 = r.files.iter().map(|f| f.2).sum();
        let wanted: u64 = r
            .files
            .iter()
            .filter(|f| r.wanted.contains(&f.0))
            .map(|f| f.2)
            .sum();
        let all = r.wanted.len() == r.files.len();
        let edit = |f: fn(&mut Review)| {
            cx.listener(move |this: &mut Self, _: &ClickEvent, _, cx| {
                if let Some(r) = &mut this.review {
                    f(r);
                }
                cx.notify();
            })
        };
        let files = r.files.iter().map(|(index, path, len)| {
            let (index, on) = (*index, r.wanted.contains(index));
            let p = Path::new(path);
            let name = p
                .file_name()
                .map_or(path.clone(), |n| n.to_string_lossy().into_owned());
            let dir = p
                .parent()
                .map(|p| p.display().to_string())
                .filter(|p| !p.is_empty());
            div()
                .id(("file", index))
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(6.))
                .rounded(t.r_md())
                .hover(|s| s.bg(t.hover))
                .child(checkbox(("check", index), on, &t))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .when(!on, |el| el.opacity(0.5))
                        .child(div().truncate().child(name))
                        .when_some(dir, |el, dir| {
                            el.child(
                                div()
                                    .truncate()
                                    .text_size(px(10.5))
                                    .text_color(t.faint)
                                    .child(dir),
                            )
                        }),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(t.muted)
                        .child(fmt_bytes(*len)),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(r) = &mut this.review
                        && !r.wanted.remove(&index)
                    {
                        r.wanted.insert(index);
                    }
                    cx.notify();
                }))
        });

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
            .child(drag_region(px(44.)))
            .child(
                div()
                    .px_5()
                    .pb_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Add Torrent"),
                    )
                    .child(div().truncate().text_color(t.muted).child(r.name.clone())),
            )
            .child(
                div()
                    .mx_5()
                    .p_2()
                    .pl_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(t.r_lg())
                    .bg(t.surface)
                    .border_1()
                    .border_color(t.border)
                    .shadow(t.lift())
                    .child(icon("folder", px(14.), t.faint))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(r.folder().display().to_string()),
                    )
                    .child(
                        button("folder", "Change…", &t, false)
                            .on_click(cx.listener(|this, _, _, cx| this.choose_folder(cx))),
                    ),
            )
            .child(
                div()
                    .px_5()
                    .pt_4()
                    .pb_1()
                    .flex()
                    .items_center()
                    .child(div().flex_1().text_color(t.muted).child(format!(
                        "{} of {} · {} files",
                        fmt_bytes(wanted),
                        fmt_bytes(total),
                        r.files.len()
                    )))
                    .when(r.files.len() > 1, |d| {
                        d.child(
                            button(
                                "all",
                                if all { "Select None" } else { "Select All" },
                                &t,
                                false,
                            )
                            .on_click(edit(|r| {
                                r.wanted = if r.wanted.len() == r.files.len() {
                                    Default::default()
                                } else {
                                    r.files.iter().map(|f| f.0).collect()
                                }
                            })),
                        )
                    }),
            )
            .child(
                div()
                    .id("files")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_3()
                    .children(files),
            )
            .child(
                div()
                    .px_5()
                    .py_4()
                    .flex()
                    .items_center()
                    .gap_2()
                    .border_t_1()
                    .border_color(t.border)
                    .child(
                        div()
                            .id("paused")
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(checkbox("paused-box", r.paused, &t))
                            .child(div().text_color(t.muted).child("Start paused"))
                            .on_click(edit(|r| r.paused = !r.paused)),
                    )
                    .child(div().flex_1())
                    .child(
                        button("cancel", "Cancel", &t, false)
                            .on_click(|_, window, _| window.remove_window()),
                    )
                    .child(
                        button("add", "Add", &t, true)
                            .when(r.wanted.is_empty(), |d| d.opacity(0.5))
                            .on_click(cx.listener(|this, _, window, _| this.add(window))),
                    ),
            )
    }
}
