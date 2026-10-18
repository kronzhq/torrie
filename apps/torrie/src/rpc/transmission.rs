//! The Transmission RPC protocol (rpc-version 17), which many download managers,
//! `transmission-remote` and phone remotes already speak. Its path, header and field names are fixed by the
//! protocol, since those tools hard-code them. Served by remote control (`rpc.rs`), off by default.

use std::{
    hash::{BuildHasher, Hasher},
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context as _, bail};
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use librqbit::ManagedTorrent;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::{
    engine::{Engine, Pending, Settings},
    format::{Row, State as St},
    plugins::torrent_dir,
    rpc::{self, select},
};

pub const PATH: &str = "/transmission/rpc";
const SESSION_ID: &str = "X-Transmission-Session-Id";

struct Rpc {
    engine: Arc<Engine>,
    session_id: String,
}

pub fn router(engine: Arc<Engine>) -> Router {
    Router::new()
        .route(PATH, any(handle))
        .with_state(Arc::new(Rpc {
            engine,
            session_id: session_id(),
        }))
}

/// 128 random bits from the OS-seeded hasher keys.
fn session_id() -> String {
    let r = || {
        std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish()
    };
    format!("{:016x}{:016x}", r(), r())
}

#[derive(Deserialize)]
struct Request {
    method: String,
    #[serde(default)]
    arguments: Value,
    tag: Option<Value>,
}

/// A 409 hands out the session id, which a cross-origin web page can't read.
async fn handle(State(rpc): State<Arc<Rpc>>, headers: HeaderMap, body: Bytes) -> Response {
    if headers.get(SESSION_ID).and_then(|v| v.to_str().ok()) != Some(rpc.session_id.as_str()) {
        let sid = rpc.session_id.clone();
        let body = format!(
            "<h1>409: Conflict</h1><p>Invalid session-id header.</p><p><code>{SESSION_ID}: {sid}</code></p>"
        );
        return (StatusCode::CONFLICT, [(SESSION_ID, sid)], body).into_response();
    }
    let req: Request = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, format!("invalid request: {e}")).into_response();
        }
    };
    let (result, arguments) = match call(&rpc, &req.method, &req.arguments) {
        Ok(args) => ("success".to_owned(), args),
        Err(e) => (format!("{e:#}"), json!({})),
    };
    let mut res = json!({ "result": result, "arguments": arguments });
    if let Some(tag) = req.tag {
        res["tag"] = tag;
    }
    Json(res).into_response()
}

fn call(rpc: &Rpc, method: &str, args: &Value) -> anyhow::Result<Value> {
    let e = &rpc.engine;
    Ok(match method {
        "session-get" => only(session(&e.settings(), &rpc.session_id), args.get("fields")),
        "session-set" => {
            let obj = args.as_object().context("arguments must be an object")?;
            if let Some(dir) = obj.get("download-dir").and_then(Value::as_str) {
                anyhow::ensure!(
                    PathBuf::from(dir).is_absolute(),
                    "download-dir must be an absolute path"
                );
            }
            e.update_settings(|s| session_set(s, obj));
            json!({})
        }
        "session-stats" => stats(e),
        "torrent-get" => torrent_get(e, args),
        "torrent-add" => {
            let s = |k| args.get(k).and_then(Value::as_str);
            let paused = args.get("paused").and_then(Value::as_bool).unwrap_or(false);
            let (name, hash, dup) =
                rpc::add(e, s("filename"), s("metainfo"), s("download-dir"), paused)?;
            match dup {
                Some(id) => {
                    json!({ "torrent-duplicate": { "id": id, "name": name, "hashString": hash } })
                }
                None => json!({ "torrent-added": { "name": name, "hashString": hash } }),
            }
        }
        "torrent-start" | "torrent-start-now" => {
            ids(e, args)
                .into_iter()
                .for_each(|i| e.set_paused(i.id(), false));
            json!({})
        }
        "torrent-stop" => {
            ids(e, args)
                .into_iter()
                .for_each(|i| e.set_paused(i.id(), true));
            json!({})
        }
        "torrent-verify" => {
            ids(e, args).into_iter().for_each(|i| e.verify(i.id()));
            json!({})
        }
        "torrent-reannounce" => {
            ids(e, args).into_iter().for_each(|i| e.reannounce(i.id()));
            json!({})
        }
        "torrent-remove" => {
            let delete = args
                .get("delete-local-data")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            ids(e, args)
                .into_iter()
                .for_each(|i| e.remove(i.id(), delete));
            json!({})
        }
        // Download managers send per-torrent seeding limits, labels and queue moves; Torrie has no
        // per-torrent equivalents, and failing would fail their grab.
        // Accepted and ignored; map `files-wanted`/`labels` if a remote needs them.
        "torrent-set" | "queue-move-top" | "queue-move-up" | "queue-move-down"
        | "queue-move-bottom" => json!({}),
        _ => bail!("method name not recognized"),
    })
}

/// Keeps only the requested `fields`, if any were named.
fn only(all: Value, fields: Option<&Value>) -> Value {
    let (Value::Object(all), Some(Value::Array(fields))) = (&all, fields) else {
        return all;
    };
    let want: Vec<&str> = fields.iter().filter_map(Value::as_str).collect();
    Value::Object(
        all.iter()
            .filter(|(k, _)| want.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
    )
}

fn session(s: &Settings, sid: &str) -> Value {
    json!({
        // Clients parse the leading version number to check support.
        "version": format!("4.0.6 (Torrie {})", env!("CARGO_PKG_VERSION")),
        "rpc-version": 17,
        "rpc-version-minimum": 14,
        "rpc-version-semver": "5.3.0",
        "session-id": sid,
        "config-dir": crate::engine::config_dir(),
        "download-dir": s.download_dir,
        "peer-port": s.peer_port,
        "peer-limit-per-torrent": s.peer_limit_per_torrent,
        "port-forwarding-enabled": s.port_forwarding,
        "dht-enabled": true,
        "utp-enabled": true,
        "speed-limit-down": s.speed_limit_down_kbps,
        "speed-limit-down-enabled": s.speed_limit_down_enabled,
        "speed-limit-up": s.speed_limit_up_kbps,
        "speed-limit-up-enabled": s.speed_limit_up_enabled,
        "alt-speed-down": s.alt_speed_down_kbps,
        "alt-speed-up": s.alt_speed_up_kbps,
        "alt-speed-enabled": s.alt_speed_enabled,
        "alt-speed-time-enabled": s.alt_speed_time_enabled,
        "alt-speed-time-begin": s.alt_speed_begin,
        "alt-speed-time-end": s.alt_speed_end,
        "alt-speed-time-day": s.alt_speed_days,
        "seedRatioLimit": s.ratio_limit,
        "seedRatioLimited": s.ratio_limit_enabled,
        "units": {
            "speed-units": ["kB/s", "MB/s", "GB/s", "TB/s"], "speed-bytes": 1000,
            "size-units": ["kB", "MB", "GB", "TB"], "size-bytes": 1000,
            "memory-units": ["KiB", "MiB", "GiB", "TiB"], "memory-bytes": 1024,
        },
    })
}

/// Unknown keys are ignored; `sanitize` clamps the rest.
fn session_set(s: &mut Settings, args: &Map<String, Value>) {
    for (k, v) in args {
        let (b, n) = (v.as_bool(), v.as_u64());
        let num = |n: Option<u64>| n.map(|n| n.min(u32::MAX as u64) as u32);
        match k.as_str() {
            "download-dir" => {
                s.download_dir = v.as_str().map_or(s.download_dir.clone(), PathBuf::from)
            }
            "peer-port" => s.peer_port = n.map_or(s.peer_port, |n| n.min(u16::MAX as u64) as u16),
            "peer-limit-per-torrent" => {
                s.peer_limit_per_torrent = n.map_or(s.peer_limit_per_torrent, |n| n as usize)
            }
            "port-forwarding-enabled" => s.port_forwarding = b.unwrap_or(s.port_forwarding),
            "speed-limit-down" => {
                s.speed_limit_down_kbps = num(n).unwrap_or(s.speed_limit_down_kbps)
            }
            "speed-limit-down-enabled" => {
                s.speed_limit_down_enabled = b.unwrap_or(s.speed_limit_down_enabled)
            }
            "speed-limit-up" => s.speed_limit_up_kbps = num(n).unwrap_or(s.speed_limit_up_kbps),
            "speed-limit-up-enabled" => {
                s.speed_limit_up_enabled = b.unwrap_or(s.speed_limit_up_enabled)
            }
            "alt-speed-down" => s.alt_speed_down_kbps = num(n).unwrap_or(s.alt_speed_down_kbps),
            "alt-speed-up" => s.alt_speed_up_kbps = num(n).unwrap_or(s.alt_speed_up_kbps),
            "alt-speed-enabled" => s.alt_speed_enabled = b.unwrap_or(s.alt_speed_enabled),
            "alt-speed-time-enabled" => {
                s.alt_speed_time_enabled = b.unwrap_or(s.alt_speed_time_enabled)
            }
            "alt-speed-time-begin" => {
                s.alt_speed_begin = n.map_or(s.alt_speed_begin, |n| n.min(1439) as u16)
            }
            "alt-speed-time-end" => {
                s.alt_speed_end = n.map_or(s.alt_speed_end, |n| n.min(1439) as u16)
            }
            "alt-speed-time-day" => {
                s.alt_speed_days = n.map_or(s.alt_speed_days, |n| (n & 0x7f) as u8)
            }
            "seedRatioLimit" => s.ratio_limit = v.as_f64().unwrap_or(s.ratio_limit),
            "seedRatioLimited" => s.ratio_limit_enabled = b.unwrap_or(s.ratio_limit_enabled),
            _ => {}
        }
    }
}

/// A session torrent, or a magnet still fetching metadata: it has no handle yet, but clients track
/// a grab by its hash from the moment it's added.
enum Item {
    Torrent(usize, Arc<ManagedTorrent>),
    /// Id, name, info hash, folder.
    Magnet(usize, String, String, PathBuf),
}

impl Item {
    fn id(&self) -> usize {
        match self {
            Item::Torrent(id, _) | Item::Magnet(id, ..) => *id,
        }
    }
}

/// Pending ids count down from `usize::MAX`, but clients parse ids as 32-bit ints; count down from `i32::MAX`.
fn wire_id(id: usize) -> usize {
    if id > i32::MAX as usize {
        i32::MAX as usize - (usize::MAX - id)
    } else {
        id
    }
}

/// `ids`: absent (all), an id, a hash, "recently-active", or a list of ids and hashes.
fn ids(e: &Engine, args: &Value) -> Vec<Item> {
    let sel = args.get("ids");
    let hit = |v: &Value, p: &Pending| match v {
        Value::Number(n) => n.as_u64() == Some(wire_id(p.id) as u64),
        Value::String(s) => !p.hash.is_empty() && s.eq_ignore_ascii_case(&p.hash),
        _ => false,
    };
    let mut items: Vec<Item> = torrents(e, sel)
        .into_iter()
        .map(|(id, h)| Item::Torrent(id, h))
        .collect();
    items.extend(
        e.pending
            .lock()
            .unwrap()
            .iter()
            .filter(|p| match sel {
                // Fetching metadata counts as active (`Row::active`).
                None => true,
                Some(Value::String(s)) if s == "recently-active" => true,
                Some(Value::Array(list)) => list.iter().any(|v| hit(v, p)),
                Some(v) => hit(v, p),
            })
            .map(|p| Item::Magnet(p.id, p.name.clone(), p.hash.clone(), p.dir.clone())),
    );
    items
}

fn torrents(e: &Engine, sel: Option<&Value>) -> Vec<(usize, Arc<ManagedTorrent>)> {
    match sel {
        // Lingers past the last traffic (`Row::active`), so remotes polling for changes see a torrent go idle.
        Some(Value::String(s)) if s == "recently-active" => {
            let active: std::collections::HashSet<usize> = e
                .rows()
                .iter()
                .filter(|r| r.active())
                .map(|r| r.id)
                .collect();
            select(e, None)
                .into_iter()
                .filter(|(id, _)| active.contains(id))
                .collect()
        }
        ids => select(e, ids),
    }
}

fn torrent_get(e: &Engine, args: &Value) -> Value {
    let fields: Vec<&str> = match args.get("fields") {
        Some(Value::Array(f)) => f.iter().filter_map(Value::as_str).collect(),
        _ => vec!["id", "name", "status", "percentDone", "hashString"],
    };
    let rows = ids(e, args).into_iter().map(|it| {
        let (r, h, hash, dir) = match &it {
            Item::Torrent(id, h) => {
                let r = e.row(*id, h);
                let dir = torrent_dir(h.output_folder(), &r.name);
                (r, Some(h.as_ref()), h.info_hash().as_string(), dir)
            }
            Item::Magnet(id, name, hash, dir) => {
                (Row::pending(*id, name), None, hash.clone(), dir.clone())
            }
        };
        fields
            .iter()
            .map(|f| (*f, field(f, h, &r, &hash, &dir)))
            .collect::<Vec<_>>()
    });
    if args.get("format").and_then(Value::as_str) == Some("table") {
        let mut table = vec![json!(fields)];
        table.extend(rows.map(|row| {
            Value::Array(
                row.into_iter()
                    .map(|(_, v)| v.unwrap_or(Value::Null))
                    .collect(),
            )
        }));
        return json!({ "torrents": table });
    }
    let list: Vec<Value> = rows
        .map(|row| {
            Value::Object(
                row.into_iter()
                    .filter_map(|(k, v)| Some((k.to_owned(), v?)))
                    .collect(),
            )
        })
        .collect();
    json!({ "torrents": list })
}

/// The protocol's status codes.
fn status(s: St) -> u8 {
    match s {
        St::Paused | St::Error => 0,
        St::Checking => 2,
        St::Metadata | St::Downloading => 4,
        St::Seeding => 6,
    }
}

/// One `torrent-get` field; `None` for fields Torrie doesn't track (left out). No `h` while fetching metadata.
fn field(name: &str, h: Option<&ManagedTorrent>, r: &Row, hash: &str, dir: &Path) -> Option<Value> {
    let left = r.total.saturating_sub(r.done);
    Some(match name {
        "id" => json!(wire_id(r.id)),
        "name" => json!(r.name),
        "hashString" => json!(hash),
        "status" => json!(status(r.state)),
        "error" => json!(if r.error.is_some() { 3 } else { 0 }),
        "errorString" => json!(r.error.clone().unwrap_or_default()),
        "percentDone" | "percentComplete" => json!(r.fraction()),
        "metadataPercentComplete" => json!(if h.is_some() { 1.0 } else { 0.0 }),
        "rateDownload" => json!(r.down_bps as u64),
        "rateUpload" => json!(r.up_bps as u64),
        "totalSize" | "sizeWhenDone" => json!(r.total),
        "leftUntilDone" => json!(left),
        "haveValid" | "downloadedEver" => json!(r.done),
        "haveUnchecked" | "corruptEver" | "queuePosition" | "bandwidthPriority" => json!(0),
        "uploadedEver" => json!(r.uploaded),
        "uploadRatio" => json!(r.ratio()),
        "eta" => json!(rpc::eta(r).map_or(-1, |s| s as i64)),
        "downloadDir" => json!(dir),
        "peersConnected" => json!(r.peers_live),
        "isFinished" => json!(r.state == St::Paused && r.finished()),
        "isStalled" => json!(false),
        "labels" => json!([]),
        "magnetLink" => json!(format!(
            "magnet:?xt=urn:btih:{}&dn={}",
            hash,
            url::form_urlencoded::byte_serialize(r.name.as_bytes()).collect::<String>()
        )),
        "files" | "fileStats" | "wanted" | "priorities" | "trackers" if h.is_none() => json!([]),
        "files" | "fileStats" | "wanted" | "priorities" => {
            let h = h?;
            let progress = h.stats().file_progress;
            let only = h.only_files();
            // Relative to downloadDir: "name" for a single file, "Name/sub/file" for a folder.
            let (out, dir) = (h.output_folder(), torrent_dir(h.output_folder(), &r.name));
            let files: Vec<(String, u64, u64, bool)> = h
                .with_metadata(|m| {
                    m.file_infos
                        .iter()
                        .enumerate()
                        .map(|(i, f)| {
                            let path = out.join(&f.relative_filename);
                            let name = path.strip_prefix(&dir).unwrap_or(&f.relative_filename);
                            let wanted =
                                !f.attrs.padding && only.as_ref().is_none_or(|o| o.contains(&i));
                            (
                                name.to_string_lossy().into_owned(),
                                f.len,
                                progress.get(i).copied().unwrap_or(0),
                                wanted,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            match name {
                "files" => json!(files.iter().map(|(n, len, done, _)| json!({"name": n, "length": len, "bytesCompleted": done})).collect::<Vec<_>>()),
                "fileStats" => json!(files.iter().map(|(_, _, done, w)| json!({"bytesCompleted": done, "wanted": w, "priority": 0})).collect::<Vec<_>>()),
                "wanted" => json!(files.iter().map(|f| f.3).collect::<Vec<_>>()),
                _ => json!(vec![0; files.len()]),
            }
        }
        "trackers" => {
            json!(h?
            .shared()
            .trackers
            .iter()
            .enumerate()
            .map(|(i, u)| json!({"id": i, "announce": u.to_string(), "scrape": "", "tier": 0}))
            .collect::<Vec<_>>())
        }
        _ => return None,
    })
}

fn stats(e: &Engine) -> Value {
    let rows: Vec<Row> = select(e, None)
        .iter()
        .map(|(id, h)| e.row(*id, h))
        .collect();
    let sum = |f: fn(&Row) -> f64| rows.iter().map(f).sum::<f64>() as u64;
    let totals = json!({
        "uploadedBytes": sum(|r| r.uploaded as f64),
        "downloadedBytes": sum(|r| r.done as f64),
        "filesAdded": 0, "sessionCount": 1, "secondsActive": 0,
    });
    json!({
        "torrentCount": rows.len(),
        "activeTorrentCount": rows.iter().filter(|r| r.down_bps >= 1.0 || r.up_bps >= 1.0).count(),
        "pausedTorrentCount": rows.iter().filter(|r| r.state == St::Paused).count(),
        "downloadSpeed": sum(|r| r.down_bps),
        "uploadSpeed": sum(|r| r.up_bps),
        "cumulative-stats": totals,
        "current-stats": totals,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_set_maps_and_clamps() {
        let mut s = Settings::default();
        let args = json!({"alt-speed-down": 0, "alt-speed-enabled": true, "alt-speed-time-day": 0xff, "speed-limit-up": 7, "bogus": 1});
        session_set(&mut s, args.as_object().unwrap());
        s.sanitize();
        assert_eq!(
            (
                s.alt_speed_down_kbps,
                s.alt_speed_enabled,
                s.alt_speed_days,
                s.speed_limit_up_kbps
            ),
            (1, true, 0x7f, 7)
        );
    }

    #[test]
    fn pending_ids_fit_in_i32() {
        assert_eq!(wire_id(7), 7);
        assert_eq!(wire_id(usize::MAX), i32::MAX as usize);
        assert_eq!(wire_id(usize::MAX - 3), i32::MAX as usize - 3);
    }

    #[test]
    fn fields_filter() {
        let v = only(json!({"a": 1, "b": 2}), Some(&json!(["b"])));
        assert_eq!(v, json!({"b": 2}));
        assert_eq!(session_id().len(), 32);
        assert_ne!(session_id(), session_id());
    }
}
