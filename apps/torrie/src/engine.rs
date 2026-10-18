//! Thin wrapper over a librqbit `Session`, and Torrie's settings.

use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    future::Future,
    net::{Ipv6Addr, SocketAddr},
    num::NonZeroU32,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::Context as _;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, Api, ListOnlyResponse, ListenerMode,
    ListenerOptions, Magnet, ManagedTorrent, Session, SessionOptions, SessionPersistenceConfig,
    TorrentStatsState, api::TorrentIdOrHash, generate_azereus_style, limits::LimitsConfig,
};
use serde::{Deserialize, Serialize};
use tokio::{runtime::Runtime, sync::Notify};

use crate::{
    format::{Row, State},
    plugins,
};

// Every retry waits twice as long as the one before (see `backoff`), up to its cap.
/// A magnet lookup that finds nothing is restarted (fresh tracker announces and DHT queries) after 1, 2, 4…
/// minutes, then every `METADATA_CAP`, until it succeeds or is removed.
const METADATA_RETRY: Duration = Duration::from_secs(60);
const METADATA_CAP: Duration = Duration::from_secs(16 * 60);
/// A download that gains no bytes is paused and resumed to rediscover peers after 3, 6, 12… minutes, then every
/// `STALL_CAP`; any progress starts it over. Torrents with no seeders at all thus settle at one kick an hour.
const STALL: Duration = Duration::from_secs(180);
const STALL_CAP: Duration = Duration::from_secs(3600);
/// Stalled downloads kicked per 15 s pass at most; kicks hold the lock that pausing by hand waits on.
const KICKS_PER_PASS: usize = 8;
/// An errored torrent is retried after 1, 2, 4… minutes, at most `RETRY_MAX` times, then left for the user.
const RETRY_FIRST: Duration = Duration::from_secs(60);
const RETRY_MAX: u32 = 6;
/// A retried torrent that runs this long without failing counts as recovered, and its tries reset.
const RETRY_HEALTHY: Duration = Duration::from_secs(600);
/// A transfer stays "active" this long after its last traffic, so bursty seeds don't flicker in and out of lists.
const LINGER: Duration = Duration::from_secs(10);
/// Speeds are averaged over this long. librqbit's own estimate spans only 0.5 s (5 samples, 100 ms apart),
/// so any brief lull between peers' chunks reads as 0 even on a steady connection.
const SPEED_WINDOW: Duration = Duration::from_secs(5);

/// `~/Library/Application Support/Torrie/settings.json`.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub download_dir: PathBuf,
    pub peer_port: u16,
    pub port_forwarding: bool,
    pub peer_limit_per_torrent: usize,
    /// KB/s (1 KB = 1000 bytes).
    pub speed_limit_down_enabled: bool,
    pub speed_limit_down_kbps: u32,
    pub speed_limit_up_enabled: bool,
    pub speed_limit_up_kbps: u32,
    /// Stop seeding once uploaded / size reaches this.
    pub ratio_limit_enabled: bool,
    pub ratio_limit: f64,
    pub appearance: Appearance,
    /// Themes used in light and dark mode: a built-in or `themes/<name>.json`.
    pub light_theme: String,
    pub dark_theme: String,
    /// `#rrggbb` overriding the theme's accent.
    pub accent: Option<String>,
    /// Font family overriding the theme's.
    pub font: Option<String>,
    pub animations: bool,
    /// A menu bar item; also keeps Torrie running after its window closes.
    pub menu_bar: bool,
    pub open_at_login: bool,
    /// Alternative speed limits: replace the limits above while on.
    pub alt_speed_enabled: bool,
    pub alt_speed_down_kbps: u32,
    pub alt_speed_up_kbps: u32,
    /// Switches alternative limits on at `alt_speed_begin` and off at `alt_speed_end` (minutes after
    /// midnight) on the days in `alt_speed_days` (Sunday = bit 0 … Saturday = bit 6).
    pub alt_speed_time_enabled: bool,
    pub alt_speed_begin: u16,
    pub alt_speed_end: u16,
    pub alt_speed_days: u8,
    /// Remote control: Torrie's API (scripts, plugins) and the Transmission protocol (other apps).
    pub rpc_enabled: bool,
    pub rpc_port: u16,
    /// Listen on every network, not just this Mac; anything not on this Mac then needs `rpc_password`.
    pub rpc_lan: bool,
    /// Basic auth password (any username). Generated; empty locks out everything not on this Mac.
    pub rpc_password: String,
    /// Plugin folder names allowed to run; new plugins stay off until switched on.
    pub plugins_enabled: Vec<String>,
    /// Values of plugins' settings, by plugin id; unset ones take the manifest's default.
    pub plugin_settings: BTreeMap<String, serde_json::Map<String, serde_json::Value>>,
    /// Play buttons on media files, streaming them to a player while they download (`stream.rs`).
    pub streaming: bool,
    /// Looks for a new Torrie at launch and daily (`update.rs`).
    pub check_updates: bool,
    /// Stops the Mac idle-sleeping while anything is downloading.
    pub keep_awake: bool,
}

/// Light or dark mode; System follows macOS.
#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Appearance {
    System,
    Light,
    Dark,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            download_dir: home().join("Downloads"),
            peer_port: 51413,
            port_forwarding: true,
            // librqbit's default; 50 caps well-seeded torrents.
            peer_limit_per_torrent: 128,
            speed_limit_down_enabled: false,
            speed_limit_down_kbps: 100,
            speed_limit_up_enabled: false,
            speed_limit_up_kbps: 100,
            ratio_limit_enabled: false,
            ratio_limit: 2.0,
            appearance: Appearance::System,
            light_theme: "Light".into(),
            dark_theme: "Dark".into(),
            accent: None,
            font: None,
            animations: true,
            menu_bar: true,
            open_at_login: false,
            alt_speed_enabled: false,
            alt_speed_down_kbps: 50,
            alt_speed_up_kbps: 50,
            alt_speed_time_enabled: false,
            alt_speed_begin: 9 * 60,
            alt_speed_end: 17 * 60,
            alt_speed_days: 0b111_1111,
            rpc_enabled: false,
            rpc_port: 9091,
            rpc_lan: false,
            rpc_password: crate::rpc::new_password(),
            plugins_enabled: vec![crate::plugins::BUNDLED.into()],
            plugin_settings: BTreeMap::new(),
            streaming: false,
            check_updates: true,
            keep_awake: true,
        }
    }
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

pub fn config_dir() -> PathBuf {
    home().join("Library/Application Support/Torrie")
}

impl Settings {
    fn path() -> PathBuf {
        config_dir().join("settings.json")
    }

    fn load() -> anyhow::Result<Self> {
        let path = Self::path();
        match std::fs::read(&path) {
            Ok(bytes) => {
                let mut s: Self = serde_json::from_slice(&bytes)
                    .with_context(|| format!("invalid {}", path.display()))?;
                s.sanitize();
                Ok(s)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let s = Self::default();
                s.save()?;
                Ok(s)
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    fn save(&self) -> anyhow::Result<()> {
        std::fs::create_dir_all(config_dir())?;
        std::fs::write(Self::path(), serde_json::to_vec_pretty(self)?)?;
        // It holds the remote control password.
        std::fs::set_permissions(
            Self::path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )?;
        Ok(())
    }

    /// What only applies when Torrie starts.
    fn network(&self) -> (u16, bool, bool, u16, bool) {
        (
            self.peer_port,
            self.port_forwarding,
            self.rpc_enabled,
            self.rpc_port,
            self.rpc_lan,
        )
    }

    /// Clamps every value to a sane range; all writers (Settings window, RPC, hand edits) go through it.
    pub fn sanitize(&mut self) {
        let kbps = |k: &mut u32| *k = (*k).clamp(1, 1_000_000);
        for k in [
            &mut self.speed_limit_down_kbps,
            &mut self.speed_limit_up_kbps,
            &mut self.alt_speed_down_kbps,
            &mut self.alt_speed_up_kbps,
        ] {
            kbps(k);
        }
        self.peer_port = self.peer_port.max(1);
        self.rpc_port = self.rpc_port.max(1);
        self.peer_limit_per_torrent = self.peer_limit_per_torrent.clamp(1, 1000);
        self.ratio_limit = (self.ratio_limit.clamp(0.01, 100.0) * 100.0).round() / 100.0;
        self.alt_speed_begin = self.alt_speed_begin.min(24 * 60 - 1);
        self.alt_speed_end = self.alt_speed_end.min(24 * 60 - 1);
        self.alt_speed_days &= 0b111_1111;
    }

    fn limits(&self) -> LimitsConfig {
        let kbps = |on: bool, k: u32| {
            on.then(|| NonZeroU32::new(k.saturating_mul(1000)))
                .flatten()
        };
        let alt = self.alt_speed_enabled;
        LimitsConfig {
            download_bps: kbps(
                alt || self.speed_limit_down_enabled,
                if alt {
                    self.alt_speed_down_kbps
                } else {
                    self.speed_limit_down_kbps
                },
            ),
            upload_bps: kbps(
                alt || self.speed_limit_up_enabled,
                if alt {
                    self.alt_speed_up_kbps
                } else {
                    self.speed_limit_up_kbps
                },
            ),
        }
    }

    /// Whether the schedule wants alternative limits on at `minute` past midnight on weekday `wday` (0 = Sunday).
    /// A window that ends before it begins runs overnight; begin == end means all day.
    pub fn alt_scheduled(&self, minute: u16, wday: u32) -> bool {
        let (b, e) = (self.alt_speed_begin, self.alt_speed_end);
        let in_window = match b.cmp(&e) {
            std::cmp::Ordering::Less => (b..e).contains(&minute),
            std::cmp::Ordering::Greater => minute >= b || minute < e,
            std::cmp::Ordering::Equal => true,
        };
        in_window && self.alt_speed_days & (1 << wday) != 0
    }
}

pub struct FileRow {
    pub index: usize,
    pub name: String,
    pub len: u64,
    pub done: u64,
    pub wanted: bool,
}

pub struct PeerRow {
    pub addr: String,
    /// Country flag emoji, or empty.
    pub flag: String,
    pub client: String,
    pub kind: String,
    pub downloaded: u64,
    pub uploaded: u64,
}

/// Inspector data for one torrent. Only computed for the selected torrent.
pub struct Details {
    pub info_hash: String,
    pub folder: PathBuf,
    pub pieces: u32,
    pub piece_len: u32,
    pub files: Vec<FileRow>,
    pub peers: Vec<PeerRow>,
    pub trackers: Vec<String>,
    /// Which pieces are done; empty while the torrent is initializing.
    pub haves: Vec<bool>,
}

/// A torrent being added: magnets wait here until their metadata arrives.
pub struct Pending {
    /// Counts down from `usize::MAX` so it never collides with librqbit's ids.
    pub id: usize,
    pub name: String,
    /// Known up front for magnets; empty for http links and torrent files.
    pub hash: String,
    pub dir: PathBuf,
    task: tokio::task::AbortHandle,
    /// Lookups restarted so far, and when the current one gets restarted.
    tries: u32,
    next: Instant,
    /// Cuts the current wait short with a fresh lookup ("Retry Now").
    retry_now: Arc<Notify>,
}

/// A torrent whose metadata has arrived, waiting for the user to pick a folder and files before it starts.
pub struct Review {
    pub name: String,
    /// (file index, path inside the torrent, size); padding files left out.
    pub files: Vec<(usize, String, u64)>,
    pub wanted: HashSet<usize>,
    pub dir: PathBuf,
    pub paused: bool,
    /// The torrent's own subfolder, created inside `dir`.
    sub: PathBuf,
    meta: ListOnlyResponse,
}

impl Review {
    pub fn folder(&self) -> PathBuf {
        self.dir.join(&self.sub)
    }
}

/// The wait before retry number `n` (0-based): `first`, doubling each time, never more than `cap`.
fn backoff(first: Duration, n: u32, cap: Duration) -> Duration {
    first.saturating_mul(1 << n.min(20)).min(cap)
}

/// Automatic retry state for one torrent that has errored. Lives until the torrent recovers, is paused or removed,
/// or the user retries it by hand.
#[derive(Clone, Copy, Default)]
struct Retry {
    tries: u32,
    /// None once the tries ran out.
    next: Option<Instant>,
    /// Running since this moment without failing again.
    ok_since: Option<Instant>,
}

type Retries = Arc<Mutex<HashMap<usize, Retry>>>;
/// Per download: bytes last seen, since when, and kicks since it last made progress.
type Stalls = Arc<Mutex<HashMap<usize, (u64, Instant, u32)>>>;

/// Torrents mid-[`kick`], and the lock that keeps kicks and pauses by hand from interleaving.
#[derive(Default)]
struct Kicks {
    ids: Mutex<HashSet<usize>>,
    serial: tokio::sync::Mutex<()>,
}

pub struct Engine {
    rt: Runtime,
    pub session: Arc<Session>,
    /// Failures the window shows, oldest first, until dismissed (see [`Engine::report`]).
    pub errors: Arc<Mutex<Vec<String>>>,
    pub pending: Arc<Mutex<Vec<Pending>>>,
    reviews: Arc<Mutex<Vec<Review>>>,
    /// Ids of torrents the user added again from the window; it points them out.
    duplicates: Arc<Mutex<Vec<usize>>>,
    retries: Retries,
    stalls: Stalls,
    kicks: Arc<Kicks>,
    /// When each transfer last had traffic (see [`LINGER`]).
    busy: Mutex<HashMap<usize, Instant>>,
    /// Recent (when, downloaded, uploaded) byte counters per live transfer (see [`SPEED_WINDOW`]).
    speeds: Mutex<HashMap<usize, VecDeque<(Instant, u64, u64)>>>,
    /// Torrents being verified (id → name): briefly out of the session while they're re-added.
    verifying: Arc<Mutex<HashMap<usize, String>>>,
    /// Signalled whenever an operation finishes, so the UI refreshes now rather than on its next tick.
    changed: Arc<Notify>,
    pending_seq: AtomicUsize,
    next_id: Arc<Mutex<usize>>,
    /// The session's default output folder (the download folder at launch).
    session_dir: PathBuf,
    settings: Arc<Mutex<Settings>>,
    /// Port and RPC settings only apply when the session starts.
    started_with: (u16, bool, bool, u16, bool),
}

impl Engine {
    pub fn start() -> anyhow::Result<Self> {
        let settings = Settings::load()?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        // The configured port first; if it's taken, let the OS pick.
        let session = rt
            .block_on(Session::new_with_opts(
                settings.download_dir.clone(),
                options(&settings, settings.peer_port),
            ))
            .or_else(|_| {
                rt.block_on(Session::new_with_opts(
                    settings.download_dir.clone(),
                    options(&settings, 0),
                ))
            })
            .context("starting torrent session")?;
        let (stalls, kicks) = (Stalls::default(), Arc::new(Kicks::default()));
        rt.spawn(unstick(session.clone(), stalls.clone(), kicks.clone()));
        plugins::seed();
        load_uploaded();
        let started_with = settings.network();
        let session_dir = settings.download_dir.clone();
        let settings = Arc::new(Mutex::new(settings));
        let retries = Retries::default();
        rt.spawn(watch(
            session.clone(),
            settings.clone(),
            retries.clone(),
            kicks.clone(),
        ));
        Ok(Self {
            rt,
            session,
            errors: Default::default(),
            pending: Default::default(),
            reviews: Default::default(),
            duplicates: Default::default(),
            retries,
            stalls,
            kicks,
            busy: Default::default(),
            speeds: Default::default(),
            verifying: Default::default(),
            changed: Default::default(),
            pending_seq: Default::default(),
            next_id: Default::default(),
            session_dir,
            started_with,
            settings,
        })
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    /// Applies what can change live (speed limits, ratio, folder for new torrents) and saves to disk.
    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) {
        if let Err(e) = update(&self.session, &self.settings, f) {
            self.report(format!("saving settings: {e:#}"));
        }
    }

    /// Queues an error for the window; one already waiting isn't queued twice.
    pub fn report(&self, e: String) {
        report(&self.errors, e);
    }

    pub fn needs_restart(&self) -> bool {
        let s = self.settings.lock().unwrap();
        s.network() != self.started_with
    }

    pub fn listen_port(&self) -> Option<u16> {
        self.session.listen_addr().map(|a| a.port())
    }

    /// Resolves `true` as soon as an operation finishes, or `false` at `deadline`.
    pub fn changed_before(&self, deadline: Instant) -> tokio::task::JoinHandle<bool> {
        let changed = self.changed.clone();
        self.rt.spawn(async move {
            tokio::time::timeout_at(deadline.into(), changed.notified())
                .await
                .is_ok()
        })
    }

    /// For awaiting tokio work from the UI: `runtime().spawn(…)` gives a handle any executor can await.
    pub fn runtime(&self) -> &tokio::runtime::Handle {
        self.rt.handle()
    }

    /// Runs `fut` on the engine's runtime; an error shows in the window.
    pub fn run(
        &self,
        fut: impl Future<Output = anyhow::Result<()>> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        let (errors, changed) = (self.errors.clone(), self.changed.clone());
        self.rt.spawn(async move {
            if let Err(e) = fut.await {
                eprintln!("Torrie: {e:#}");
                report(&errors, format!("{e:#}"));
            }
            changed.notify_one();
        })
    }

    /// Magnets only show up in the session once their metadata arrives; `pending` lists them meanwhile.
    /// `dir` replaces the download folder for this torrent. With `review`, the torrent waits in
    /// [`Engine::take_reviews`] for [`Engine::confirm`] instead of starting.
    pub fn add(
        &self,
        add: AddTorrent<'static>,
        name: String,
        dir: Option<PathBuf>,
        paused: bool,
        review: bool,
    ) {
        let (session, pending, next_id) = (
            self.session.clone(),
            self.pending.clone(),
            self.next_id.clone(),
        );
        let (reviews, duplicates, label) =
            (self.reviews.clone(), self.duplicates.clone(), name.clone());
        let (s, session_dir) = (self.settings(), self.session_dir.clone());
        let dir = dir.unwrap_or_else(|| s.download_dir.clone());
        let magnet = match &add {
            AddTorrent::Url(u) => Magnet::parse(u).ok().and_then(|m| m.as_id20()),
            _ => None,
        };
        let (hash, pending_dir) = (
            magnet.map(|h| h.as_string()).unwrap_or_default(),
            dir.clone(),
        );
        let pending_id = usize::MAX - self.pending_seq.fetch_add(1, Ordering::Relaxed);
        let retry_now = Arc::new(Notify::new());
        let (retry, changed) = (retry_now.clone(), self.changed.clone());
        // Held across the spawn so the task can't finish and unlist itself before it's listed.
        let mut list = self.pending.lock().unwrap();
        let task = self.run(async move {
            let res = async {
                // Already here: say so (window adds only; scripts re-adding are fine) and skip the metadata fetch.
                let dup = |id: usize| {
                    if review {
                        duplicates.lock().unwrap().push(id);
                    }
                };
                if let Some(h) = magnet.and_then(|h| session.get(TorrentIdOrHash::Hash(h))) {
                    return Ok(dup(h.id()));
                }
                // Resolve metadata first: librqbit picks the per-torrent subfolder relative to the folder the
                // session started with, and we re-root it on the current download folder (it can change live).
                // librqbit can sit on a magnet forever once its first peers go quiet; start the lookup over.
                let mut tries = 0;
                let res = loop {
                    let add = match &add {
                        AddTorrent::Url(u) => AddTorrent::Url(u.clone()),
                        AddTorrent::TorrentFileBytes(b) => AddTorrent::TorrentFileBytes(b.clone()),
                    };
                    let list = AddTorrentOptions {
                        list_only: true,
                        ..Default::default()
                    };
                    let wait = backoff(METADATA_RETRY, tries, METADATA_CAP);
                    if let Some(p) = pending
                        .lock()
                        .unwrap()
                        .iter_mut()
                        .find(|p| p.id == pending_id)
                    {
                        (p.tries, p.next) = (tries, Instant::now() + wait);
                    }
                    tokio::select! {
                        res = session.add_torrent(add, Some(list)) => break res?,
                        _ = tokio::time::sleep(wait) => tries += 1,
                        // By hand: a fresh lookup, and the backoff starts over.
                        _ = retry.notified() => tries = 0,
                    }
                    changed.notify_one();
                };
                let AddTorrentResponse::ListOnly(meta) = res else {
                    return Ok(());
                };
                if let Some(h) = session.get(TorrentIdOrHash::Hash(meta.info_hash)) {
                    return Ok(dup(h.id()));
                }
                let sub = meta
                    .output_folder
                    .strip_prefix(&session_dir)
                    .unwrap_or(Path::new(""))
                    .to_owned();
                if review {
                    let files: Vec<_> = meta
                        .info
                        .iter_file_details()
                        .enumerate()
                        .filter(|(_, f)| !f.attrs().padding)
                        .map(|(i, f)| {
                            (
                                i,
                                f.filename.to_pathbuf().to_string_lossy().into_owned(),
                                f.len,
                            )
                        })
                        .collect();
                    let wanted = files.iter().map(|f| f.0).collect();
                    let name = meta.info.name().map_or(label, |n| n.into_owned());
                    reviews.lock().unwrap().push(Review {
                        name,
                        files,
                        wanted,
                        dir,
                        paused,
                        sub,
                        meta,
                    });
                    return Ok(());
                }
                start(&session, &next_id, &s, meta, dir.join(sub), None, paused)
                    .await
                    .map(drop)
            }
            .await;
            pending.lock().unwrap().retain(|p| p.id != pending_id);
            res
        });
        list.push(Pending {
            id: pending_id,
            name,
            hash,
            dir: pending_dir,
            task: task.abort_handle(),
            tries: 0,
            next: Instant::now(),
            retry_now,
        });
    }

    /// Torrents added with `review` whose metadata has arrived since the last call.
    pub fn take_reviews(&self) -> Vec<Review> {
        std::mem::take(&mut self.reviews.lock().unwrap())
    }

    /// Starts a reviewed torrent with the folder and files picked for it.
    pub fn confirm(&self, r: Review) {
        let (session, next_id, s) = (self.session.clone(), self.next_id.clone(), self.settings());
        let duplicates = self.duplicates.clone();
        let folder = r.folder();
        let only = (r.wanted.len() < r.files.len()).then(|| r.wanted.into_iter().collect());
        self.run(async move {
            // Added meanwhile, e.g. from a second Add window for the same torrent.
            if let Some(id) = start(&session, &next_id, &s, r.meta, folder, only, r.paused).await? {
                duplicates.lock().unwrap().push(id);
            }
            Ok(())
        });
    }

    /// Torrents added again from the window since the last call.
    pub fn take_duplicates(&self) -> Vec<usize> {
        std::mem::take(&mut self.duplicates.lock().unwrap())
    }

    /// One torrent as the window and the APIs show it: the moment a [`kick`] spends paused doesn't show.
    pub fn row(&self, id: usize, h: &ManagedTorrent) -> Row {
        let mut r = Row::new(id, h);
        if r.state == State::Paused && self.kicks.ids.lock().unwrap().contains(&id) {
            r.state = if h.stats().finished {
                State::Seeding
            } else {
                State::Downloading
            };
        }
        self.smooth(&mut r, h);
        r
    }

    /// Replaces librqbit's twitchy speeds with an average over [`SPEED_WINDOW`], sampled whenever rows are read.
    fn smooth(&self, r: &mut Row, h: &ManagedTorrent) {
        let mut speeds = self.speeds.lock().unwrap();
        let Some(live) = h.live() else {
            speeds.remove(&r.id);
            return;
        };
        let st = live.stats_snapshot();
        let samples = speeds.entry(r.id).or_default();
        // Until there's a second of history (just started, or nobody read rows for a while), keep librqbit's.
        if let Some(bps) = average(samples, Instant::now(), st.fetched_bytes, st.uploaded_bytes) {
            (r.down_bps, r.up_bps) = bps;
        }
    }

    /// Every transfer, including magnets still waiting for metadata.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows: Vec<Row> = self
            .session
            .with_torrents(|it| it.map(|(id, h)| self.row(id, h)).collect());
        let now = Instant::now();
        let secs = |at: Instant| at.saturating_duration_since(now).as_secs();
        let (retries, stalls) = (self.retries.lock().unwrap(), self.stalls.lock().unwrap());
        let mut busy = self.busy.lock().unwrap();
        busy.retain(|_, at| now.duration_since(*at) < LINGER);
        for r in &mut rows {
            if r.active() {
                busy.insert(r.id, now);
            } else {
                r.recent = matches!(r.state, State::Downloading | State::Seeding)
                    && busy.contains_key(&r.id);
            }
            match r.state {
                State::Error => {
                    if let Some(x) = retries.get(&r.id) {
                        (r.tries, r.retry_in) = (x.tries, x.next.map(secs));
                    }
                }
                State::Downloading => {
                    if let Some(&(_, since, kicks)) = stalls.get(&r.id).filter(|s| s.2 > 0) {
                        (r.tries, r.retry_in) =
                            (kicks, Some(secs(since + backoff(STALL, kicks, STALL_CAP))));
                    }
                }
                _ => {}
            }
        }
        // A verify in its remove/re-add gap keeps its row (and selection) as "Checking".
        for (&id, name) in self.verifying.lock().unwrap().iter() {
            if !rows.iter().any(|r| r.id == id) {
                rows.push(Row {
                    state: State::Checking,
                    ..Row::pending(id, name)
                });
            }
        }
        let ids: HashSet<usize> = rows.iter().map(|r| r.id).collect();
        self.speeds.lock().unwrap().retain(|id, _| ids.contains(id));
        // librqbit keeps torrents in a HashMap; without this, adding one can reshuffle the whole list.
        rows.sort_by_key(|r| r.id);
        rows.extend(self.pending.lock().unwrap().iter().map(|p| Row {
            tries: p.tries,
            retry_in: (p.tries > 0).then(|| secs(p.next)),
            ..Row::pending(p.id, &p.name)
        }));
        rows
    }

    /// Restarts a magnet's metadata lookup now instead of at its next scheduled retry.
    pub fn retry_now(&self, id: usize) {
        if let Some(p) = self.pending.lock().unwrap().iter().find(|p| p.id == id) {
            p.retry_now.notify_one();
        }
    }

    fn is_pending(&self, id: usize) -> bool {
        self.pending.lock().unwrap().iter().any(|p| p.id == id)
    }

    pub fn set_paused(&self, id: usize, paused: bool) {
        if self.is_pending(id) {
            return;
        }
        // Pausing hands the torrent to the user; resuming by hand starts automatic retries over. Stopping a
        // failed torrent (from a remote; the window offers Retry) stops its automatic retries too.
        let errored = self
            .session
            .get(TorrentIdOrHash::Id(id))
            .is_some_and(|h| matches!(h.stats().state, TorrentStatsState::Error));
        let mut retries = self.retries.lock().unwrap();
        if paused && errored {
            retries.entry(id).or_default().next = None;
        } else {
            retries.remove(&id);
        }
        drop(retries);
        let (session, kicks) = (self.session.clone(), self.kicks.clone());
        self.run(async move {
            // Waits out a kick in progress, which would otherwise resume right after this pause.
            let _serial = kicks.serial.lock().await;
            let handle = session
                .get(TorrentIdOrHash::Id(id))
                .context("torrent not found")?;
            // Already there (a click on a row that hadn't refreshed yet, a remote's "start all"): librqbit
            // would fail with "already paused" / "already live".
            let running =
                !handle.is_paused() && !matches!(handle.stats().state, TorrentStatsState::Error);
            match (paused, running) {
                (true, true) => session.pause(&handle).await,
                (false, false) => session.unpause(&handle).await,
                _ => Ok(()),
            }
        });
    }

    /// Re-checks every piece on disk. librqbit only re-hashes a torrent
    /// it adds without saved progress, so this removes it (keeping files and saved progress is dropped with it)
    /// and re-adds it under the same id with the same folder, files, trackers and paused state.
    pub fn verify(&self, id: usize) {
        let Some(h) = self.session.get(TorrentIdOrHash::Id(id)) else {
            return;
        };
        if matches!(h.stats().state, TorrentStatsState::Initializing { .. }) {
            return; // already checking
        }
        let name = h.name().unwrap_or_else(|| h.info_hash().as_string());
        if self
            .verifying
            .lock()
            .unwrap()
            .insert(id, name.clone())
            .is_some()
        {
            return;
        }
        self.retries.lock().unwrap().remove(&id);
        let (session, verifying, s) = (
            self.session.clone(),
            self.verifying.clone(),
            self.settings(),
        );
        self.run(async move {
            let res = async {
                let opts = AddTorrentOptions {
                    overwrite: true,
                    output_folder: Some(h.output_folder().to_string_lossy().into_owned()),
                    only_files: h.only_files(),
                    trackers: Some(h.shared().trackers.iter().map(|u| u.to_string()).collect()),
                    peer_limit: Some(s.peer_limit_per_torrent),
                    preferred_id: Some(id),
                    paused: matches!(h.stats().state, TorrentStatsState::Paused),
                    ..Default::default()
                };
                let bytes = h.with_metadata(|m| m.torrent_bytes.clone())?;
                session.delete(TorrentIdOrHash::Id(id), false).await?;
                session
                    .add_torrent(AddTorrent::from_bytes(bytes), Some(opts))
                    .await
                    .with_context(|| format!("“{name}” was removed for verifying and couldn't be re-added; open it again"))?;
                Ok(())
            }
            .await;
            verifying.lock().unwrap().remove(&id);
            res
        });
    }

    /// Fresh tracker announces and DHT lookups ("Ask for More Peers").
    pub fn reannounce(&self, id: usize) {
        let Some(h) = self.session.get(TorrentIdOrHash::Id(id)) else {
            return;
        };
        if !matches!(h.stats().state, TorrentStatsState::Live) {
            return;
        }
        let (session, kicks) = (self.session.clone(), self.kicks.clone());
        self.run(async move { Ok(kick(&session, &kicks, &h).await) });
    }

    /// Removing a pending add cancels it.
    pub fn remove(&self, id: usize, delete_files: bool) {
        let mut pending = self.pending.lock().unwrap();
        if let Some(i) = pending.iter().position(|p| p.id == id) {
            pending.remove(i).task.abort();
            self.changed.notify_one();
            return;
        }
        drop(pending);
        let session = self.session.clone();
        self.run(async move {
            // Already gone: a second click before the list caught up.
            let Some(h) = session.get(TorrentIdOrHash::Id(id)) else {
                return Ok(());
            };
            session.delete(TorrentIdOrHash::Id(id), delete_files).await?;
            UPLOADED.lock().unwrap().remove(&h.info_hash().as_string());
            Ok(())
        });
    }

    pub fn set_file_wanted(&self, id: usize, file: usize, wanted: bool) {
        let session = self.session.clone();
        self.run(async move {
            let handle = session
                .get(TorrentIdOrHash::Id(id))
                .context("torrent not found")?;
            let all = handle.with_metadata(|m| m.file_infos.len())?;
            let mut set: HashSet<usize> = handle
                .only_files()
                .map_or_else(|| (0..all).collect(), HashSet::from_iter);
            if wanted {
                set.insert(file)
            } else {
                set.remove(&file)
            };
            anyhow::ensure!(!set.is_empty(), "At least one file must stay selected");
            session.update_only_files(&handle, &set).await
        });
    }

    pub fn set_all_files_wanted(&self, id: usize) {
        let session = self.session.clone();
        self.run(async move {
            let handle = session
                .get(TorrentIdOrHash::Id(id))
                .context("torrent not found")?;
            let all = handle.with_metadata(|m| m.file_infos.len())?;
            session
                .update_only_files(&handle, &(0..all).collect())
                .await
        });
    }

    /// Opens one file in a media player while it downloads, resuming the torrent if it's paused.
    pub fn play(&self, id: usize, file: usize, name: &str) {
        let url = match crate::stream::url(self, id, file, name) {
            Ok(url) => url,
            Err(e) => return self.report(format!("{e:#}")),
        };
        let session = self.session.clone();
        self.run(async move {
            let handle = session
                .get(TorrentIdOrHash::Id(id))
                .context("torrent not found")?;
            if handle.is_paused() {
                session.unpause(&handle).await?;
            }
            crate::stream::open(url).await
        });
    }

    pub fn details(&self, id: usize) -> Option<Details> {
        let h = self.session.get(TorrentIdOrHash::Id(id))?;
        // The piece bitfield is only public through the HTTP-API wrapper, which is just a session handle.
        let haves = Api::new(self.session.clone(), None)
            .api_dump_haves(TorrentIdOrHash::Id(id))
            .map(|(bf, n)| (0..n as usize).map(|i| bf[i]).collect())
            .unwrap_or_default();
        Some(details(&h, haves))
    }

    /// Flush resume state before the process exits.
    pub fn stop(&self) {
        self.rt.block_on(self.session.stop());
    }
}

fn report(errors: &Mutex<Vec<String>>, e: String) {
    let mut errors = errors.lock().unwrap();
    if !errors.contains(&e) {
        errors.push(e);
    }
}

/// Adds a torrent whose metadata is already resolved; `Some(id)` if it was already in the session.
async fn start(
    session: &Arc<Session>,
    next_id: &Mutex<usize>,
    s: &Settings,
    meta: ListOnlyResponse,
    folder: PathBuf,
    only_files: Option<Vec<usize>>,
    paused: bool,
) -> anyhow::Result<Option<usize>> {
    let folder = folder.to_string_lossy().into_owned();
    loop {
        // librqbit's JSON persistence hands out "highest saved id + 1", so concurrent adds collide
        // and all but one come back as AlreadyManaged. Hand out ids ourselves.
        let floor = session.with_torrents(|it| it.map(|(id, _)| id + 1).max().unwrap_or(0));
        let id = {
            let mut n = next_id.lock().unwrap();
            *n = (*n).max(floor) + 1;
            *n - 1
        };
        let opts = AddTorrentOptions {
            // Reuse data already on disk (it gets verified).
            overwrite: true,
            output_folder: Some(folder.clone()),
            only_files: only_files.clone(),
            peer_limit: Some(s.peer_limit_per_torrent),
            initial_peers: Some(meta.seen_peers.clone()),
            preferred_id: Some(id),
            paused,
            ..Default::default()
        };
        let bytes = AddTorrent::from_bytes(meta.torrent_bytes.clone());
        match session.add_torrent(bytes, Some(opts)).await? {
            // The id belonged to a torrent still being restored from disk; try the next one.
            AddTorrentResponse::AlreadyManaged(_, h) if h.info_hash() != meta.info_hash => continue,
            AddTorrentResponse::Added(id, h) => {
                plugins::run(plugins::ADDED, id, &h, s);
                return Ok(None);
            }
            AddTorrentResponse::AlreadyManaged(id, _) => return Ok(Some(id)),
            AddTorrentResponse::ListOnly(_) => return Ok(None),
        }
    }
}

fn details(h: &ManagedTorrent, haves: Vec<bool>) -> Details {
    let progress = h.stats().file_progress;
    let only = h.only_files();
    let (pieces, piece_len, files) = h
        .with_metadata(|m| {
            let files = m
                .file_infos
                .iter()
                .enumerate()
                .filter(|(_, f)| !f.attrs.padding)
                .map(|(i, f)| FileRow {
                    index: i,
                    name: f.relative_filename.to_string_lossy().into_owned(),
                    len: f.len,
                    done: progress.get(i).copied().unwrap_or(0),
                    wanted: only.as_ref().is_none_or(|o| o.contains(&i)),
                })
                .collect();
            (
                m.info.lengths().total_pieces(),
                m.info.lengths().default_piece_length(),
                files,
            )
        })
        .unwrap_or((0, 0, vec![]));
    let peers = h.live().map_or_else(Vec::new, |live| {
        live.per_peer_stats_snapshot(Default::default())
            .peers
            .into_iter()
            .map(|(addr, p)| PeerRow {
                flag: crate::format::peer_flag(&addr),
                addr,
                client: p.client_name.unwrap_or_default(),
                kind: p
                    .conn_kind
                    .map(|k| format!("{k:?}").to_uppercase())
                    .unwrap_or_default(),
                downloaded: p.counters.fetched_bytes,
                uploaded: p.counters.uploaded_bytes,
            })
            .collect()
    });
    let mut trackers: Vec<String> = h.shared().trackers.iter().map(|u| u.to_string()).collect();
    trackers.sort();
    Details {
        info_hash: h.info_hash().as_string(),
        folder: h.output_folder().to_owned(),
        pieces,
        piece_len,
        files,
        peers,
        trackers,
        haves,
    }
}

fn options(s: &Settings, port: u16) -> SessionOptions {
    SessionOptions {
        fastresume: true,
        persistence: Some(SessionPersistenceConfig::Json {
            folder: Some(config_dir().join("session")),
        }),
        peer_id: Some(generate_azereus_style(*b"TO", (0, 1, 0, 0))),
        client_name_and_version: Some(format!("Torrie {}", env!("CARGO_PKG_VERSION"))),
        peer_limit: Some(s.peer_limit_per_torrent),
        listen: Some(ListenerOptions {
            // librqbit still calls uTP not stable; drop back to TcpOnly if it misbehaves.
            mode: ListenerMode::TcpAndUtp,
            listen_addr: SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)),
            enable_upnp_port_forwarding: s.port_forwarding,
            ..Default::default()
        }),
        ratelimits: s.limits(),
        ..Default::default()
    }
}

/// Adds a sample of byte counters and returns (down, up) bytes/s over [`SPEED_WINDOW`], once it spans a second.
fn average(
    samples: &mut VecDeque<(Instant, u64, u64)>,
    now: Instant,
    down: u64,
    up: u64,
) -> Option<(f64, f64)> {
    // Counters start over when a transfer restarts.
    if samples.back().is_some_and(|&(_, d, u)| down < d || up < u) {
        samples.clear();
    }
    samples.retain(|&(at, ..)| now - at <= SPEED_WINDOW);
    if samples
        .back()
        .is_none_or(|&(at, ..)| now - at >= Duration::from_millis(250))
    {
        samples.push_back((now, down, up));
    }
    let &(at, d, u) = samples.front()?;
    let secs = (now - at).as_secs_f64();
    (secs >= 1.0).then(|| ((down - d) as f64 / secs, (up - u) as f64 / secs))
}

/// Restarts downloads that stopped making progress: a pause/resume drops dead connections and asks
/// trackers and the DHT for fresh peers.
async fn unstick(session: Arc<Session>, stalls: Stalls, kicks: Arc<Kicks>) {
    let mut every = tokio::time::interval(Duration::from_secs(15));
    loop {
        every.tick().await;
        let live: Vec<_> = session.with_torrents(|it| {
            it.filter_map(|(id, h)| {
                let st = h.stats();
                (matches!(st.state, TorrentStatsState::Live) && !st.finished)
                    .then(|| (id, h.clone(), st.progress_bytes))
            })
            .collect()
        });
        let now = Instant::now();
        let mut due = vec![];
        {
            let mut last = stalls.lock().unwrap();
            let ids: HashSet<usize> = live.iter().map(|(id, ..)| *id).collect();
            last.retain(|id, _| ids.contains(id));
            for (id, h, bytes) in live {
                let (seen, since, kicks) = last.entry(id).or_insert((bytes, now, 0));
                if *seen != bytes {
                    (*seen, *since, *kicks) = (bytes, now, 0);
                // The rest wait for the next pass, so a mass stall can't hold pauses by hand up for long.
                } else if due.len() < KICKS_PER_PASS
                    && now - *since >= backoff(STALL, *kicks, STALL_CAP)
                {
                    (*since, *kicks) = (now, *kicks + 1);
                    due.push(h);
                }
            }
        }
        for h in due {
            kick(&session, &kicks, &h).await;
        }
    }
}

/// Pause and resume: drops dead connections and asks trackers and the DHT for fresh peers. While it runs the
/// torrent is listed in `kicks.ids`, so its moment paused neither shows nor counts as paused by the user.
async fn kick(session: &Arc<Session>, kicks: &Kicks, h: &Arc<ManagedTorrent>) {
    let _serial = kicks.serial.lock().await;
    kicks.ids.lock().unwrap().insert(h.id());
    if session.pause(h).await.is_ok()
        && let Err(e) = session.unpause(h).await
    {
        eprintln!("Torrie: resuming after a peer refresh: {e:#}");
    }
    kicks.ids.lock().unwrap().remove(&h.id());
}

/// Saves a settings change and applies the live parts (speed limits).
fn update(
    session: &Session,
    settings: &Mutex<Settings>,
    f: impl FnOnce(&mut Settings),
) -> anyhow::Result<()> {
    let mut s = settings.lock().unwrap();
    f(&mut s);
    s.sanitize();
    let limits = s.limits();
    session.ratelimits.set_download_bps(limits.download_bps);
    session.ratelimits.set_upload_bps(limits.upload_bps);
    s.save()
}

/// Advances every torrent's retry state; returns the errored torrents to restart now.
fn retry_due(
    retries: &mut HashMap<usize, Retry>,
    torrents: impl Iterator<Item = (usize, TorrentStatsState)>,
    now: Instant,
) -> Vec<usize> {
    let mut seen = HashSet::new();
    let mut due = vec![];
    for (id, state) in torrents {
        seen.insert(id);
        match state {
            TorrentStatsState::Error => {
                let r = retries.entry(id).or_insert(Retry {
                    next: Some(now + RETRY_FIRST),
                    ..Default::default()
                });
                r.ok_since = None;
                if r.next.is_some_and(|at| now >= at) {
                    r.tries += 1;
                    r.next = (r.tries < RETRY_MAX)
                        .then(|| now + backoff(RETRY_FIRST, r.tries, Duration::MAX));
                    due.push(id);
                }
            }
            TorrentStatsState::Paused => _ = retries.remove(&id),
            TorrentStatsState::Live => {
                if let Some(r) = retries.get_mut(&id)
                    && now - *r.ok_since.get_or_insert(now) >= RETRY_HEALTHY
                {
                    retries.remove(&id);
                }
            }
            TorrentStatsState::Initializing { .. } => {}
        }
    }
    retries.retain(|id, _| seen.contains(id));
    due
}

pub fn local_now() -> libc::tm {
    // SAFETY: localtime_r only writes into `tm`, and an all-zero tm is a valid value.
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    }
}

/// Bytes uploaded ever, by info hash: (from earlier runs, librqbit's counter when [`watch`] last saw it).
/// librqbit's counter restarts at 0 whenever a torrent starts, and reads 0 while it's stopped, so `watch` banks it.
// Up to 2 s of upload before a pause goes uncounted; bank in `set_paused` if that ever matters.
static UPLOADED: Mutex<BTreeMap<String, (u64, u64)>> = Mutex::new(BTreeMap::new());

fn uploaded_path() -> PathBuf {
    config_dir().join("uploaded.json")
}

/// Lifetime upload of torrent `hash`, given librqbit's counter `now`.
pub fn uploaded(hash: &str, now: u64) -> u64 {
    lifetime(UPLOADED.lock().unwrap().get(hash).copied().unwrap_or_default(), now)
}

/// A counter below the one last seen has restarted since, after reaching at least `last`.
fn lifetime((base, last): (u64, u64), now: u64) -> u64 {
    base + if now >= last { now } else { last + now }
}

/// Records counter `now`; whether anything changed.
fn bank(map: &mut BTreeMap<String, (u64, u64)>, hash: String, now: u64) -> bool {
    let e = map.entry(hash).or_default();
    let new = (lifetime(*e, now) - now, now);
    std::mem::replace(e, new) != new
}

fn load_uploaded() {
    let totals: BTreeMap<String, u64> = std::fs::read(uploaded_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    *UPLOADED.lock().unwrap() = totals.into_iter().map(|(h, n)| (h, (n, 0))).collect();
}

/// Saves the lifetime totals; [`watch`] calls it once a minute while they change, and quitting calls it too.
pub fn save_uploaded() {
    let totals: BTreeMap<String, u64> = UPLOADED
        .lock()
        .unwrap()
        .iter()
        .map(|(h, &(base, last))| (h.clone(), base + last))
        .collect();
    let res = serde_json::to_vec(&totals)
        .map_err(Into::into)
        .and_then(|b| std::fs::write(uploaded_path(), b));
    if let Err(e) = res {
        eprintln!("Torrie: saving upload totals: {e}");
    }
}

/// Session-wide housekeeping every 2 s, whether or not a window is open:
/// - `torrent-done` plugins when a download finishes. Only torrents seen downloading count, so torrents
///   restored already complete (or re-checked on launch) don't fire again.
/// - Lifetime upload totals (see [`UPLOADED`]), saved once a minute while they change.
/// - The ratio limit (stop seeding at ratio, on lifetime upload), then `torrent-done-seeding` plugins.
///   Resuming a stopped torrent by hand keeps it seeding, as does one already past the limit at launch.
/// - Automatic retries of errored torrents (see [`Retry`]).
/// - The alternative speed schedule. It only flips the switch when the schedule
///   changes, so switching by hand in between sticks.
/// - Keep awake: `caffeinate` runs while anything downloads, and dies with Torrie even if Torrie crashes.
async fn watch(
    session: Arc<Session>,
    settings: Arc<Mutex<Settings>>,
    retries: Retries,
    kicks: Arc<Kicks>,
) {
    let mut downloading: HashSet<usize> = HashSet::new();
    // Stopped once, or past the limit when first seen (resumed by hand before a relaunch): left seeding.
    let mut ratio_stopped: HashSet<usize> = HashSet::new();
    let mut seen: HashSet<usize> = HashSet::new();
    let (mut unsaved, mut saved) = (false, Instant::now());
    let mut scheduled: Option<bool> = None;
    let mut caffeinate: Option<tokio::process::Child> = None;
    let mut every = tokio::time::interval(Duration::from_secs(2));
    loop {
        every.tick().await;
        let s = settings.lock().unwrap().clone();

        let now = s.alt_speed_time_enabled.then(|| {
            let tm = local_now();
            s.alt_scheduled((tm.tm_hour * 60 + tm.tm_min) as u16, tm.tm_wday as u32)
        });
        if let Some(on) = now
            && scheduled != now
            && let Err(e) = update(&session, &settings, |s| s.alt_speed_enabled = on)
        {
            eprintln!("Torrie: saving settings: {e:#}");
        }
        scheduled = now;

        // Stats once per torrent per pass: each copies the torrent's per-file progress.
        let torrents: Vec<_> =
            session.with_torrents(|it| it.map(|(id, h)| (id, h.clone(), h.stats())).collect());
        // A kicked torrent was running a moment ago and is again a moment later.
        let kicked = kicks.ids.lock().unwrap().clone();
        let states = torrents.iter().map(|(id, _, st)| {
            (
                *id,
                if kicked.contains(id) {
                    TorrentStatsState::Live
                } else {
                    st.state
                },
            )
        });
        let due = retry_due(&mut retries.lock().unwrap(), states, Instant::now());
        for (_, h, _) in torrents.iter().filter(|(id, ..)| due.contains(id)) {
            _ = session.unpause(h).await;
        }
        let awake = s.keep_awake
            && torrents
                .iter()
                .any(|(_, _, st)| matches!(st.state, TorrentStatsState::Live) && !st.finished);
        if awake && caffeinate.is_none() {
            // -i holds off idle sleep only; the display still sleeps. -w exits with Torrie.
            caffeinate = tokio::process::Command::new("/usr/bin/caffeinate")
                .args(["-i", "-w", &std::process::id().to_string()])
                .kill_on_drop(true)
                .spawn()
                .inspect_err(|e| eprintln!("Torrie: keep awake: {e}"))
                .ok();
        } else if !awake {
            caffeinate = None;
        }

        let ids: HashSet<usize> = torrents.iter().map(|(id, ..)| *id).collect();
        downloading.retain(|id| ids.contains(id));
        ratio_stopped.retain(|id| ids.contains(id));
        seen.retain(|id| ids.contains(id));
        for (id, h, st) in torrents {
            let hash = h.info_hash().as_string();
            unsaved |= bank(&mut UPLOADED.lock().unwrap(), hash.clone(), st.uploaded_bytes);
            let ratio = if st.total_bytes == 0 {
                0.0
            } else {
                uploaded(&hash, st.uploaded_bytes) as f64 / st.total_bytes as f64
            };
            if seen.insert(id) && s.ratio_limit_enabled && ratio >= s.ratio_limit {
                ratio_stopped.insert(id);
            }
            if !matches!(st.state, TorrentStatsState::Live) {
                continue;
            }
            if !st.finished {
                downloading.insert(id);
                continue;
            }
            if downloading.remove(&id) {
                plugins::run(plugins::DONE, id, &h, &s);
            }
            if s.ratio_limit_enabled
                && ratio >= s.ratio_limit
                && ratio_stopped.insert(id)
                && session.pause(&h).await.is_ok()
            {
                plugins::run(plugins::DONE_SEEDING, id, &h, &s);
            }
        }
        if unsaved && saved.elapsed() >= Duration::from_secs(60) {
            save_uploaded();
            (unsaved, saved) = (false, Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_survives_restarts() {
        let mut m = BTreeMap::new();
        let total = |m: &BTreeMap<String, (u64, u64)>, now| lifetime(m["h"], now);
        assert!(bank(&mut m, "h".into(), 100));
        assert!(!bank(&mut m, "h".into(), 100), "unchanged");
        assert_eq!(total(&m, 150), 150);
        // Paused: librqbit's counter reads 0, then restarts from 0 on resume.
        assert_eq!(total(&m, 0), 100, "between a pause and the next pass");
        bank(&mut m, "h".into(), 0);
        bank(&mut m, "h".into(), 30);
        assert_eq!(total(&m, 30), 130);
        // Saved as base + last, loaded as (total, 0).
        m.insert("h".into(), (130, 0));
        assert_eq!(total(&m, 5), 135);
    }

    /// What lifetime upload totals cost, next to what a row already cost. 10,000 saved totals, a real torrent.
    /// `cargo test --release bench_upload_totals -- --nocapture`
    #[tokio::test(flavor = "multi_thread")]
    async fn bench_upload_totals() {
        const N: u32 = 100_000;
        let dir = std::env::temp_dir().join(format!("torrie-bench-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut torrent = b"d4:infod6:lengthi16384e4:name5:a.bin12:piece lengthi16384e6:pieces20:".to_vec();
        torrent.extend([7u8; 20]);
        torrent.extend(b"ee");
        let opts = SessionOptions {
            dht: None,
            listen: None,
            ..Default::default()
        };
        let session = Session::new_with_opts(dir.clone(), opts).await.unwrap();
        let add = AddTorrentOptions {
            paused: true,
            ..Default::default()
        };
        let (id, h) = match session
            .add_torrent(AddTorrent::from_bytes(torrent), Some(add))
            .await
            .unwrap()
        {
            AddTorrentResponse::Added(id, h) => (id, h),
            _ => panic!("not added"),
        };
        *UPLOADED.lock().unwrap() = (0..10_000u64)
            .map(|i| (format!("{i:040x}"), (i, 0)))
            .chain([(h.info_hash().as_string(), (5, 0))])
            .collect();
        let time = |label: &str, f: &mut dyn FnMut()| {
            let t = Instant::now();
            for _ in 0..N {
                f();
            }
            println!("{label:<34} {:>7.0} ns", t.elapsed().as_nanos() as f64 / N as f64);
        };
        time("h.stats() (already in every row)", &mut || {
            std::hint::black_box(h.stats());
        });
        time("Row::new, with lifetime upload", &mut || {
            std::hint::black_box(Row::new(id, &h));
        });
        time("added to Row::new: uploaded()", &mut || {
            std::hint::black_box(uploaded(&h.info_hash().as_string(), 0));
        });
        time("added to watch, per torrent", &mut || {
            let hash = h.info_hash().as_string();
            bank(&mut UPLOADED.lock().unwrap(), hash.clone(), 0);
            std::hint::black_box(uploaded(&hash, 0));
        });
        let file = dir.join("uploaded.json");
        let t = Instant::now();
        for _ in 0..100 {
            let totals: BTreeMap<String, u64> = UPLOADED
                .lock()
                .unwrap()
                .iter()
                .map(|(h, &(base, last))| (h.clone(), base + last))
                .collect();
            std::fs::write(&file, serde_json::to_vec(&totals).unwrap()).unwrap();
        }
        println!(
            "save, 10,001 totals ({} KB)        {:>7.0} µs",
            std::fs::metadata(&file).unwrap().len() / 1000,
            t.elapsed().as_micros() as f64 / 100.0
        );
        let t = Instant::now();
        let totals: BTreeMap<String, u64> =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        std::hint::black_box(totals.into_iter().map(|(h, n)| (h, (n, 0))).collect::<BTreeMap<_, _>>());
        println!("load, 10,001 totals                {:>7.0} µs", t.elapsed().as_micros());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn average_spans_lulls() {
        let (mut s, t0) = (VecDeque::new(), Instant::now());
        let at = |ms| t0 + Duration::from_millis(ms);
        assert_eq!(average(&mut s, at(0), 0, 0), None);
        assert_eq!(average(&mut s, at(500), 500, 0), None);
        // 2 MB in the first second, then a 2 s lull: still 1 MB/s, not 0.
        assert_eq!(average(&mut s, at(1000), 2_000_000, 0), Some((2e6, 0.0)));
        assert_eq!(average(&mut s, at(2000), 2_000_000, 0), Some((1e6, 0.0)));
        // A restart resets the counters.
        assert_eq!(average(&mut s, at(2500), 10, 0), None);
        // Samples older than the window drop out.
        assert_eq!(average(&mut s, at(9000), 20, 0), None);
    }

    #[test]
    fn alt_schedule() {
        let mut s = Settings {
            alt_speed_begin: 9 * 60,
            alt_speed_end: 17 * 60,
            alt_speed_days: 0b011_1110,
            ..Default::default()
        };
        assert!(s.alt_scheduled(9 * 60, 1)); // Monday 9:00
        assert!(!s.alt_scheduled(17 * 60, 1)); // ends at 17:00
        assert!(!s.alt_scheduled(12 * 60, 0)); // not on Sunday
        (s.alt_speed_begin, s.alt_speed_end) = (22 * 60, 6 * 60); // overnight
        assert!(
            s.alt_scheduled(23 * 60, 2) && s.alt_scheduled(60, 2) && !s.alt_scheduled(12 * 60, 2)
        );
        s.alt_speed_end = s.alt_speed_begin;
        assert!(s.alt_scheduled(0, 3));
    }

    #[test]
    fn backoff_doubles_up_to_its_cap() {
        let m = |n: u64| Duration::from_secs(60 * n);
        let waits: Vec<_> = (0..7).map(|n| backoff(m(1), n, m(16))).collect();
        assert_eq!(waits, [m(1), m(2), m(4), m(8), m(16), m(16), m(16)]);
        assert_eq!(
            backoff(m(3), 1000, STALL_CAP),
            STALL_CAP,
            "huge counts don't overflow"
        );
    }

    #[test]
    fn errored_torrents_retry_with_backoff_then_stop() {
        use TorrentStatsState::*;
        let mut r = HashMap::new();
        let t0 = Instant::now();
        let min = |m: u64| t0 + Duration::from_secs(60 * m);
        let tick = |r: &mut HashMap<usize, Retry>, s: TorrentStatsState, at: Instant| {
            retry_due(r, [(1, s)].into_iter(), at)
        };
        assert!(
            tick(&mut r, Error, t0).is_empty(),
            "first try waits a minute"
        );
        assert_eq!(tick(&mut r, Error, min(1)), vec![1]);
        assert!(tick(&mut r, Error, min(2)).is_empty(), "then two minutes");
        assert_eq!(tick(&mut r, Error, min(3)), vec![1]);
        // Briefly running doesn't reset the count…
        tick(&mut r, Live, min(4));
        assert_eq!(r[&1].tries, 2);
        // …and after the sixth try it gives up (1+2+4+8+16 minutes after the first).
        let mut at = min(3);
        for _ in 0..4 {
            at += backoff(RETRY_FIRST, r[&1].tries, Duration::MAX);
            assert_eq!(tick(&mut r, Error, at), vec![1]);
        }
        assert_eq!((r[&1].tries, r[&1].next), (RETRY_MAX, None));
        assert!(tick(&mut r, Error, at + Duration::from_secs(86_400)).is_empty());
        // Running healthily for ten minutes clears it; so does pausing or removal.
        tick(&mut r, Live, at);
        tick(&mut r, Live, at + RETRY_HEALTHY);
        assert!(r.is_empty());
        tick(&mut r, Error, at);
        tick(&mut r, Paused, at);
        assert!(r.is_empty());
        tick(&mut r, Error, at);
        retry_due(&mut r, std::iter::empty(), at);
        assert!(r.is_empty());
    }

    #[test]
    fn errored_torrents_stopped_by_hand_stay_stopped() {
        // What `set_paused(id, true)` leaves for an errored torrent.
        let mut r = HashMap::from([(1, Retry::default())]);
        let day = Instant::now() + Duration::from_secs(86_400);
        assert!(retry_due(&mut r, [(1, TorrentStatsState::Error)].into_iter(), day).is_empty());
    }

    #[test]
    fn alt_limits_replace_normal_ones() {
        let mut s = Settings {
            speed_limit_down_enabled: true,
            speed_limit_down_kbps: 500,
            ..Default::default()
        };
        assert_eq!(s.limits().download_bps.map(|b| b.get()), Some(500_000));
        assert_eq!(s.limits().upload_bps, None);
        s.alt_speed_enabled = true;
        assert_eq!(s.limits().download_bps.map(|b| b.get()), Some(50_000));
        assert_eq!(s.limits().upload_bps.map(|b| b.get()), Some(50_000));
    }
}
