//! Palette, bundled assets (Figtree font, Phosphor icons) and small shared widgets.

use std::{
    borrow::Cow,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::Context as _;
use gpui::{
    Animation, AnimationExt, AnyElement, AnyView, App, AssetSource, BoxShadow, Context, Div,
    ElementId, Global, Hsla, IntoElement, Pixels, Render, Rgba, SharedString, Stateful, Svg,
    Transformation, Window, WindowAppearance, div, linear_color_stop, linear_gradient, percentage,
    point, prelude::*, px, relative, rgb, svg,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    engine::{Appearance, Settings},
    format::State,
};

pub const FONT: &str = "Figtree";

/// Regular, Medium, SemiBold, Bold. Also served to the web remote.
pub const FONTS: [&[u8]; 4] = [
    include_bytes!("../assets/fonts/Figtree-Regular.ttf"),
    include_bytes!("../assets/fonts/Figtree-Medium.ttf"),
    include_bytes!("../assets/fonts/Figtree-SemiBold.ttf"),
    include_bytes!("../assets/fonts/Figtree-Bold.ttf"),
];

pub fn load_fonts(cx: &App) {
    cx.text_system()
        .add_fonts(FONTS.iter().map(|f| Cow::Borrowed(*f)).collect())
        .expect("bundled Figtree fonts");
}

/// Phosphor icons (MIT), regular weight, 256×256 fills.
const ICONS: &[(&str, &str)] = &[
    (
        "play",
        r#"<path d="M232.4,114.49,88.32,26.35a16,16,0,0,0-16.2-.3A15.86,15.86,0,0,0,64,39.87V216.13A15.94,15.94,0,0,0,80,232a16.07,16.07,0,0,0,8.36-2.35L232.4,141.51a15.81,15.81,0,0,0,0-27ZM80,215.94V40l143.83,88Z"/>"#,
    ),
    (
        "pause",
        r#"<path d="M200,32H160a16,16,0,0,0-16,16V208a16,16,0,0,0,16,16h40a16,16,0,0,0,16-16V48A16,16,0,0,0,200,32Zm0,176H160V48h40ZM96,32H56A16,16,0,0,0,40,48V208a16,16,0,0,0,16,16H96a16,16,0,0,0,16-16V48A16,16,0,0,0,96,32Zm0,176H56V48H96Z"/>"#,
    ),
    (
        "x",
        r#"<path d="M205.66,194.34a8,8,0,0,1-11.32,11.32L128,139.31,61.66,205.66a8,8,0,0,1-11.32-11.32L116.69,128,50.34,61.66A8,8,0,0,1,61.66,50.34L128,116.69l66.34-66.35a8,8,0,0,1,11.32,11.32L139.31,128Z"/>"#,
    ),
    (
        "trash",
        r#"<path d="M216,48H176V40a24,24,0,0,0-24-24H104A24,24,0,0,0,80,40v8H40a8,8,0,0,0,0,16h8V208a16,16,0,0,0,16,16H192a16,16,0,0,0,16-16V64h8a8,8,0,0,0,0-16ZM96,40a8,8,0,0,1,8-8h48a8,8,0,0,1,8,8v8H96Zm96,168H64V64H192ZM112,104v64a8,8,0,0,1-16,0V104a8,8,0,0,1,16,0Zm48,0v64a8,8,0,0,1-16,0V104a8,8,0,0,1,16,0Z"/>"#,
    ),
    (
        "plus",
        r#"<path d="M224,128a8,8,0,0,1-8,8H136v80a8,8,0,0,1-16,0V136H40a8,8,0,0,1,0-16h80V40a8,8,0,0,1,16,0v80h80A8,8,0,0,1,224,128Z"/>"#,
    ),
    (
        "check",
        r#"<path d="M229.66,77.66l-128,128a8,8,0,0,1-11.32,0l-56-56a8,8,0,0,1,11.32-11.32L96,188.69,218.34,66.34a8,8,0,0,1,11.32,11.32Z"/>"#,
    ),
    (
        "magnet",
        r#"<path d="M207,50.25A87.46,87.46,0,0,0,144.6,24h-.33A87.48,87.48,0,0,0,82,49.81L20.61,112a16,16,0,0,0,.06,22.56l28.66,28.66a15.92,15.92,0,0,0,11.32,4.69h.09a16,16,0,0,0,11.36-4.82L133,100.69a16.08,16.08,0,0,1,22.41-.21,15.6,15.6,0,0,1,4.73,11.19,16.89,16.89,0,0,1-4.85,12L93,183.88a16,16,0,0,0-.17,22.79l28.66,28.66a16.06,16.06,0,0,0,22.52.12L205.81,175C240.26,140.5,240.79,84.56,207,50.25ZM60.65,151.89,32,123.24,55.8,99.12l28.52,28.52ZM132.79,224l-28.68-28.65,24.38-23.57L157,200.32Zm61.76-60.44-26.11,25.54L140,160.68l26.44-25.57.1-.09a33,33,0,0,0,9.57-23.5A31.44,31.44,0,0,0,166.47,89a32.2,32.2,0,0,0-44.9.5L95.49,116.18,67,87.74,93.35,61.09A71.51,71.51,0,0,1,144.27,40h.27a71.55,71.55,0,0,1,51.05,21.48C223.25,89.55,222.75,135.38,194.55,163.58Z"/>"#,
    ),
    (
        "sliders",
        r#"<path d="M40,88H73a32,32,0,0,0,62,0h81a8,8,0,0,0,0-16H135a32,32,0,0,0-62,0H40a8,8,0,0,0,0,16Zm64-24A16,16,0,1,1,88,80,16,16,0,0,1,104,64ZM216,168H199a32,32,0,0,0-62,0H40a8,8,0,0,0,0,16h97a32,32,0,0,0,62,0h17a8,8,0,0,0,0-16Zm-48,24a16,16,0,1,1,16-16A16,16,0,0,1,168,192Z"/>"#,
    ),
    (
        "arrow-down",
        r#"<path d="M205.66,149.66l-72,72a8,8,0,0,1-11.32,0l-72-72a8,8,0,0,1,11.32-11.32L120,196.69V40a8,8,0,0,1,16,0V196.69l58.34-58.35a8,8,0,0,1,11.32,11.32Z"/>"#,
    ),
    (
        "arrow-up",
        r#"<path d="M205.66,117.66a8,8,0,0,1-11.32,0L136,59.31V216a8,8,0,0,1-16,0V59.31L61.66,117.66a8,8,0,0,1-11.32-11.32l72-72a8,8,0,0,1,11.32,0l72,72A8,8,0,0,1,205.66,117.66Z"/>"#,
    ),
    (
        "folder",
        r#"<path d="M216,72H131.31L104,44.69A15.86,15.86,0,0,0,92.69,40H40A16,16,0,0,0,24,56V200.62A15.4,15.4,0,0,0,39.38,216H216.89A15.13,15.13,0,0,0,232,200.89V88A16,16,0,0,0,216,72ZM40,56H92.69l16,16H40ZM216,200H40V88H216Z"/>"#,
    ),
    (
        "panel",
        r#"<g transform="matrix(-1 0 0 1 256 0)"><path d="M216,40H40A16,16,0,0,0,24,56V200a16,16,0,0,0,16,16H216a16,16,0,0,0,16-16V56A16,16,0,0,0,216,40ZM40,56H80V200H40ZM216,200H96V56H216V200Z"/></g>"#,
    ),
    (
        "layers",
        r#"<path d="M230.91,172A8,8,0,0,1,228,182.91l-96,56a8,8,0,0,1-8.06,0l-96-56A8,8,0,0,1,36,169.09l92,53.65,92-53.65A8,8,0,0,1,230.91,172ZM220,121.09l-92,53.65L36,121.09A8,8,0,0,0,28,134.91l96,56a8,8,0,0,0,8.06,0l96-56A8,8,0,1,0,220,121.09ZM24,80a8,8,0,0,1,4-6.91l96-56a8,8,0,0,1,8.06,0l96,56a8,8,0,0,1,0,13.82l-96,56a8,8,0,0,1-8.06,0l-96-56A8,8,0,0,1,24,80Zm23.88,0L128,126.74,208.12,80,128,33.26Z"/>"#,
    ),
    (
        "activity",
        r#"<path d="M240,128a8,8,0,0,1-8,8H204.94l-37.78,75.58A8,8,0,0,1,160,216h-.4a8,8,0,0,1-7.08-5.14L95.35,60.76,63.28,131.31A8,8,0,0,1,56,136H24a8,8,0,0,1,0-16H50.85L88.72,36.69a8,8,0,0,1,14.76.46l57.51,151,31.85-63.71A8,8,0,0,1,200,120h32A8,8,0,0,1,240,128Z"/>"#,
    ),
    (
        "alert",
        r#"<path d="M236.8,188.09,149.35,36.22h0a24.76,24.76,0,0,0-42.7,0L19.2,188.09a23.51,23.51,0,0,0,0,23.72A24.35,24.35,0,0,0,40.55,224h174.9a24.35,24.35,0,0,0,21.33-12.19A23.51,23.51,0,0,0,236.8,188.09ZM222.93,203.8a8.5,8.5,0,0,1-7.48,4.2H40.55a8.5,8.5,0,0,1-7.48-4.2,7.59,7.59,0,0,1,0-7.72L120.52,44.21a8.75,8.75,0,0,1,15,0l87.45,151.87A7.59,7.59,0,0,1,222.93,203.8ZM120,144V104a8,8,0,0,1,16,0v40a8,8,0,0,1-16,0Zm20,36a12,12,0,1,1-12-12A12,12,0,0,1,140,180Z"/>"#,
    ),
    (
        "refresh",
        r#"<path d="M224,48V96a8,8,0,0,1-8,8H168a8,8,0,0,1,0-16h28.69L182.06,73.37a79.56,79.56,0,0,0-56.13-23.43h-.45A79.52,79.52,0,0,0,69.59,72.71,8,8,0,0,1,58.41,61.27a96,96,0,0,1,135,.79L208,76.69V48a8,8,0,0,1,16,0ZM186.41,183.29a80,80,0,0,1-112.47-.66L59.31,168H88a8,8,0,0,0,0-16H40a8,8,0,0,0-8,8v48a8,8,0,0,0,16,0V179.31l14.63,14.63A95.43,95.43,0,0,0,130,222.06h.53a95.36,95.36,0,0,0,67.07-27.33,8,8,0,0,0-11.18-11.44Z"/>"#,
    ),
    (
        "globe",
        r#"<path d="M128,24h0A104,104,0,1,0,232,128,104.12,104.12,0,0,0,128,24Zm88,104a87.61,87.61,0,0,1-3.33,24H174.16a157.44,157.44,0,0,0,0-48h38.51A87.61,87.61,0,0,1,216,128ZM102,168H154a115.11,115.11,0,0,1-26,45A115.27,115.27,0,0,1,102,168Zm-3.9-16a140.84,140.84,0,0,1,0-48h59.88a140.84,140.84,0,0,1,0,48ZM40,128a87.61,87.61,0,0,1,3.33-24H81.84a157.44,157.44,0,0,0,0,48H43.33A87.61,87.61,0,0,1,40,128ZM154,88H102a115.11,115.11,0,0,1,26-45A115.27,115.27,0,0,1,154,88Zm52.33,0H170.71a135.28,135.28,0,0,0-22.3-45.6A88.29,88.29,0,0,1,206.37,88ZM107.59,42.4A135.28,135.28,0,0,0,85.29,88H49.63A88.29,88.29,0,0,1,107.59,42.4ZM49.63,168H85.29a135.28,135.28,0,0,0,22.3,45.6A88.29,88.29,0,0,1,49.63,168Zm98.78,45.6a135.28,135.28,0,0,0,22.3-45.6h35.66A88.29,88.29,0,0,1,148.41,213.6Z"/>"#,
    ),
    (
        "users",
        r#"<path d="M117.25,157.92a60,60,0,1,0-66.5,0A95.83,95.83,0,0,0,3.53,195.63a8,8,0,1,0,13.4,8.74,80,80,0,0,1,134.14,0,8,8,0,0,0,13.4-8.74A95.83,95.83,0,0,0,117.25,157.92ZM40,108a44,44,0,1,1,44,44A44.05,44.05,0,0,1,40,108Zm210.14,98.7a8,8,0,0,1-11.07-2.33A79.83,79.83,0,0,0,172,168a8,8,0,0,1,0-16,44,44,0,1,0-16.34-84.87,8,8,0,1,1-5.94-14.85,60,60,0,0,1,55.53,105.64,95.83,95.83,0,0,1,47.22,37.71A8,8,0,0,1,250.14,206.7Z"/>"#,
    ),
    (
        "file",
        r#"<path d="M213.66,82.34l-56-56A8,8,0,0,0,152,24H56A16,16,0,0,0,40,40V216a16,16,0,0,0,16,16H200a16,16,0,0,0,16-16V88A8,8,0,0,0,213.66,82.34ZM160,51.31,188.69,80H160ZM200,216H56V40h88V88a8,8,0,0,0,8,8h48V216Z"/>"#,
    ),
    (
        "info",
        r#"<path d="M128,24A104,104,0,1,0,232,128,104.11,104.11,0,0,0,128,24Zm0,192a88,88,0,1,1,88-88A88.1,88.1,0,0,1,128,216Zm16-40a8,8,0,0,1-8,8,16,16,0,0,1-16-16V128a8,8,0,0,1,0-16,16,16,0,0,1,16,16v40A8,8,0,0,1,144,176ZM112,84a12,12,0,1,1,12,12A12,12,0,0,1,112,84Z"/>"#,
    ),
    (
        "window",
        r#"<path d="M216,40H40A16,16,0,0,0,24,56V200a16,16,0,0,0,16,16H216a16,16,0,0,0,16-16V56A16,16,0,0,0,216,40Zm0,160H40V56H216V200ZM80,84A12,12,0,1,1,68,72,12,12,0,0,1,80,84Zm40,0a12,12,0,1,1-12-12A12,12,0,0,1,120,84Z"/>"#,
    ),
    (
        "power",
        r#"<path d="M120,128V48a8,8,0,0,1,16,0v80a8,8,0,0,1-16,0Zm60.37-78.7a8,8,0,0,0-8.74,13.4C194.74,77.77,208,101.57,208,128a80,80,0,0,1-160,0c0-26.43,13.26-50.23,36.37-65.3a8,8,0,0,0-8.74-13.4C47.9,67.38,32,96.06,32,128a96,96,0,0,0,192,0C224,96.06,208.1,67.38,180.37,49.3Z"/>"#,
    ),
    (
        "gauge",
        r#"<path d="M207.06,72.67A111.24,111.24,0,0,0,128,40h-.4C66.07,40.21,16,91,16,153.13V176a16,16,0,0,0,16,16H224a16,16,0,0,0,16-16V152A111.25,111.25,0,0,0,207.06,72.67ZM224,176H119.71l54.76-75.3a8,8,0,0,0-12.94-9.42L99.92,176H32V153.13c0-3.08.15-6.12.43-9.13H56a8,8,0,0,0,0-16H35.27c10.32-38.86,44-68.24,84.73-71.66V80a8,8,0,0,0,16,0V56.33A96.14,96.14,0,0,1,221,128H200a8,8,0,0,0,0,16h23.67c.21,2.65.33,5.31.33,8Z"/>"#,
    ),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        let name = path
            .strip_prefix("icons/")
            .and_then(|p| p.strip_suffix(".svg"));
        Ok(ICONS.iter().find(|(n, _)| Some(*n) == name).map(|(_, body)| {
            Cow::Owned(
                format!(
                    r##"<svg xmlns="http://www.w3.org/2000/svg" width="256" height="256" viewBox="0 0 256 256" fill="#000">{body}</svg>"##
                )
                .into_bytes(),
            )
        }))
    }

    fn list(&self, _: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(n, _)| format!("icons/{n}.svg").into())
            .collect())
    }
}

/// Every color in the UI. Theme files set any subset of these keys (as `#rrggbb` or `#rrggbbaa`) on top of
/// `"extends"` (default "Dark").
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Theme {
    pub dark: bool,
    pub font: SharedString,
    /// Medium corner radius in px; the other tiers scale from it (see [`Theme::r_sm`] and friends).
    pub radius: f32,
    pub bg: Hsla,
    pub sidebar: Hsla,
    pub surface: Hsla,
    pub hover: Hsla,
    pub elevated: Hsla,
    pub border: Hsla,
    pub border_strong: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub faint: Hsla,
    pub accent: Hsla,
    pub down: Hsla,
    pub up: Hsla,
    pub checking: Hsla,
    pub paused: Hsla,
    pub error: Hsla,
    /// From the "Animations" setting, not the theme.
    #[serde(skip)]
    pub motion: bool,
}

/// bg, sidebar, surface, hover, elevated, border, border_strong, text, muted, faint, accent, down, up,
/// checking, paused, error. Light and Dark are the defaults.
const BUILTIN: [(&str, bool, [u32; 16]); 16] = [
    (
        "Light",
        false,
        [
            0xfcfcfd, 0xf1f2f5, 0xffffff, 0xf3f4f7, 0xeceef2, 0xe6e8ed, 0xd4d8df, 0x14161b,
            0x525865, 0x646c7b, 0x5c4dff, 0x185bff, 0x0b7a51, 0x8e6400, 0x666c77, 0xd01e23,
        ],
    ),
    (
        "Dark",
        true,
        [
            0x0f1115, 0x17191f, 0x14161b, 0x1a1d24, 0x20232b, 0x22262e, 0x30353f, 0xe8eaef,
            0xa7adb7, 0x89919e, 0x8f82ff, 0x5b9dff, 0x3ddc97, 0xf5b544, 0x8b919e, 0xff6b6b,
        ],
    ),
    (
        "Catppuccin Latte",
        false,
        [
            0xeff1f5, 0xe6e9ef, 0xf7f8fa, 0xdadde4, 0xccd0da, 0xccd0da, 0xbcc0cc, 0x2a2b3a,
            0x444654, 0x555869, 0x7113ec, 0x094cd2, 0x29661b, 0x7c4f10, 0x555868, 0xb30d31,
        ],
    ),
    (
        "Catppuccin Mocha",
        true,
        [
            0x1e1e2e, 0x181825, 0x212131, 0x262736, 0x313244, 0x313244, 0x45475a, 0xf5f7fd,
            0xb9bed4, 0x9b9eae, 0xcba6f7, 0x89b4fa, 0xa6e3a1, 0xf9e2af, 0x9a9eb1, 0xf38ba8,
        ],
    ),
    (
        "Dracula",
        true,
        [
            0x282a36, 0x21222c, 0x2c2d38, 0x32333c, 0x44475a, 0x44475a, 0x565b73, 0xffffff,
            0xcdd2e2, 0xb0b6cd, 0xc9a6fa, 0x8be9fd, 0x50fa7b, 0xf1fa8c, 0xaeb6d0, 0xff9a9a,
        ],
    ),
    (
        "Everforest Dark",
        true,
        [
            0x2d353b, 0x232a2e, 0x343c41, 0x414a4c, 0x343f44, 0x343f44, 0x475258, 0xffffff,
            0xe9ebe9, 0xbdc4c2, 0xb5ca94, 0x9fccc6, 0x9eceaa, 0xdcbe83, 0xbec5c0, 0xf0b3b5,
        ],
    ),
    (
        "Gruvbox Dark",
        true,
        [
            0x282828, 0x1d2021, 0x31302e, 0x413e39, 0x3c3836, 0x3c3836, 0x504945, 0xffffff,
            0xdbd4cc, 0xb8b2a8, 0xfabd2f, 0x9cb7ad, 0xb8bb26, 0xe1a739, 0xbab1a7, 0xfd9387,
        ],
    ),
    (
        "Gruvbox Light",
        false,
        [
            0xfbf1c7, 0xebdbb2, 0xfdf8e3, 0xeae6ca, 0xd5c4a1, 0xd5c4a1, 0xbdae93, 0x252321,
            0x463f39, 0x5b523f, 0x933103, 0x065a6a, 0x57530a, 0x70490c, 0x5a5046, 0x9d0006,
        ],
    ),
    (
        "Kanagawa",
        true,
        [
            0x1f1f28, 0x16161d, 0x212129, 0x26252a, 0x2a2a37, 0x2a2a37, 0x363646, 0xf7f5ee,
            0xbebdb8, 0x9f9e9d, 0x809ed9, 0x7fb4ca, 0x98bb6c, 0xe6c384, 0x9e9d96, 0xe87e8a,
        ],
    ),
    (
        "Nord",
        true,
        [
            0x2e3440, 0x272c36, 0x373d4a, 0x484e5e, 0x3b4252, 0x3b4252, 0x4c566a, 0xffffff,
            0xedeef1, 0xc3c6cc, 0xa2ceda, 0xb6c9db, 0xb8cda6, 0xebcb8b, 0xc1c7d3, 0xe4bcc0,
        ],
    ),
    (
        "One Dark",
        true,
        [
            0x282c34, 0x21252b, 0x292d35, 0x2c3037, 0x2c313a, 0x2c313a, 0x3e4451, 0xffffff,
            0xcdd0d6, 0xa8aeb9, 0x68b3f0, 0x5db9c5, 0x98c379, 0xe5c07b, 0xa8aeb8, 0xe9969d,
        ],
    ),
    (
        "Rosé Pine",
        true,
        [
            0x191724, 0x1f1d2e, 0x201e2d, 0x2e2c3e, 0x26233a, 0x26233a, 0x403d52, 0xf2f1fa,
            0xbab8cb, 0x9c99ad, 0xc4a7e7, 0x9ccfd8, 0xebbcba, 0xf6c177, 0x9c99ae, 0xec7395,
        ],
    ),
    (
        "Rosé Pine Dawn",
        false,
        [
            0xfaf4ed, 0xf2e9e1, 0xfcfaf6, 0xf2ede9, 0xdfdad9, 0xdfdad9, 0xcecacd, 0x343148,
            0x4e4c61, 0x645d71, 0x6c5685, 0x3b666d, 0xa93a34, 0x89550e, 0x625d70, 0x95485d,
        ],
    ),
    (
        "Solarized Dark",
        true,
        [
            0x002b36, 0x00212b, 0x06323d, 0x123f4a, 0x073642, 0x073642, 0x586e75, 0xffffff,
            0xc7d0d4, 0x92b3bc, 0x6fb4e5, 0x32c1b7, 0x9db500, 0xd4a000, 0x9dafb5, 0xec8f8d,
        ],
    ),
    (
        "Solarized Light",
        false,
        [
            0xfdf6e3, 0xeee8d5, 0xfefaf1, 0xf0ebda, 0xe4ddc8, 0xe4ddc8, 0x93a1a1, 0x2b3639,
            0x455052, 0x5b645c, 0x1c669a, 0x1c6c66, 0x596600, 0x7a5d00, 0x566363, 0xbd2320,
        ],
    ),
    (
        "Tokyo Night",
        true,
        [
            0x1a1b26, 0x16161e, 0x1e1e29, 0x24242f, 0x292e42, 0x292e42, 0x414868, 0xeceffc,
            0xb3b8cf, 0x9399ba, 0x7aa2f7, 0x7dcfff, 0x9ece6a, 0xe0af68, 0x9299bb, 0xf7768e,
        ],
    ),
];

/// WCAG contrast ratio, 1 to 21.
fn contrast(a: Hsla, b: Hsla) -> f32 {
    let lum = |c: Hsla| {
        let c = c.to_rgb();
        let f = |v: f32| {
            if v <= 0.03928 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
    };
    let (a, b) = (lum(a), lum(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn palette(dark: bool, c: [u32; 16]) -> Theme {
    let c = c.map(|v| Hsla::from(rgb(v)));
    Theme {
        dark,
        font: FONT.into(),
        radius: 6.0,
        bg: c[0],
        sidebar: c[1],
        surface: c[2],
        hover: c[3],
        elevated: c[4],
        border: c[5],
        border_strong: c[6],
        text: c[7],
        muted: c[8],
        faint: c[9],
        accent: c[10],
        down: c[11],
        up: c[12],
        checking: c[13],
        paused: c[14],
        error: c[15],
        motion: true,
    }
}

/// Accent presets offered in Settings.
pub const ACCENTS: [&str; 9] = [
    "#6a5cff", "#2f6bff", "#0ea5e9", "#14b8a6", "#22c55e", "#f59e0b", "#f97316", "#ef4444",
    "#ec4899",
];

/// Built-in and user themes plus the current choice; rebuilt by [`apply`].
pub struct Themes {
    pub list: Vec<(String, Theme)>,
    pub errors: Vec<String>,
    appearance: Appearance,
    light_theme: String,
    dark_theme: String,
    accent: Option<Hsla>,
    font: Option<SharedString>,
    motion: bool,
}

impl Global for Themes {}

pub fn themes_dir() -> PathBuf {
    crate::engine::config_dir().join("themes")
}

/// (Re)reads `themes/*.json` and applies the theme settings to every window.
pub fn apply(s: &Settings, cx: &mut App) {
    let mut list: Vec<(String, Theme)> = BUILTIN
        .iter()
        .map(|(n, d, c)| (n.to_string(), palette(*d, *c)))
        .collect();
    let mut errors = vec![];
    for path in theme_files() {
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        match load_theme(&path, &list) {
            Ok(t) => {
                list.retain(|(n, _)| *n != name);
                list.push((name, t));
            }
            Err(e) => errors.push(format!("{name}.json: {e:#}")),
        }
    }
    let accent = s
        .accent
        .as_deref()
        .and_then(|h| Rgba::try_from(h).ok())
        .map(Hsla::from);
    let motion = s.animations && !system_reduces_motion();
    cx.set_global(Themes {
        list,
        errors,
        appearance: s.appearance,
        light_theme: s.light_theme.clone(),
        dark_theme: s.dark_theme.clone(),
        accent,
        font: s.font.clone().map(Into::into),
        motion,
    });
    cx.refresh_windows();
}

/// Sorted `themes/*.json`. The first time, seeds the folder with an editable copy of Dark listing every key.
fn theme_files() -> Vec<PathBuf> {
    let dir = themes_dir();
    if !dir.exists() {
        let mut t = palette(BUILTIN[1].1, BUILTIN[1].2);
        t.accent = rgb(0xff7a59).into();
        let json = serde_json::to_vec_pretty(&t).unwrap_or_default();
        _ = std::fs::create_dir_all(&dir)
            .and_then(|_| std::fs::write(dir.join("Custom.json"), json));
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|it| {
            it.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "json"))
                .collect()
        })
        .unwrap_or_default();
    paths.sort();
    paths
}

fn load_theme(path: &Path, list: &[(String, Theme)]) -> anyhow::Result<Theme> {
    let mut keys: serde_json::Map<String, Value> = serde_json::from_slice(&std::fs::read(path)?)?;
    let base = match keys.remove("extends") {
        Some(Value::String(b)) => b,
        None => "Dark".into(),
        Some(_) => anyhow::bail!("\"extends\" must be a theme name"),
    };
    let (_, base) = list
        .iter()
        .find(|(n, _)| *n == base)
        .with_context(|| format!("no theme named \"{base}\""))?;
    let Value::Object(mut full) = serde_json::to_value(base)? else {
        unreachable!()
    };
    full.extend(keys);
    Ok(serde_json::from_value(Value::Object(full))?)
}

impl Theme {
    pub fn of(window: &Window, cx: &App) -> Self {
        let os_dark = matches!(
            window.appearance(),
            WindowAppearance::Dark | WindowAppearance::VibrantDark
        );
        let g = cx.global::<Themes>();
        let find = |name: &str| {
            g.list
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, t)| t.clone())
        };
        let dark = match g.appearance {
            Appearance::System => os_dark,
            Appearance::Light => false,
            Appearance::Dark => true,
        };
        // A theme file that's gone, or one for the other mode, falls back to the default.
        let chosen = if dark { &g.dark_theme } else { &g.light_theme };
        let mut t = find(chosen)
            .filter(|t| t.dark == dark)
            .or_else(|| find(if dark { "Dark" } else { "Light" }))
            .unwrap_or_else(|| palette(BUILTIN[1].1, BUILTIN[1].2));
        if let Some(a) = g.accent {
            t.set_accent(a);
        }
        if let Some(f) = &g.font {
            t.font = f.clone();
        }
        t.motion = g.motion;
        t
    }

    /// One preset serves both modes, so keep its hue but shift lightness until it reads on this theme.
    fn set_accent(&mut self, mut a: Hsla) {
        let bgs = [
            self.bg,
            self.sidebar,
            self.surface,
            self.hover,
            self.elevated,
        ];
        while bgs.iter().any(|&bg| contrast(a, bg) < 4.5) && a.l > 0.0 && a.l < 1.0 {
            a.l = (a.l + if self.dark { 0.01 } else { -0.01 }).clamp(0.0, 1.0);
        }
        self.accent = a;
    }

    pub fn state(&self, s: State) -> Hsla {
        match s {
            State::Downloading => self.down,
            State::Seeding => self.up,
            State::Metadata | State::Checking => self.checking,
            State::Paused => self.paused,
            State::Error => self.error,
        }
    }

    /// White or near-black, whichever reads better on the accent (pastel accents need dark text).
    pub fn on_accent(&self) -> Hsla {
        if contrast(gpui::white(), self.accent) >= contrast(rgb(0x14161b).into(), self.accent) {
            gpui::white()
        } else {
            rgb(0x14161b).into()
        }
    }

    pub fn tint(&self, c: Hsla) -> Hsla {
        c.opacity(if self.dark { 0.16 } else { 0.11 })
    }

    /// A 1px drop shadow that makes buttons and cards sit slightly raised; stronger on dark themes.
    pub fn lift(&self) -> Vec<BoxShadow> {
        let color = gpui::black().opacity(if self.dark { 0.45 } else { 0.08 });
        vec![BoxShadow {
            color,
            offset: point(px(0.), px(1.)),
            blur_radius: px(2.),
            spread_radius: px(0.),
        }]
    }

    /// Controls inside a padded container (concentric with `r_lg` around a ~2px inset).
    pub fn r_sm(&self) -> Pixels {
        px(self.radius * 2.0 / 3.0)
    }

    /// Buttons, fields, list items.
    pub fn r_md(&self) -> Pixels {
        px(self.radius)
    }

    /// Rows, segmented controls, banners.
    pub fn r_lg(&self) -> Pixels {
        px(self.radius * 4.0 / 3.0)
    }

    /// Grouped sections and large surfaces.
    pub fn r_xl(&self) -> Pixels {
        px(self.radius * 2.0)
    }
}

// --- motion ---

/// Ease-out cubic.
pub fn ease(p: f32) -> f32 {
    1.0 - (1.0 - p).powi(3)
}

/// A value easing toward a target, owned by the view that renders it. Retargeting mid-flight starts from
/// the value currently on screen, so interrupted motion never jumps.
#[derive(Clone, Copy)]
pub struct Tween {
    from: f32,
    to: f32,
    start: Instant,
    ms: f32,
}

impl Tween {
    pub fn new(v: f32) -> Self {
        Self {
            from: v,
            to: v,
            start: Instant::now(),
            ms: 1.0,
        }
    }

    /// Changes smaller than 0.002 (under a pixel on a progress bar) snap, so slow downloads don't keep
    /// the window redrawing.
    pub fn set(&mut self, to: f32, ms: u64, motion: bool) {
        if to == self.to {
            return;
        }
        let now = self.value();
        self.from = if motion && (to - now).abs() >= 0.002 {
            now
        } else {
            to
        };
        self.to = to;
        self.start = Instant::now();
        self.ms = ms as f32;
    }

    fn progress(&self) -> f32 {
        (self.start.elapsed().as_secs_f32() * 1000.0 / self.ms).min(1.0)
    }

    pub fn value(&self) -> f32 {
        self.from + (self.to - self.from) * ease(self.progress())
    }

    /// Callers request another frame while this is true.
    pub fn moving(&self) -> bool {
        self.from != self.to && self.progress() < 1.0
    }
}

/// Whether motion is on (the Animations setting, and macOS Reduce motion is off).
pub fn motion(cx: &App) -> bool {
    cx.global::<Themes>().motion
}

/// macOS Settings › Accessibility › Display › Reduce motion.
fn system_reduces_motion() -> bool {
    objc2_app_kit::NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

/// Endless rotation, for "working" icons.
pub fn spin(el: Svg, id: impl Into<ElementId>, t: &Theme) -> AnyElement {
    if !t.motion {
        return el.into_any_element();
    }
    el.with_animation(
        id,
        Animation::new(Duration::from_millis(1100)).repeat(),
        |el, p| el.with_transformation(Transformation::rotate(percentage(p))),
    )
    .into_any_element()
}

pub fn state_icon(s: State) -> &'static str {
    match s {
        State::Downloading => "arrow-down",
        State::Seeding => "arrow-up",
        State::Metadata => "magnet",
        State::Checking => "refresh",
        State::Paused => "pause",
        State::Error => "alert",
    }
}

pub fn icon(name: &str, size: Pixels, color: Hsla) -> Svg {
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .size(size)
        .flex_none()
        .text_color(color)
}

pub fn icon_button(id: impl Into<ElementId>, name: &str, t: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(28.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(t.r_md())
        // Translucent so hover stays visible on any row fill (selected rows use `elevated`).
        .hover(|s| s.bg(t.text.opacity(0.08)))
        .active(|s| s.bg(t.text.opacity(0.14)))
        .child(icon(name, px(15.), t.muted))
}

pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    t: &Theme,
    primary: bool,
) -> Stateful<Div> {
    let shade = |c: Hsla, dl: f32| Hsla {
        l: (c.l + dl).clamp(0.0, 1.0),
        ..c
    };
    let el = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .h(px(28.))
        .px_3()
        // Text sits 1px high, like a key cap.
        .pb(px(1.))
        .rounded(t.r_md())
        .border_1()
        .shadow(t.lift())
        .font_weight(gpui::FontWeight::MEDIUM)
        .child(label.into());
    if primary {
        let (accent, top) = (t.accent, shade(t.accent, 0.06));
        // Lit from above: lighter top fading to the accent, with a darker rim. Hover flattens it.
        el.bg(linear_gradient(
            180.,
            linear_color_stop(top, 0.),
            linear_color_stop(accent, 1.),
        ))
        .border_color(shade(accent, -0.12))
        .text_color(t.on_accent())
        .hover(move |s| s.bg(top))
    } else {
        let hover = t.border_strong;
        el.bg(t.elevated)
            .border_color(t.border_strong)
            .text_color(t.text)
            .hover(move |s| s.bg(hover))
    }
}

pub fn toggle(id: impl Into<ElementId>, on: bool, t: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(34.))
        .h(px(20.))
        .flex_none()
        .p(px(2.))
        .rounded_full()
        .bg(if on { t.accent } else { t.border_strong })
        .flex()
        .when(on, |d| d.justify_end())
        .child(
            div()
                .size(px(16.))
                .rounded_full()
                .bg(gpui::white())
                .shadow(t.lift()),
        )
}

pub fn checkbox(id: impl Into<ElementId>, on: bool, t: &Theme) -> Stateful<Div> {
    div()
        .id(id)
        .size(px(16.))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded(t.r_sm())
        .border_1()
        .border_color(if on { t.accent } else { t.border_strong })
        .when(on, |d| {
            d.bg(t.accent)
                .shadow(t.lift())
                .child(icon("check", px(11.), t.on_accent()))
        })
}

/// 4x4 ordered-dither thresholds (0..16).
const BAYER: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// A dithered bar in the style of the sidebar speed graph: 2px columns with 1px gaps, and the leading edge
/// fading out through a 4x4 Bayer pattern of 1px dots.
pub fn progress_bar(fraction: f32, color: Hsla, t: &Theme, height: Pixels) -> Div {
    const TAIL: usize = 6;
    let track = t.elevated;
    let fraction = fraction.clamp(0.0, 1.0);
    div().h(height).w_full().child(
        gpui::canvas(
            |_, _, _| {},
            move |b, _, window, _| {
                let cols = ((f32::from(b.size.width) + 1.0) / 3.0).floor() as usize;
                let filled = (cols as f32 * fraction).round() as usize;
                let rows = f32::from(b.size.height).round() as usize;
                for i in 0..cols {
                    let x = b.origin.x + px(i as f32 * 3.0);
                    let col = gpui::Bounds::new(
                        gpui::point(x, b.origin.y),
                        gpui::size(px(2.), b.size.height),
                    );
                    window.paint_quad(gpui::fill(col, if i < filled { color } else { track }));
                    // Dither tail just past the fill edge (skipped when empty, so 0% stays clean).
                    let k = i.wrapping_sub(filled);
                    if filled == 0 || k >= TAIL {
                        continue;
                    }
                    let density = 16 - (k + 1) * 16 / (TAIL + 1);
                    for y in 0..rows {
                        for dx in 0..2 {
                            if (BAYER[y % 4][(i * 2 + dx) % 4] as usize) < density {
                                let dot = gpui::Bounds::new(
                                    gpui::point(x + px(dx as f32), b.origin.y + px(y as f32)),
                                    gpui::size(px(1.), px(1.)),
                                );
                                window.paint_quad(gpui::fill(dot, color));
                            }
                        }
                    }
                }
            },
        )
        .size_full(),
    )
}

/// The sidebar speed graph in `progress_bar`'s grain: a 2px column per second, newest on the right, over track
/// columns. ↓ stacks under ↑ and each column's top fades out through the Bayer pattern. `max` is the full height.
/// `hatch` shades the whole field like `ceiling`'s headroom, under the columns, for when there is no cap to draw.
pub fn speed_graph(history: Vec<(f64, f64)>, max: f64, hatch: Option<Hsla>, t: &Theme) -> Div {
    const TAIL: usize = 3;
    let (track, down, up) = (t.elevated, t.down, t.up.opacity(0.75));
    div().size_full().child(
        gpui::canvas(
            |_, _, _| {},
            move |b, _, window, _| {
                let cols = ((f32::from(b.size.width) + 1.0) / 3.0).floor() as usize;
                let rows = f32::from(b.size.height).round() as usize;
                let height = |v: f64| ((v / max * rows as f64).round() as usize).min(rows);
                let (skip, pad) = (
                    history.len().saturating_sub(cols),
                    cols.saturating_sub(history.len()),
                );
                let mut rect = |x: usize, y: usize, w: usize, h: usize, c: Hsla| {
                    let o = gpui::point(b.origin.x + px(x as f32), b.origin.y + px(y as f32));
                    window.paint_quad(gpui::fill(
                        gpui::Bounds::new(o, gpui::size(px(w as f32), px(h as f32))),
                        c,
                    ));
                };
                for i in 0..cols {
                    rect(i * 3, 0, 2, rows, track);
                }
                if let Some(c) = hatch {
                    bayer_hatch(cols * 3, rows, |x, y| rect(x, y, 1, 1, c));
                }
                for i in 0..cols {
                    let x = i * 3;
                    let Some(&(d, u)) = i.checked_sub(pad).and_then(|j| history.get(skip + j))
                    else {
                        continue;
                    };
                    // Heights from the running sum, so rounding never opens a gap between ↓ and ↑.
                    let (dh, top) = (height(d), rows - height(d + u));
                    rect(x, rows - dh, 2, dh, down);
                    rect(x, top, 2, rows - dh - top, up);
                    if top == rows {
                        continue;
                    }
                    let c = if rows - top > dh { up } else { down };
                    for k in 0..TAIL.min(top) {
                        let (y, density) = (top - 1 - k, 16 - (k + 1) * 16 / (TAIL + 1));
                        for dx in 0..2 {
                            if (BAYER[y % 4][(x + dx) % 4] as usize) < density {
                                rect(x + dx, y, 1, 1, c);
                            }
                        }
                    }
                }
            },
        )
        .size_full(),
    )
}

/// Sparse Bayer dots over a `w` × `rows` field, thickening toward the bottom row.
fn bayer_hatch(w: usize, rows: usize, mut dot: impl FnMut(usize, usize)) {
    for y in 0..rows {
        let density = 1 + 4 * y / rows;
        for x in 0..w {
            if (BAYER[y % 4][x % 4] as usize) < density {
                dot(x, y);
            }
        }
    }
}

/// The alt-speed limit over the sidebar graph: a dotted line `level` above the bottom, with the space above
/// it hatched through the Bayer pattern, densest at the line, so the throttled headroom reads as a lid.
pub fn ceiling(level: Pixels, color: Hsla) -> Div {
    div()
        .absolute()
        .top_0()
        .bottom_0()
        .left_0()
        .right_0()
        .child(
            gpui::canvas(
                |_, _, _| {},
                move |b, _, window, _| {
                    let w = f32::from(b.size.width) as usize;
                    let line = (f32::from(b.size.height - level).round().max(0.0)) as usize;
                    let mut dot = |x: usize, y: usize, c: Hsla| {
                        let o = gpui::point(b.origin.x + px(x as f32), b.origin.y + px(y as f32));
                        window.paint_quad(gpui::fill(
                            gpui::Bounds::new(o, gpui::size(px(1.), px(1.))),
                            c,
                        ));
                    };
                    bayer_hatch(w, line, |x, y| dot(x, y, color.opacity(0.45)));
                    for x in (0..w).step_by(3) {
                        dot(x, line, color);
                        dot(x + 1, line, color);
                    }
                },
            )
            .size_full(),
        )
}

/// Pieces split into `n` runs; each cell is the fraction of its run that is done. `haves` must not be empty.
fn buckets(haves: &[bool], n: usize) -> Vec<f32> {
    let p = haves.len();
    (0..n)
        .map(|c| {
            let lo = c * p / n;
            let hi = ((c + 1) * p / n).max(lo + 1);
            haves[lo..hi].iter().filter(|&&h| h).count() as f32 / (hi - lo) as f32
        })
        .collect()
}

/// Every piece on a full-width square, row by row, in `progress_bar`'s grain: 2px cells with 1px gaps. A
/// partly done cell dithers through the Bayer pattern, so download fronts read as texture. Without a piece map
/// (still checking) the whole square dithers at `fraction`.
pub fn piece_matrix(haves: Vec<bool>, fraction: f32, color: Hsla, t: &Theme) -> Div {
    let (track, gap) = (t.elevated, t.surface);
    let fraction = fraction.clamp(0.0, 1.0);
    let mut el = div().w_full();
    el.style().aspect_ratio = Some(1.0);
    el.child(
        gpui::canvas(
            |_, _, _| {},
            move |b, _, window, _| {
                let n = ((f32::from(b.size.width) + 1.0) / 3.0).floor() as usize;
                let cells = if haves.is_empty() {
                    vec![fraction; n * n]
                } else {
                    buckets(&haves, n * n)
                };
                let on =
                    |x: usize, y: usize| cells[y * n + x] * 16.0 > BAYER[y % 4][x % 4] as f32 + 0.5;
                let side = px(n as f32 * 3.0 - 1.0);
                // Each row as runs of one colour, then 1px column gaps cut them into cells.
                for y in 0..n {
                    let top = b.origin.y + px(y as f32 * 3.0);
                    let mut x0 = 0;
                    for x in 1..=n {
                        if x == n || on(x, y) != on(x0, y) {
                            let run = gpui::Bounds::new(
                                gpui::point(b.origin.x + px(x0 as f32 * 3.0), top),
                                gpui::size(px((x - x0) as f32 * 3.0 - 1.0), px(2.)),
                            );
                            window
                                .paint_quad(gpui::fill(run, if on(x0, y) { color } else { track }));
                            x0 = x;
                        }
                    }
                }
                for x in 1..n {
                    let line = gpui::Bounds::new(
                        gpui::point(b.origin.x + px(x as f32 * 3.0 - 1.0), b.origin.y),
                        gpui::size(px(1.), side),
                    );
                    window.paint_quad(gpui::fill(line, gap));
                }
            },
        )
        .size_full(),
    )
}

/// A bar with a sliding segment, for work of unknown length (fetching metadata).
pub fn busy_bar(key: &str, color: Hsla, t: &Theme, height: Pixels) -> AnyElement {
    let track = div()
        .h(height)
        .w_full()
        .rounded_full()
        .bg(t.elevated)
        .overflow_hidden()
        .relative();
    let seg = move |x: f32| {
        div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(relative(x))
            .w(relative(0.3))
            .rounded_full()
            .bg(color.opacity(0.8))
    };
    if !t.motion {
        return track.child(seg(0.0)).into_any_element();
    }
    let anim = Animation::new(Duration::from_millis(1400))
        .repeat()
        .with_easing(gpui::ease_in_out);
    track
        .with_animation(
            SharedString::from(format!("busy-{key}")),
            anim,
            move |d, p| d.child(seg(-0.3 + p * 1.3)),
        )
        .into_any_element()
}

/// Connected peers orbiting you (the centre dot), in the 1px grain of the other widgets. Faster peers sit
/// closer; data travels their dotted spokes as moving dots (inward ↓, outward ↑). `peers` is (addr, ↓, ↑ B/s).
pub fn peer_radar<'a>(
    peers: impl IntoIterator<Item = (&'a str, f64, f64)>,
    t: &Theme,
) -> AnyElement {
    use std::hash::{Hash, Hasher};
    let (accent, down, up, faint, ring) = (t.accent, t.down, t.up, t.faint, t.border_strong);
    // A stable angle per address, so peers keep their place between ticks.
    let peers: Vec<(f32, f64, f64)> = peers
        .into_iter()
        .map(|(addr, d, u)| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            addr.hash(&mut h);
            ((h.finish() % 10_000) as f32 / 10_000.0, d, u)
        })
        .collect();
    let max = peers.iter().map(|p| p.1 + p.2).fold(1.0, f64::max);
    let peers = std::rc::Rc::new(peers);
    // `p` runs 0..1 over a minute: one full orbit, 20 data trips, 15 pulses, so the loop is seamless.
    let frame = move |p: f32| {
        let peers = peers.clone();
        gpui::canvas(
            |_, _, _| {},
            move |b, _, window, _| {
                let tau = std::f32::consts::TAU;
                let (w, h) = (f32::from(b.size.width), f32::from(b.size.height));
                let (cx, cy) = (
                    f32::from(b.origin.x) + w / 2.0,
                    f32::from(b.origin.y) + h / 2.0,
                );
                let rmax = w.min(h) / 2.0 - 4.0;
                let mut dot = |r: f32, a: f32, s: f32, c: Hsla| {
                    let o = gpui::point(
                        px((cx + r * a.cos() - s / 2.0).round()),
                        px((cy + r * a.sin() - s / 2.0).round()),
                    );
                    window.paint_quad(gpui::fill(
                        gpui::Bounds::new(o, gpui::size(px(s), px(s))),
                        c,
                    ));
                };
                let mut circle = |r: f32, gap: f32, c: Hsla| {
                    let n = (tau * r / gap) as usize;
                    for i in 0..n {
                        dot(r, tau * i as f32 / n as f32, 1.0, c);
                    }
                };
                for k in 1..=3 {
                    circle(rmax * k as f32 / 3.0, 5.0, ring);
                }
                let pulse = (p * 15.0).fract();
                circle(rmax * pulse, 4.0, accent.opacity(0.6 * (1.0 - pulse)));
                for &(turn, d, u) in peers.iter() {
                    let active = d + u >= 1.0;
                    let a = (turn + p) * tau;
                    let r = rmax * (1.0 - 0.65 * ((d + u) / max).sqrt() as f32);
                    if active {
                        for s in (8..r as usize).step_by(6) {
                            dot(s as f32, a, 1.0, faint.opacity(0.6));
                        }
                    }
                    let flow = (p * 20.0 + turn).fract();
                    if d >= 1.0 {
                        dot(r * (1.0 - flow), a, 2.0, down);
                    }
                    if u >= 1.0 {
                        dot(r * flow, a, 2.0, up);
                    }
                    let c = if d >= 1.0 {
                        down
                    } else if u >= 1.0 {
                        up
                    } else {
                        faint
                    };
                    dot(r, a, if active { 4.0 } else { 3.0 }, c);
                }
                dot(0.0, 0.0, 6.0, accent);
            },
        )
        .size_full()
    };
    let el = div().w_full().h(px(180.)).flex_none();
    if !t.motion {
        return el.child(frame(0.0)).into_any_element();
    }
    let anim = Animation::new(Duration::from_secs(60)).repeat();
    el.with_animation("peer-radar", anim, move |d, p| d.child(frame(p)))
        .into_any_element()
}

/// Torrie's logo. 256px, so it stays sharp up to the About window's size.
pub fn logo(size: Pixels) -> gpui::Img {
    gpui::img(std::sync::Arc::new(gpui::Image::from_bytes(
        gpui::ImageFormat::Png,
        include_bytes!("../assets/logo.png").to_vec(),
    )))
    .size(size)
}

pub fn pill(label: impl Into<SharedString>, color: Hsla, t: &Theme) -> Div {
    div()
        .flex_none()
        .px_2()
        .py(px(1.))
        .rounded_full()
        .bg(t.tint(color))
        .text_color(color)
        .text_size(px(10.5))
        .font_weight(gpui::FontWeight::MEDIUM)
        .child(label.into())
}

pub struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = Theme::of(window, cx);
        div()
            .font_family(t.font.clone())
            .text_size(px(11.))
            .px_2()
            .py_1()
            .rounded(t.r_md())
            .bg(t.surface)
            .border_1()
            .border_color(t.border_strong)
            .text_color(t.text)
            .shadow_md()
            .child(self.0.clone())
    }
}

pub fn tip(text: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_, cx| cx.new(|_| Tip(text.into())).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tween_retargets_from_current_value() {
        let mut t = Tween::new(0.0);
        t.set(1.0, 10_000, true);
        assert!(t.moving() && t.value() < 0.1);
        // Interrupted: reverses from where it is, not from the old endpoint.
        t.from = 0.5;
        t.set(0.0, 10_000, true);
        assert!((t.value() - 0.5).abs() < 0.05);
        // Motion off, or a sub-pixel change: snaps.
        t.set(0.8, 10_000, false);
        assert_eq!((t.value(), t.moving()), (0.8, false));
        t.set(0.801, 10_000, true);
        assert!(!t.moving());
    }

    #[test]
    fn buckets_cover_every_piece() {
        // More cells than pieces: each cell shows one piece.
        assert_eq!(buckets(&[true, false], 4), vec![1.0, 1.0, 0.0, 0.0]);
        // More pieces than cells: runs average.
        assert_eq!(
            buckets(&[true, true, true, false, false, false], 2),
            vec![1.0, 0.0]
        );
        assert_eq!(buckets(&[true, false, false, true], 2), vec![0.5, 0.5]);
    }

    #[test]
    fn builtin_text_is_readable() {
        for (name, dark, c) in BUILTIN {
            let t = palette(dark, c);
            for bg in [t.bg, t.sidebar, t.surface, t.hover, t.elevated] {
                let roles = [
                    ("text", t.text, 7.0),
                    ("muted", t.muted, 6.0),
                    ("faint", t.faint, 4.5),
                    ("accent", t.accent, 4.5),
                    ("down", t.down, 4.5),
                    ("up", t.up, 4.5),
                    ("checking", t.checking, 4.5),
                    ("paused", t.paused, 4.5),
                    ("error", t.error, 4.5),
                ];
                for (role, fg, min) in roles {
                    // Hex rounding can land a hair under the target.
                    assert!(contrast(fg, bg) >= min - 0.05, "{name} {role}");
                }
            }
            assert!(contrast(t.on_accent(), t.accent) >= 4.5, "{name} on_accent");
            for a in ACCENTS {
                let mut t = t.clone();
                t.set_accent(Rgba::try_from(a).unwrap().into());
                assert!(contrast(t.accent, t.surface) >= 4.45, "{name} {a}");
                assert!(
                    contrast(t.on_accent(), t.accent) >= 4.5,
                    "{name} {a} on_accent"
                );
            }
        }
    }

    #[test]
    fn theme_files_extend_a_base() {
        let list: Vec<(String, Theme)> = BUILTIN
            .iter()
            .map(|(n, d, c)| (n.to_string(), palette(*d, *c)))
            .collect();
        let dir = std::env::temp_dir().join(format!("torrie-theme-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = |body: &str| {
            let p = dir.join("t.json");
            std::fs::write(&p, body).unwrap();
            load_theme(&p, &list)
        };
        let t = file(r##"{"extends": "Light", "accent": "#ff0000", "radius": 10}"##).unwrap();
        assert!(!t.dark && t.radius == 10.0 && t.accent == Hsla::from(rgb(0xff0000)));
        assert_eq!(t.bg, list[0].1.bg);
        assert!(
            file(r##"{"acent": "#ff0000"}"##).is_err(),
            "typos are reported, not ignored"
        );
        assert!(file(r#"{"extends": "Nope"}"#).is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
