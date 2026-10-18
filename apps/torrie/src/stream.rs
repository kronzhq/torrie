//! Watch while downloading: a file served over HTTP from librqbit's `FileStream`, which downloads the pieces
//! just ahead of wherever the player reads (and follows it when it seeks) while the rest stay rarest-first.
//! Only listens on 127.0.0.1, on a random port, behind a random token in the path, so neither web pages nor
//! other apps can guess the URL. Started on the first Play and kept for the session.

use std::{
    io::SeekFrom,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    sync::{Arc, OnceLock},
};

use anyhow::Context as _;
use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use librqbit::{Api, Session, api::TorrentIdOrHash};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use crate::engine::Engine;

/// File types a player can open; others get no Play button.
const MEDIA: [&str; 14] = [
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "ts", "mp3", "m4a", "aac", "flac", "wav", "ogg",
    "opus",
];

static SERVER: OnceLock<(SocketAddr, String)> = OnceLock::new();

pub fn playable(name: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|e| MEDIA.iter().any(|m| e.eq_ignore_ascii_case(m)))
}

/// URL for one file of a torrent, starting the server if needed. The file name at the end is only for the
/// player's title bar and format guess.
pub fn url(engine: &Engine, id: usize, file: usize, name: &str) -> anyhow::Result<String> {
    let (addr, token) = match SERVER.get() {
        Some(s) => s,
        None => {
            let listener =
                TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("Streaming: no free port")?;
            listener.set_nonblocking(true)?;
            let addr = listener.local_addr()?;
            let token = crate::rpc::random(16, 4);
            let app = Router::new()
                .route(&format!("/{token}/{{id}}/{{file}}/{{*name}}"), get(serve))
                .with_state(engine.session.clone());
            engine.run(async move {
                axum::serve(tokio::net::TcpListener::from_std(listener)?, app)
                    .await
                    .context("Streaming")
            });
            SERVER.get_or_init(|| (addr, token))
        }
    };
    let name =
        url::form_urlencoded::byte_serialize(name.rsplit('/').next().unwrap_or(name).as_bytes())
            .collect::<String>();
    Ok(format!("http://{addr}/{token}/{id}/{file}/{name}"))
}

/// One HTTP GET, honouring a single `Range: bytes=start-[end]` so players can seek.
async fn serve(
    State(session): State<Arc<Session>>,
    Path((id, file, _)): Path<(usize, usize, String)>,
    headers: HeaderMap,
) -> Response {
    let api = Api::new(session, None);
    let mut stream = match api.api_stream(TorrentIdOrHash::Id(id), file).await {
        Ok(s) => s,
        Err(e) => return (StatusCode::NOT_FOUND, e.to_string()).into_response(),
    };
    let len = stream.len();
    let mime = api
        .torrent_file_mime_type(TorrentIdOrHash::Id(id), file)
        .unwrap_or("application/octet-stream");
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok()?.strip_prefix("bytes=")?.split_once('-'))
        .and_then(|(a, b)| {
            Some((
                a.parse::<u64>().ok()?,
                if b.is_empty() {
                    len
                } else {
                    b.parse::<u64>().ok()?.saturating_add(1)
                },
            ))
        });
    let base = [
        (header::CONTENT_TYPE, mime.to_owned()),
        (header::ACCEPT_RANGES, "bytes".into()),
    ];
    let Some((start, end)) = range else {
        let body = Body::from_stream(tokio_util::io::ReaderStream::with_capacity(stream, 1 << 16));
        return (base, [(header::CONTENT_LENGTH, len.to_string())], body).into_response();
    };
    if start >= len || end <= start || end > len {
        return (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{len}"))],
        )
            .into_response();
    }
    if let Err(e) = stream.seek(SeekFrom::Start(start)).await {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }
    let body = Body::from_stream(tokio_util::io::ReaderStream::with_capacity(
        stream.take(end - start),
        1 << 16,
    ));
    let part = [
        (header::CONTENT_LENGTH, (end - start).to_string()),
        (
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{len}", end - 1),
        ),
    ];
    (StatusCode::PARTIAL_CONTENT, base, part, body).into_response()
}

/// Opens in IINA or VLC when installed (they play nearly everything), else the default app for the URL.
pub async fn open(url: String) -> anyhow::Result<()> {
    for app in ["IINA", "VLC"] {
        if tokio::process::Command::new("open")
            .args(["-a", app, &url])
            .status()
            .await?
            .success()
        {
            return Ok(());
        }
    }
    anyhow::ensure!(
        tokio::process::Command::new("open")
            .arg(&url)
            .status()
            .await?
            .success(),
        "Couldn't open a player"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::{Router, body::to_bytes, http::Request, routing::get};
    use librqbit::{
        AddTorrent, AddTorrentOptions, Session, SessionOptions, spawn_utils::BlockingSpawner,
    };
    use tower::ServiceExt;

    #[test]
    fn playable() {
        assert!(super::playable("Show/S01E01.MKV"));
        assert!(!super::playable("readme.txt"));
        assert!(!super::playable("mp4"));
    }

    /// A finished torrent served whole, by range, open-ended range, and out of range.
    #[tokio::test(flavor = "multi_thread")]
    async fn ranges() {
        let dir = std::env::temp_dir().join(format!("torrie-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(dir.join("clip.mp4"), &data).unwrap();
        let torrent = librqbit::create_torrent(
            &dir.join("clip.mp4"),
            Default::default(),
            &BlockingSpawner::new(1),
        )
        .await
        .unwrap();
        let opts = SessionOptions {
            dht: None,
            disable_trackers: true,
            listen: None,
            persistence: None,
            ..Default::default()
        };
        let session = Session::new_with_opts(dir.clone(), opts).await.unwrap();
        let add = AddTorrentOptions {
            overwrite: true,
            ..Default::default()
        };
        let h = session
            .add_torrent(
                AddTorrent::from_bytes(torrent.as_bytes().unwrap()),
                Some(add),
            )
            .await
            .unwrap();
        h.into_handle()
            .unwrap()
            .wait_until_initialized()
            .await
            .unwrap();
        let app = Router::new()
            .route("/{id}/{file}/{*name}", get(super::serve))
            .with_state(session);

        let get = |range: Option<&str>| {
            let mut req = Request::get("/0/0/clip.mp4");
            if let Some(r) = range {
                req = req.header("range", r);
            }
            let app = app.clone();
            async move {
                let res = app
                    .oneshot(req.body(axum::body::Body::empty()).unwrap())
                    .await
                    .unwrap();
                let status = res.status().as_u16();
                let cr = res
                    .headers()
                    .get("content-range")
                    .map(|v| v.to_str().unwrap().to_owned());
                (
                    status,
                    cr,
                    to_bytes(res.into_body(), usize::MAX).await.unwrap(),
                )
            }
        };
        let (st, _, body) = get(None).await;
        assert_eq!((st, &body[..]), (200, &data[..]));
        let (st, cr, body) = get(Some("bytes=10-19")).await;
        assert_eq!(
            (st, cr.as_deref(), &body[..]),
            (206, Some("bytes 10-19/100000"), &data[10..20])
        );
        let (st, cr, body) = get(Some("bytes=99990-")).await;
        assert_eq!(
            (st, cr.as_deref(), &body[..]),
            (206, Some("bytes 99990-99999/100000"), &data[99990..])
        );
        assert_eq!(get(Some("bytes=100000-")).await.0, 416);
        std::fs::remove_dir_all(&dir).ok();
    }
}
