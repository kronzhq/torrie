//! Transfer snapshot and display strings (decimal units, like Finder).

use std::{net::SocketAddr, sync::LazyLock};

use humansize::{DECIMAL, FormatSizeOptions, format_size};
use librqbit::{ManagedTorrent, TorrentStatsState};
use maxminddb::{Reader, geoip2};

/// DB-IP Country Lite (CC BY 4.0), refreshed by re-downloading the file.
static GEO: LazyLock<Option<Reader<&'static [u8]>>> = LazyLock::new(|| {
    Reader::from_source(&include_bytes!("../assets/geo/dbip-country-lite.mmdb")[..]).ok()
});

/// Flag emoji for a peer's `ip:port`; empty when the country is unknown.
pub fn peer_flag(addr: &str) -> String {
    let Ok(sock) = addr.parse::<SocketAddr>() else {
        return String::new();
    };
    let lookup = || {
        GEO.as_ref()?
            .lookup(sock.ip().to_canonical())
            .ok()?
            .decode::<geoip2::Country>()
            .ok()??
            .country
            .iso_code
    };
    lookup()
        .filter(|c| c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()))
        .map_or_else(String::new, |c| {
            // Two regional indicator letters render as the flag.
            c.bytes()
                .filter_map(|b| char::from_u32(0x1F1E6 + (b - b'A') as u32))
                .collect()
        })
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum State {
    Metadata,
    Checking,
    Downloading,
    Seeding,
    Paused,
    Error,
}

/// One transfer, snapshotted from librqbit once a second.
#[derive(Clone, Debug)]
pub struct Row {
    pub id: usize,
    pub name: String,
    pub state: State,
    pub error: Option<String>,
    pub done: u64,
    pub total: u64,
    pub uploaded: u64,
    pub down_bps: f64,
    pub up_bps: f64,
    pub peers_live: u32,
    pub peers_seen: u32,
    /// Automatic retries so far (errors, magnet lookups, stall kicks), and seconds until the next one; None
    /// once an error's retries have run out.
    pub tries: u32,
    pub retry_in: Option<u64>,
    /// Idle now, but had traffic moments ago; still counts as active.
    pub recent: bool,
}

impl Row {
    pub fn new(id: usize, h: &ManagedTorrent) -> Self {
        let st = h.stats();
        let live = st.live.as_ref();
        let state = match st.state {
            TorrentStatsState::Initializing { .. } => State::Checking,
            TorrentStatsState::Paused => State::Paused,
            TorrentStatsState::Error => State::Error,
            TorrentStatsState::Live if st.finished => State::Seeding,
            TorrentStatsState::Live => State::Downloading,
        };
        const MIB: f64 = 1024.0 * 1024.0; // librqbit reports MiB/s
        Self {
            id,
            name: h.name().unwrap_or_else(|| h.info_hash().as_string()),
            state,
            error: st.error.clone(),
            done: st.progress_bytes,
            total: st.total_bytes,
            uploaded: crate::engine::uploaded(&h.info_hash().as_string(), st.uploaded_bytes),
            down_bps: live.map_or(0.0, |l| l.download_speed.mbps * MIB),
            up_bps: live.map_or(0.0, |l| l.upload_speed.mbps * MIB),
            peers_live: live.map_or(0, |l| l.snapshot.peer_stats.live),
            peers_seen: live.map_or(0, |l| l.snapshot.peer_stats.seen),
            tries: 0,
            retry_in: None,
            recent: false,
        }
    }

    /// A magnet still waiting for its metadata (or a torrent file being added).
    pub fn pending(id: usize, name: &str) -> Self {
        Self {
            id,
            name: name.to_owned(),
            state: State::Metadata,
            error: None,
            done: 0,
            total: 0,
            uploaded: 0,
            down_bps: 0.0,
            up_bps: 0.0,
            peers_live: 0,
            peers_seen: 0,
            tries: 0,
            retry_in: None,
            recent: false,
        }
    }

    pub fn finished(&self) -> bool {
        self.total > 0 && self.done >= self.total
    }

    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.done as f64 / self.total as f64) as f32
        }
    }

    pub fn ratio(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.uploaded as f64 / self.total as f64
        }
    }

    /// Paused, or errored: librqbit restarts an errored torrent (re-checking its data) on resume.
    pub fn resumable(&self) -> bool {
        matches!(self.state, State::Paused | State::Error)
    }

    pub fn active(&self) -> bool {
        matches!(self.state, State::Metadata | State::Checking)
            || self.down_bps >= 1.0
            || self.up_bps >= 1.0
            || self.recent
    }
}

fn size() -> FormatSizeOptions {
    FormatSizeOptions::from(DECIMAL).decimal_places(1)
}

pub fn fmt_bytes(b: u64) -> String {
    format_size(b, size())
}

pub fn fmt_speed(bps: f64) -> String {
    format_size(bps as u64, size().suffix("/s"))
}

pub fn fmt_eta(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60);
    match secs {
        0..60 => format!("{secs} sec"),
        60..3600 => format!("{m} min"),
        3600..86400 => format!("{h} hr {m} min"),
        _ => format!("{d} days {h} hr"),
    }
}

/// Truncates instead of rounding so a transfer never shows 100% before it's done.
pub fn pct(fraction: f32) -> String {
    format!("{:.2}%", (fraction as f64 * 10000.0).floor() / 100.0)
}

pub fn eta(r: &Row) -> Option<String> {
    (r.state == State::Downloading && r.down_bps >= 1.0 && !r.finished())
        .then(|| fmt_eta(((r.total - r.done) as f64 / r.down_bps) as u64))
}

pub fn progress_line(r: &Row) -> String {
    if r.state == State::Metadata {
        return "Size unknown".into();
    }
    if r.finished() {
        return format!(
            "{}, uploaded {} (Ratio: {:.2})",
            fmt_bytes(r.total),
            fmt_bytes(r.uploaded),
            r.ratio()
        );
    }
    let mut s = format!(
        "{} of {} ({})",
        fmt_bytes(r.done),
        fmt_bytes(r.total),
        pct(r.fraction())
    );
    if let Some(eta) = eta(r) {
        s += &format!(" — {eta} remaining");
    }
    s
}

/// Short state label for the list; speeds are shown separately.
pub fn state_label(r: &Row) -> String {
    let base = match r.state {
        State::Error => format!("Error: {}", r.error.as_deref().unwrap_or("unknown")),
        State::Metadata => "Fetching metadata from peers".into(),
        State::Checking => "Checking existing data".into(),
        State::Paused => "Paused".into(),
        State::Downloading if r.peers_live == 0 => {
            format!("Looking for peers ({} known)", r.peers_seen)
        }
        State::Downloading => format!(
            "Downloading from {} of {} peers",
            r.peers_live, r.peers_seen
        ),
        State::Seeding => format!("Seeding to {} of {} peers", r.peers_live, r.peers_seen),
    };
    let next = r.tries + 1;
    match (r.state, r.retry_in) {
        (State::Error, None) if r.tries > 0 => {
            format!("{base} · gave up after {} retries", r.tries)
        }
        (State::Error, Some(s)) => format!("{base} · retry {next} in {}", fmt_eta(s)),
        (State::Metadata, Some(s)) => format!("{base} · retry {next} in {}", fmt_eta(s)),
        (State::Downloading, Some(s)) => {
            format!("{base} · stalled, retry {next} in {}", fmt_eta(s))
        }
        _ => base,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(state: State, done: u64, total: u64) -> Row {
        Row {
            id: 0,
            name: "ubuntu.iso".into(),
            state,
            error: None,
            done,
            total,
            uploaded: 0,
            down_bps: 0.0,
            up_bps: 0.0,
            peers_live: 0,
            peers_seen: 0,
            tries: 0,
            retry_in: None,
            recent: false,
        }
    }

    #[test]
    fn units() {
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1_500), "1.5 kB");
        assert_eq!(fmt_bytes(4_700_000_000), "4.7 GB");
        assert_eq!(fmt_speed(0.0), "0 B/s");
        assert_eq!(fmt_speed(1_250_000.0), "1.2 MB/s");
        assert_eq!(fmt_eta(42), "42 sec");
        assert_eq!(fmt_eta(125), "2 min");
        assert_eq!(fmt_eta(3 * 3600 + 20 * 60), "3 hr 20 min");
        assert_eq!(fmt_eta(2 * 86400 + 3600), "2 days 1 hr");
    }

    #[test]
    fn lines() {
        let mut r = row(State::Downloading, 999_999, 1_000_000);
        r.down_bps = 1.0;
        r.peers_live = 3;
        r.peers_seen = 40;
        // Never rounds up to 100% while incomplete.
        assert_eq!(
            progress_line(&r),
            "1000.0 kB of 1 MB (99.99%) — 1 sec remaining"
        );
        assert_eq!(state_label(&r), "Downloading from 3 of 40 peers");
        assert!(r.active());

        let mut r = row(State::Seeding, 2_000, 2_000);
        r.uploaded = 3_000;
        assert_eq!(progress_line(&r), "2 kB, uploaded 3 kB (Ratio: 1.50)");
        assert_eq!(eta(&r), None);

        assert_eq!(
            progress_line(&row(State::Paused, 0, 0)),
            "0 B of 0 B (0.00%)"
        );
        assert_eq!(state_label(&row(State::Paused, 0, 0)), "Paused");
        assert_eq!(
            state_label(&row(State::Downloading, 0, 10)),
            "Looking for peers (0 known)"
        );
        assert!(row(State::Error, 0, 10).resumable());
        let mut r = row(State::Error, 0, 10);
        r.error = Some("disk full".into());
        assert_eq!(state_label(&r), "Error: disk full");
        r.retry_in = Some(90);
        assert_eq!(state_label(&r), "Error: disk full · retry 1 in 1 min");
        (r.tries, r.retry_in) = (6, None);
        assert_eq!(
            state_label(&r),
            "Error: disk full · gave up after 6 retries"
        );
        let r = Row {
            tries: 2,
            retry_in: Some(240),
            ..Row::pending(1, "x")
        };
        assert_eq!(
            state_label(&r),
            "Fetching metadata from peers · retry 3 in 4 min"
        );
        let r = Row {
            tries: 1,
            retry_in: Some(30),
            ..row(State::Downloading, 0, 10)
        };
        assert_eq!(
            state_label(&r),
            "Looking for peers (0 known) · stalled, retry 2 in 30 sec"
        );

        let r = Row::pending(usize::MAX, "ubuntu.iso");
        assert_eq!(progress_line(&r), "Size unknown");
        assert!(r.active());
    }

    #[test]
    fn peer_flags() {
        assert_eq!(peer_flag("8.8.8.8:6881"), "🇺🇸");
        assert_eq!(peer_flag("[::ffff:8.8.8.8]:6881"), "🇺🇸");
        assert_eq!(peer_flag("192.168.1.2:6881"), "");
        assert_eq!(peer_flag("garbage"), "");
    }
}
