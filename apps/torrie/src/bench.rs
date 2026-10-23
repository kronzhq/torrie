//! Timings for the work done on the UI thread, on an offline session of synthetic torrents. Not run by default:
//! `cargo test --release bench -- --ignored --nocapture`. `BENCH_TORRENTS` sets the count (default 500).

use std::{hint::black_box, time::Instant};

use librqbit::{AddTorrent, AddTorrentOptions, TorrentStatsState};

use crate::{engine::Engine, format, rpc};

/// Average milliseconds per run of `f`.
fn time<T>(name: &str, runs: u32, mut f: impl FnMut() -> T) -> f64 {
    let at = Instant::now();
    for _ in 0..runs {
        black_box(f());
    }
    let ms = at.elapsed().as_secs_f64() * 1000. / runs as f64;
    println!("{name:<44} {ms:>9.3} ms");
    ms
}

/// A multi-file .torrent of `files` files of `file_len` bytes, in 16 KiB pieces.
fn torrent(name: &str, files: usize, file_len: u64) -> Vec<u8> {
    const PIECE: u64 = 16384;
    let pieces = (files as u64 * file_len).div_ceil(PIECE) as usize;
    let mut t = b"d4:infod5:filesl".to_vec();
    for i in 0..files {
        let f = format!("file {i:05}.bin");
        t.extend(format!("d6:lengthi{file_len}e4:pathl{}:{f}ee", f.len()).bytes());
    }
    t.extend(
        format!(
            "e4:name{}:{name}12:piece lengthi{PIECE}e6:pieces{}:",
            name.len(),
            pieces * 20
        )
        .bytes(),
    );
    t.extend(std::iter::repeat_n(7u8, pieces * 20));
    t.extend(b"ee");
    t
}

#[test]
#[ignore]
fn bench() {
    let n: usize = std::env::var("BENCH_TORRENTS").map_or(500, |s| s.parse().unwrap());
    let dir = std::env::temp_dir().join(format!("torrie-bench-{}", std::process::id()));
    let e = Engine::offline(dir.clone());
    let paused = || AddTorrentOptions {
        paused: true,
        ..Default::default()
    };
    // One big torrent for the inspector: 5,000 files, 20,000 pieces.
    let big = std::iter::once(torrent("big", 5_000, 65_536));
    let small = (0..n - 1).map(|i| torrent(&format!("torrent {i}"), 20, 1 << 20));
    e.runtime().block_on(async {
        for t in big.chain(small) {
            e.session
                .add_torrent(AddTorrent::from_bytes(t), Some(paused()))
                .await
                .unwrap();
        }
        while e.session.with_torrents(|it| {
            it.map(|(_, h)| h.stats().state)
                .any(|s| matches!(s, TorrentStatsState::Initializing { .. }))
        }) {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });
    let big = e
        .session
        .with_torrents(|it| it.map(|(id, _)| id).min())
        .unwrap();
    println!("{n} torrents\n");

    println!("-- every tick (each second), main window and popover --");
    let rows = e.rows();
    time(&format!("Engine::rows ({} rows)", rows.len()), 20, || {
        e.rows()
    });
    let d = e.details(big).unwrap();
    let what = format!("{} files, {} pieces", d.files.len(), d.pieces);
    time(&format!("Engine::details ({what})"), 20, || e.details(big));
    let peers: Vec<String> = (0..200u32)
        .map(|i| format!("{}.{}.{}.1:6881", 1 + i % 223, i * 7 % 256, i * 13 % 256))
        .collect();
    time("peer_flag, 200 peers", 20, || {
        peers
            .iter()
            .map(|p| format::peer_flag(p))
            .collect::<Vec<_>>()
    });

    println!("-- every frame, for each row on screen --");
    time("progress_line + state_label, all rows", 20, || {
        rows.iter()
            .map(|r| (format::progress_line(r), format::state_label(r)))
            .collect::<Vec<_>>()
    });

    println!("-- web remote, each poll --");
    time("rpc torrent json, all torrents", 20, || {
        e.session.with_torrents(|it| {
            it.map(|(id, h)| rpc::torrent(&e, id, h))
                .collect::<Vec<_>>()
        })
    });

    e.stop();
    _ = std::fs::remove_dir_all(&dir);
}
