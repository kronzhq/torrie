//! Remote control. Torrie's own API is JSON-RPC 2.0 at `/torrie/rpc`, described in `openrpc.json` (also
//! served by `rpc.discover`). The same server speaks the Transmission protocol at `/transmission/rpc`
//! (`transmission.rs`), for apps built for it. Both use the torrent operations here. `/` serves
//! the web remote (`assets/remote.html`), a page that calls the Torrie API, with its font and logo.
//!
//! Safety: it listens on 127.0.0.1 unless "Allow other devices" is on. A request that isn't from this Mac
//! (judged by the connection's address, and a localhost `Host` against DNS rebinding) needs HTTP Basic auth
//! with the generated password. A browser that signed in gets a cookie holding it (HttpOnly, SameSite=Strict),
//! so it isn't asked again; a new password signs everyone out. Against web pages (CSRF), the Torrie API
//! refuses a non-JSON body or an `Origin` other than its own (the web remote's): a browser can't send that
//! cross-origin without a CORS preflight, which is never answered. A rebound DNS name passes the `Origin` check, but its `Host` isn't
//! local, so it still needs the password, which the browser only holds for the real address.
//! The Transmission protocol uses its session-id handshake instead. Off by default.
//! Plain HTTP, so on the network the password travels readable; add TLS if Torrie is ever used on untrusted
//! networks.

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
};

use anyhow::{Context as _, bail, ensure};
use axum::{
    Json, Router,
    body::Bytes,
    extract::{ConnectInfo, DefaultBodyLimit, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine as _;
use librqbit::{AddTorrent, Magnet, ManagedTorrent, torrent_from_bytes};
use serde_json::{Map, Value, json};

use crate::{
    engine::{Engine, Settings},
    format::Row,
    plugins::torrent_dir,
    theme::FONTS,
};

pub const PATH: &str = "/torrie/rpc";
const OPENRPC: &str = include_str!("../openrpc.json");
const REMOTE: &str = include_str!("../assets/remote.html");
const LOGO: &[u8] = include_bytes!("../assets/logo.png");
/// Remembers a browser that signed in, so phones don't ask for the password every visit.
const COOKIE: &str = "torrie_password";
/// Settings `settings.set` can't change: they decide what runs and who connects, or only apply from the
/// Settings window.
const LOCKED: [&str; 16] = [
    "plugins_enabled",
    "plugin_settings",
    "rpc_enabled",
    "rpc_port",
    "rpc_lan",
    "rpc_password",
    "appearance",
    "light_theme",
    "dark_theme",
    "accent",
    "font",
    "animations",
    "menu_bar",
    "open_at_login",
    "menu_bar_mode",
    "check_updates",
];

mod transmission;

/// Starts the server if remote control is on; a busy port shows as an error in the window.
pub fn start(engine: &Arc<Engine>) {
    let s = engine.settings();
    if !s.rpc_enabled {
        return;
    }
    // Torrent files arrive base64-encoded inside the JSON.
    let fonts = [400, 500, 600, 700]
        .into_iter()
        .zip(FONTS)
        .fold(Router::new(), |r, (weight, font)| {
            r.route(&format!("/fonts/{weight}.ttf"), file("font/ttf", font))
        });
    let app = Router::new()
        .route(PATH, post(handle))
        .with_state(engine.clone())
        .route("/", get(|| async { Html(REMOTE) }))
        .route("/logo.png", file("image/png", LOGO))
        .merge(fonts)
        .merge(transmission::router(engine.clone()))
        .layer(DefaultBodyLimit::max(32 << 20))
        .layer(middleware::from_fn_with_state(engine.clone(), guard));
    engine.run(async move {
        let ip = if s.rpc_lan {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        };
        let listener = tokio::net::TcpListener::bind(SocketAddr::from((ip, s.rpc_port)))
            .await
            .with_context(|| format!("Remote control: port {} is busy", s.rpc_port))?;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .context("Remote control")
    });
}

/// A file the web remote loads, cached for a day.
fn file(content_type: &'static str, bytes: &'static [u8]) -> axum::routing::MethodRouter {
    get(move || async move {
        let headers = [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "max-age=86400"),
        ];
        (headers, bytes)
    })
}

/// Lets in this Mac, and anyone else with the password. Read per request, so a new password applies at once.
async fn guard(
    State(e): State<Arc<Engine>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let password = e.settings().rpc_password;
    let (local, signed_in, basic) = {
        let header = |name| req.headers().get(name).and_then(|v| v.to_str().ok());
        (
            peer.ip().is_loopback() && header(header::HOST).is_some_and(local_host),
            header(header::COOKIE).is_some_and(|c| remembered(c, &password)),
            authorized(header(header::AUTHORIZATION), &password),
        )
    };
    if !(local || signed_in || basic) {
        let challenge = [(header::WWW_AUTHENTICATE, "Basic realm=\"Torrie\"")];
        return (
            StatusCode::UNAUTHORIZED,
            challenge,
            "Torrie: wrong or missing remote control password",
        )
            .into_response();
    }
    let mut res = next.run(req).await;
    if basic && !signed_in {
        let cookie =
            format!("{COOKIE}={password}; Max-Age=31536000; Path=/; HttpOnly; SameSite=Strict");
        if let Ok(v) = cookie.parse() {
            res.headers_mut().append(header::SET_COOKIE, v);
        }
    }
    res
}

/// Whether a `Cookie` header holds the password.
fn remembered(cookies: &str, password: &str) -> bool {
    cookies
        .split(';')
        .filter_map(|c| c.trim().split_once('='))
        .any(|(name, value)| name == COOKIE && same(value, password))
}

/// HTTP Basic auth with `password` and any username.
fn authorized(auth: Option<&str>, password: &str) -> bool {
    let given = auth.and_then(|a| a.strip_prefix("Basic ")).and_then(|b| {
        base64::engine::general_purpose::STANDARD
            .decode(b.trim())
            .ok()
    });
    given
        .as_deref()
        .and_then(|g| std::str::from_utf8(g).ok())
        .and_then(|g| g.split_once(':'))
        .is_some_and(|(_, pass)| same(pass, password))
}

/// `given == password`, comparing every byte so timing doesn't reveal how much matched. An empty password
/// locks everyone out.
fn same(given: &str, password: &str) -> bool {
    !password.is_empty()
        && given.len() == password.len()
        && given
            .bytes()
            .zip(password.bytes())
            .fold(0, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// 10 characters (50 bits): quick to type on a phone, which then remembers it.
pub fn new_password() -> String {
    random(10, 5)
}

/// `len` characters from macOS's CSPRNG (5 bits each), unambiguous for typing, in groups of `group`.
pub fn random(len: usize, group: usize) -> String {
    const ABC: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut b = vec![0u8; len];
    // SAFETY: fills exactly `b.len()` bytes of `b`.
    unsafe { libc::arc4random_buf(b.as_mut_ptr().cast(), b.len()) };
    b.chunks(group)
        .map(|c| {
            c.iter()
                .map(|x| ABC[(x & 31) as usize] as char)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("-")
}

fn local_host(host: &str) -> bool {
    let name = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => host.split(':').next().unwrap_or(""),
    };
    matches!(name, "localhost" | "127.0.0.1" | "::1")
}

/// One JSON-RPC 2.0 call. Batches aren't supported.
async fn handle(State(e): State<Arc<Engine>>, headers: HeaderMap, body: Bytes) -> Response {
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !same_origin(&headers) || !json {
        return (
            StatusCode::FORBIDDEN,
            "Send Content-Type: application/json, without another site's Origin header",
        )
            .into_response();
    }
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return reply(Some(Value::Null), Err((-32700, "parse error".into())));
    };
    let id = req.get("id").cloned();
    let (Some(method), Some("2.0")) = (
        req.get("method").and_then(Value::as_str),
        req.get("jsonrpc").and_then(Value::as_str),
    ) else {
        return reply(
            Some(id.unwrap_or(Value::Null)),
            Err((-32600, "invalid request".into())),
        );
    };
    let p = req.get("params").cloned().unwrap_or_else(|| json!({}));
    reply(id, call(&e, method, &p))
}

/// No `Origin` (curl, scripts), or the server's own (the web remote).
fn same_origin(headers: &HeaderMap) -> bool {
    let h = |name| headers.get(name).and_then(|v| v.to_str().ok());
    h(header::ORIGIN)
        .is_none_or(|o| h(header::HOST).is_some_and(|host| o == format!("http://{host}")))
}

/// One API call, over HTTP or from a plugin process.
pub fn call(e: &Engine, method: &str, p: &Value) -> Result<Value, (i64, String)> {
    let out = match method {
        "rpc.discover" => serde_json::from_str(OPENRPC).map_err(Into::into),
        "settings.get" => Ok(json!(e.settings())),
        "settings.set" => settings_set(e, p),
        "torrents.list" => Ok(json!(
            select(e, p.get("ids"))
                .iter()
                .map(|(id, h)| torrent(e, *id, h))
                .collect::<Vec<_>>()
        )),
        "torrents.add" => {
            let s = |k| p.get(k).and_then(Value::as_str);
            add(e, s("link"), s("torrent"), s("dir"), p.get("paused").and_then(Value::as_bool).unwrap_or(false))
                .map(|(name, hash, id)| json!({ "name": name, "hash": hash, "id": id, "duplicate": id.is_some() }))
        }
        "torrents.pause" => each(e, p, |id| e.set_paused(id, true)),
        "torrents.resume" => each(e, p, |id| e.set_paused(id, false)),
        "torrents.remove" => {
            let delete = p
                .get("delete_files")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            each(e, p, |id| e.remove(id, delete))
        }
        _ => return Err((-32601, format!("method not found: {method}"))),
    };
    out.map_err(|e| (-32602, format!("{e:#}")))
}

/// A call without an `id` is a notification: it runs, but gets no reply.
fn reply(id: Option<Value>, out: Result<Value, (i64, String)>) -> Response {
    let Some(id) = id else {
        return StatusCode::NO_CONTENT.into_response();
    };
    Json(response(id, out)).into_response()
}

pub fn response(id: Value, out: Result<Value, (i64, String)>) -> Value {
    match out {
        Ok(result) => json!({ "jsonrpc": "2.0", "result": result, "id": id }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "error": { "code": code, "message": message }, "id": id })
        }
    }
}

/// Runs `f` on each torrent in `ids`, which is required so a forgotten one can't hit every torrent.
fn each(e: &Engine, p: &Value, f: impl Fn(usize)) -> anyhow::Result<Value> {
    let hits = select(e, Some(p.get("ids").context("\"ids\" is required")?));
    hits.iter().for_each(|(id, _)| f(*id));
    Ok(json!(hits.len()))
}

fn settings_set(e: &Engine, p: &Value) -> anyhow::Result<Value> {
    let changes = p
        .as_object()
        .context("params must be an object of settings")?;
    let mut res = Ok(());
    e.update_settings(|s| match merge(s, changes) {
        Ok(new) => *s = new,
        Err(err) => res = Err(err),
    });
    res.map(|_| json!(e.settings()))
}

/// `s` with `changes` applied. Unknown keys are refused so typos don't pass silently.
fn merge(s: &Settings, changes: &Map<String, Value>) -> anyhow::Result<Settings> {
    let mut all = json!(s);
    for (k, v) in changes {
        ensure!(
            !LOCKED.contains(&k.as_str()),
            "\"{k}\" can only be changed in the Settings window"
        );
        *all.get_mut(k)
            .with_context(|| format!("unknown setting \"{k}\""))? = v.clone();
    }
    let new: Settings = serde_json::from_value(all).context("invalid value")?;
    ensure!(
        new.download_dir.is_absolute(),
        "download_dir must be an absolute path"
    );
    Ok(new)
}

pub fn torrent(e: &Engine, id: usize, h: &ManagedTorrent) -> Value {
    torrent_json(&e.row(id, h), h)
}

pub fn torrent_json(r: &Row, h: &ManagedTorrent) -> Value {
    json!({
        "id": r.id,
        "name": r.name,
        "hash": h.info_hash().as_string(),
        "state": format!("{:?}", r.state).to_lowercase(),
        "error": r.error,
        "progress": r.fraction(),
        "size": r.total,
        "downloaded": r.done,
        "uploaded": r.uploaded,
        "ratio": r.ratio(),
        "download_rate": r.down_bps as u64,
        "upload_rate": r.up_bps as u64,
        "eta": eta(r),
        "peers": r.peers_live,
        "path": torrent_dir(h.output_folder(), &r.name).join(&r.name),
    })
}

/// Seconds left: 0 once done, `None` while nothing is coming in.
pub fn eta(r: &Row) -> Option<u64> {
    let left = r.total.saturating_sub(r.done);
    if left == 0 {
        Some(0)
    } else {
        (r.down_bps >= 1.0).then(|| (left as f64 / r.down_bps) as u64)
    }
}

/// Torrents named by `ids` (an id, an info hash, or a list of them), or every torrent without `ids`.
/// Magnets still fetching metadata aren't torrents yet, so aren't included.
pub fn select(e: &Engine, ids: Option<&Value>) -> Vec<(usize, Arc<ManagedTorrent>)> {
    let all: Vec<_> = e
        .session
        .with_torrents(|it| it.map(|(id, h)| (id, h.clone())).collect());
    let Some(ids) = ids else { return all };
    let one = |v: &Value, id: usize, h: &ManagedTorrent| match v {
        Value::Number(n) => n.as_u64() == Some(id as u64),
        Value::String(s) => s.eq_ignore_ascii_case(&h.info_hash().as_string()),
        _ => false,
    };
    all.into_iter()
        .filter(|(id, h)| match ids {
            Value::Array(list) => list.iter().any(|v| one(v, *id, h)),
            v => one(v, *id, h),
        })
        .collect()
}

/// Adds a magnet, http(s) URL or local `.torrent` path (`link`), or a base64 `.torrent` file, into `dir`
/// (the download folder if `None`). Returns its name, its info hash (empty for an http URL, unknown until
/// fetched) and, if it was already there, its id. Adding finishes in the background, so new ones have no id yet.
pub fn add(
    e: &Engine,
    link: Option<&str>,
    base64: Option<&str>,
    dir: Option<&str>,
    paused: bool,
) -> anyhow::Result<(String, String, Option<usize>)> {
    let dir = dir.map(PathBuf::from);
    ensure!(
        dir.as_ref().is_none_or(|d| d.is_absolute()),
        "the folder must be an absolute path"
    );
    let (add, name, hash) = if let Some(b64) = base64 {
        let clean: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(clean)
            .context("the torrent file isn't base64")?;
        let (name, hash) = parse_torrent(&bytes)?;
        (AddTorrent::from_bytes(bytes), name, hash)
    } else if let Some(f) = link {
        if f.starts_with("magnet:") {
            let m = Magnet::parse(f).context("invalid magnet link")?;
            let hash = m.as_id20().map(|h| h.as_string()).unwrap_or_default();
            (
                AddTorrent::from_url(f.to_owned()),
                m.name.clone().unwrap_or_else(|| hash.clone()),
                hash,
            )
        } else if f.starts_with("http://") || f.starts_with("https://") {
            (
                AddTorrent::from_url(f.to_owned()),
                f.to_owned(),
                String::new(),
            )
        } else {
            let bytes = std::fs::read(f).with_context(|| format!("reading {f}"))?;
            let (name, hash) = parse_torrent(&bytes)?;
            (AddTorrent::from_bytes(bytes), name, hash)
        }
    } else {
        bail!("nothing to add: give a link or a torrent file");
    };
    if !hash.is_empty()
        && let Some((id, h)) = select(e, None)
            .into_iter()
            .find(|(_, h)| h.info_hash().as_string() == hash)
    {
        return Ok((h.name().unwrap_or(name), hash, Some(id)));
    }
    e.add(add, name.clone(), dir, paused, false);
    Ok((name, hash, None))
}

fn parse_torrent(bytes: &[u8]) -> anyhow::Result<(String, String)> {
    let t = torrent_from_bytes(bytes).map_err(|e| anyhow::anyhow!("invalid torrent: {e}"))?;
    let hash = t.info_hash.as_string();
    let name = t
        .info
        .data
        .name
        .as_ref()
        .map(|n| String::from_utf8_lossy(n.as_ref()).into_owned())
        .unwrap_or_else(|| hash.clone());
    Ok((name, hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_localhost() {
        for ok in [
            "localhost",
            "localhost:9091",
            "127.0.0.1:9091",
            "[::1]:9091",
        ] {
            assert!(local_host(ok), "{ok}");
        }
        for bad in [
            "evil.com",
            "localhost.evil.com:9091",
            "127.0.0.1.nip.io",
            "192.168.1.2:9091",
            "",
        ] {
            assert!(!local_host(bad), "{bad}");
        }
    }

    #[test]
    fn origin() {
        // Host 192.168.1.2:9091, plus an Origin if given.
        let check = |origin: Option<&str>| {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, "192.168.1.2:9091".parse().unwrap());
            if let Some(o) = origin {
                h.insert(header::ORIGIN, o.parse().unwrap());
            }
            same_origin(&h)
        };
        assert!(check(None), "no Origin");
        assert!(check(Some("http://192.168.1.2:9091")));
        assert!(!check(Some("http://evil.com")));
        assert!(!check(Some("https://192.168.1.2:9091")));
        let mut no_host = HeaderMap::new();
        no_host.insert(header::ORIGIN, "http://localhost:9091".parse().unwrap());
        assert!(!same_origin(&no_host), "no Host");
    }

    #[test]
    fn basic_auth() {
        let pw = new_password();
        assert_eq!((pw.len(), pw.matches('-').count()), (11, 1));
        assert_ne!(pw, new_password());
        let basic = |s: &str| {
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(s)
            )
        };
        assert!(authorized(Some(&basic(&format!("anyone:{pw}"))), &pw));
        assert!(authorized(Some(&basic(&format!(":{pw}"))), &pw));
        assert!(!authorized(Some(&basic("anyone:wrong")), &pw));
        assert!(!authorized(Some(&basic(&pw)), &pw), "no colon");
        assert!(
            !authorized(Some(&basic("anyone:")), ""),
            "empty password locks out"
        );
        assert!(!authorized(Some("Bearer x"), &pw));
        assert!(!authorized(None, &pw));
        assert!(remembered(&format!("a=1; {COOKIE}={pw}"), &pw));
        assert!(!remembered(&format!("{COOKIE}=wrong; a={pw}"), &pw));
        assert!(
            !remembered(&format!("{COOKIE}="), ""),
            "empty password locks out"
        );
    }

    #[test]
    fn settings_merge() {
        let s = Settings::default();
        let set = |v: Value| merge(&s, v.as_object().unwrap()).map_err(|e| format!("{e:#}"));
        let new =
            set(json!({"speed_limit_down_kbps": 500, "speed_limit_down_enabled": true})).unwrap();
        assert_eq!(
            (new.speed_limit_down_kbps, new.speed_limit_down_enabled),
            (500, true)
        );
        assert!(
            set(json!({"plugins_enabled": ["evil"]}))
                .err()
                .unwrap()
                .contains("Settings window")
        );
        assert!(
            set(json!({"rpc_password": ""}))
                .err()
                .unwrap()
                .contains("Settings window")
        );
        assert!(
            set(json!({"speed_limit_dwn": 1}))
                .err()
                .unwrap()
                .contains("unknown")
        );
        assert!(
            set(json!({"peer_port": "x"}))
                .err()
                .unwrap()
                .contains("invalid")
        );
        assert!(
            set(json!({"download_dir": "rel"}))
                .err()
                .unwrap()
                .contains("absolute")
        );
    }

    #[test]
    fn discover_documents_every_method() {
        let doc: Value = serde_json::from_str(OPENRPC).unwrap();
        let mut documented: Vec<&str> = doc["methods"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|m| m["name"].as_str())
            .collect();
        // The arms of `call`'s `match method`.
        let src = include_str!("rpc.rs");
        let start = src.find("let out = match method {").unwrap();
        let end = start + src[start..].find("_ => return Err").unwrap();
        let mut handled: Vec<&str> = src[start..end]
            .lines()
            .filter_map(|l| l.trim().strip_prefix('"')?.split('"').next())
            .collect();
        documented.sort();
        handled.sort();
        assert_eq!(documented, handled);
    }
}
