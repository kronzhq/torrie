//! Plugins extend Torrie. A plugin is a folder in `plugins/` described by its manifest, `plugin.json`, with any of:
//! - Hooks: executables run once per event, with Transmission's script environment (`TR_*`), so Transmission
//!   scripts work unchanged.
//! - A plugin process (`main`): started with Torrie and kept running. It talks to Torrie over the plugin protocol,
//!   JSON-RPC 2.0 with one message per line on stdin and stdout: Torrie sends it events and asks it to run commands
//!   and render panels; it calls Torrie's API methods (`openrpc.json`) the same way.
//! - Contributions (`contributes`): commands (the Plugins menu), panels (Inspector tabs, filled with the views the
//!   plugin process renders) and settings (Settings → Plugins, handed to hooks and the process as
//!   `TORRIE_SETTING_<KEY>`).
//!
//! Plugins are distributed as packages: the plugin folder zipped into a `.torrieplugin` file, opened from Finder or
//! through an install link, `torrie://install?url=https://…`.
//!
//! Guard rails:
//! - Plugins start off, after an update too; only `plugins_enabled` ones run.
//! - Every executable must be inside the plugin's folder (no absolute paths, `..` or symlinks out).
//! - A plain environment (no inherited variables beyond PATH, HOME, USER, LOGNAME, LANG, TMPDIR), each in its own
//!   process group.
//! - Hooks get no stdin and a timeout (manifest `timeout`, default 60 s) after which their whole process group is
//!   killed; at most [`MAX_RUNNING`] run at once, the rest queue, and Torrie never waits on them.
//! - Output goes to `plugins/<id>.log`, which starts over once it passes 1 MB.
//!
//! Plugins run as you, with your files and network: only switch on plugins you trust.

use std::{
    collections::{BTreeMap, HashMap},
    io::Write as _,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context as _, anyhow, bail, ensure};
use librqbit::ManagedTorrent;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{Semaphore, mpsc, oneshot},
};

use crate::{
    engine::{Engine, Settings, config_dir, local_now},
    format::Row,
};

pub const ADDED: &str = "torrent-added";
pub const DONE: &str = "torrent-done";
pub const DONE_SEEDING: &str = "torrent-done-seeding";
pub const EVENTS: [&str; 3] = [ADDED, DONE, DONE_SEEDING];

pub const MAX_RUNNING: usize = 4;
const MAX_TIMEOUT: u64 = 3600;
const LOG_LIMIT: u64 = 1 << 20;
const PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
/// How long a plugin process gets to exit after SIGTERM before it's killed.
const STOP_GRACE: Duration = Duration::from_secs(5);
/// Largest package an install link downloads.
const MAX_PACKAGE: u64 = 50 << 20;

/// The plugin that posted "Download Complete" before Torrie did it itself ([`Settings::notify_done`]).
const NOTIFY: &str = "notify";

/// Its hook as shipped. macOS credits osascript's notifications to Script Editor, and drops them unless Script
/// Editor may notify.
const NOTIFY_SH: &str = r#"#!/bin/sh
# Torrie's bundled plugin, and an example to copy. See README "Plugins" for every variable.
exec osascript -e 'on run argv' -e 'display notification (item 1 of argv) with title "Download Complete"' -e 'end run' "$TR_TORRENT_NAME"
"#;

/// `plugin.json`. Unknown keys are ignored so manifests can grow.
#[derive(Debug, Deserialize)]
pub struct Manifest {
    /// The folder name a package installs to. Defaults to `name`, lowercased with dashes.
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    /// Event → executable run once each time it happens, relative to the plugin folder.
    #[serde(default)]
    pub hooks: BTreeMap<String, String>,
    /// Seconds a hook may run before it's stopped.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    /// The plugin process's executable, relative to the plugin folder.
    #[serde(default)]
    pub main: Option<String>,
    #[serde(default)]
    pub contributes: Contributes,
    /// Manifests from before hooks had their own key: `main` was the one hook, run for each of these events.
    #[serde(default)]
    events: Option<Vec<String>>,
}

fn default_timeout() -> u64 {
    60
}

/// What a plugin adds to Torrie's interface.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Contributes {
    pub commands: Vec<Command>,
    pub panels: Vec<Panel>,
    pub settings: Vec<Setting>,
}

/// An item in the Plugins menu. Choosing it sends `command.run` to the plugin process.
#[derive(Clone, Debug, Deserialize)]
pub struct Command {
    pub id: String,
    pub title: String,
    /// Acts on the selected transfer, which the request carries.
    #[serde(default)]
    pub torrent: bool,
}

/// An Inspector tab. While it's on show, Torrie sends `panel.render` once a second and draws the view it gets back.
#[derive(Clone, Debug, Deserialize)]
pub struct Panel {
    pub id: String,
    pub title: String,
}

/// A value set in Settings → Plugins.
#[derive(Clone, Debug, Deserialize)]
pub struct Setting {
    pub key: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(rename = "type", default)]
    pub kind: Kind,
    #[serde(default)]
    pub default: Value,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    String,
    Boolean,
    Folder,
}

/// One block of a view, which is what a panel shows: a list of blocks. Unknown types are skipped, so a plugin can
/// use blocks a newer Torrie adds.
#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Block {
    Heading {
        text: String,
    },
    Text {
        text: String,
    },
    /// A label and its value, like the rows of the Overview tab.
    Field {
        label: String,
        value: String,
    },
    /// Runs one of the plugin's commands on the transfer.
    Button {
        label: String,
        command: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug)]
pub struct Plugin {
    /// The folder name; what Settings stores.
    pub id: String,
    pub manifest: Manifest,
    dir: PathBuf,
    main: Option<PathBuf>,
    hooks: BTreeMap<String, PathBuf>,
}

pub fn dir() -> PathBuf {
    config_dir().join("plugins")
}

fn log_path(id: &str) -> PathBuf {
    dir().join(format!("{id}.log"))
}

/// Plugins Torrie ships, off until switched on: folder, manifest, hook file name and hook.
const SEEDED: [(&str, &str, &str, &str); 1] = [(
    "shortcuts",
    include_str!("../assets/plugins/shortcuts/plugin.json"),
    "shortcuts.sh",
    include_str!("../assets/plugins/shortcuts/shortcuts.sh"),
)];

/// Adds each shipped plugin once, so ones added in an update appear and ones you delete stay gone.
/// `plugins/.seeded` lists the ids already added.
pub fn seed() {
    let marker = dir().join(".seeded");
    let seeded = std::fs::read_to_string(&marker).unwrap_or_default();
    for (id, json, file, script) in SEEDED {
        if seeded.lines().any(|l| l == id) {
            continue;
        }
        let plugin = dir().join(id);
        let main = plugin.join(file);
        // Installs from before the marker already have theirs; leave any edits alone.
        let write = || {
            std::fs::create_dir_all(&plugin)?;
            std::fs::write(plugin.join("plugin.json"), json)?;
            std::fs::write(&main, script)?;
            std::fs::set_permissions(&main, std::fs::Permissions::from_mode(0o755))
        };
        let res = (if plugin.exists() { Ok(()) } else { write() })
            .and_then(|_| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&marker)?
                    .write_all(format!("{id}\n").as_bytes())
            });
        if let Err(e) = res {
            eprintln!("Torrie: seeding plugins: {e}");
        }
    }
}

/// Once, for installs that had the notify plugin (in `root`'s `.seeded`): `notify_done` follows whether it was
/// on, and an unedited copy goes away. An edited one stays a plugin, so notifications are off rather than
/// doubled; one you deleted leaves them off. Returns whether `s` changed.
pub fn migrate_notify(root: &Path, s: &mut Settings) -> bool {
    let marker = root.join(".seeded");
    let Ok(seeded) = std::fs::read_to_string(&marker) else {
        return false;
    };
    if !seeded.lines().any(|l| l == NOTIFY) {
        return false;
    }
    let folder = root.join(NOTIFY);
    // No hook at all is the copy an unreleased build left.
    let shipped = load(NOTIFY.into(), &folder).is_ok_and(|p| {
        p.main.is_none()
            && p.hooks
                .values()
                .all(|f| std::fs::read_to_string(f).is_ok_and(|s| s == NOTIFY_SH))
    });
    s.notify_done = shipped && s.plugins_enabled.iter().any(|p| p == NOTIFY);
    if shipped {
        _ = std::fs::remove_dir_all(&folder);
        s.plugins_enabled.retain(|p| p != NOTIFY);
    }
    let rest: String = seeded
        .lines()
        .filter(|l| *l != NOTIFY)
        .map(|l| format!("{l}\n"))
        .collect();
    _ = std::fs::write(&marker, rest);
    true
}

/// Every plugin folder, sorted, with the reason it can't load if it can't.
// Re-read on each event (a few small files); cache if anyone installs hundreds.
pub fn list() -> Vec<(String, Result<Plugin, String>)> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir())
        .map(|it| {
            it.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs.into_iter()
        .filter_map(|d| {
            let id = d.file_name()?.to_string_lossy().into_owned();
            (!id.starts_with('.')).then(|| (id.clone(), load(id, &d).map_err(|e| format!("{e:#}"))))
        })
        .collect()
}

fn load(id: String, dir: &Path) -> anyhow::Result<Plugin> {
    let dir = dir.canonicalize()?;
    let manifest = parse(&std::fs::read(dir.join("plugin.json")).context("no plugin.json")?)?;
    let hooks = manifest
        .hooks
        .iter()
        .map(|(e, f)| Ok((e.clone(), executable(&dir, f)?)))
        .collect::<anyhow::Result<_>>()?;
    let main = manifest
        .main
        .as_deref()
        .map(|f| executable(&dir, f))
        .transpose()?;
    Ok(Plugin {
        id,
        manifest,
        dir,
        main,
        hooks,
    })
}

/// Reads and checks a manifest, short of the files it names.
fn parse(json: &[u8]) -> anyhow::Result<Manifest> {
    let mut m: Manifest = serde_json::from_slice(json).context("plugin.json")?;
    ensure!(!m.name.trim().is_empty(), "plugin.json: \"name\" is empty");
    if let Some(events) = m.events.take()
        && let Some(main) = m.main.take()
    {
        m.hooks
            .extend(events.into_iter().map(|e| (e, main.clone())));
    }
    if let Some(e) = m.hooks.keys().find(|e| !EVENTS.contains(&e.as_str())) {
        bail!(
            "plugin.json: unknown event \"{e}\" (expected one of {})",
            EVENTS.join(", ")
        );
    }
    let c = &m.contributes;
    ensure!(
        m.main.is_some() || (c.commands.is_empty() && c.panels.is_empty()),
        "plugin.json: commands and panels need \"main\", the plugin process"
    );
    ensure!(
        m.id.is_empty() || m.id == slug(&m.id),
        "plugin.json: \"id\" may only have a-z, 0-9 and dashes"
    );
    if m.id.is_empty() {
        m.id = slug(&m.name);
    }
    ensure!(
        !m.id.is_empty(),
        "plugin.json: \"name\" needs a letter or digit"
    );
    m.timeout = m.timeout.clamp(1, MAX_TIMEOUT);
    Ok(m)
}

/// `My Plugin!` → `my-plugin`.
fn slug(s: &str) -> String {
    s.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// `rel` inside `dir`, resolved through symlinks, and executable.
fn executable(dir: &Path, rel: &str) -> anyhow::Result<PathBuf> {
    let path = Path::new(rel);
    if !path.components().all(|c| matches!(c, Component::Normal(_))) {
        bail!("plugin.json: \"{rel}\" must be a path inside the plugin folder");
    }
    // Resolves symlinks, so a link pointing outside the folder is caught too.
    let path = dir
        .join(path)
        .canonicalize()
        .with_context(|| format!("{rel} not found"))?;
    ensure!(
        path.starts_with(dir),
        "{rel} points outside the plugin folder"
    );
    let meta = std::fs::metadata(&path)?;
    ensure!(
        meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        "{rel} isn't executable (chmod +x)"
    );
    Ok(path)
}

/// A setting's value: what the user set, else the manifest's default.
pub fn value(id: &str, setting: &Setting, s: &Settings) -> Value {
    s.plugin_settings
        .get(id)
        .and_then(|m| m.get(&setting.key))
        .cloned()
        .unwrap_or_else(|| setting.default.clone())
}

/// A setting's value as text, as the environment and Settings show it.
pub fn text(v: &Value) -> String {
    match v {
        Value::String(v) => v.clone(),
        Value::Null => String::new(),
        v => v.to_string(),
    }
}

/// `TORRIE_SETTING_<KEY>` for each of the plugin's settings.
fn setting_env(p: &Plugin, s: &Settings) -> Vec<(String, String)> {
    let settings = &p.manifest.contributes.settings;
    settings
        .iter()
        .map(|set| {
            let key: String = set
                .key
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() {
                        c.to_ascii_uppercase()
                    } else {
                        '_'
                    }
                })
                .collect();
            (format!("TORRIE_SETTING_{key}"), text(&value(&p.id, set, s)))
        })
        .collect()
}

/// `exe` from plugin `p`, in its folder and process group, with a plain environment and its settings.
fn command(exe: &Path, p: &Plugin, s: &Settings) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(exe);
    cmd.env_clear()
        .env("PATH", PATH)
        .envs(
            ["HOME", "USER", "LOGNAME", "LANG", "TMPDIR"]
                .into_iter()
                .filter_map(|k| Some((k, std::env::var_os(k)?))),
        )
        .envs(setting_env(p, s))
        .current_dir(&p.dir)
        // Its own process group, so stopping it also stops whatever it started.
        .process_group(0)
        .kill_on_drop(true);
    cmd
}

/// Runs every enabled plugin's hook for `event`, and sends the event to every plugin process. Call from the
/// engine's runtime; never blocks.
pub fn run(event: &'static str, id: usize, h: &ManagedTorrent, s: &Settings) {
    notify_all(
        "event",
        json!({ "event": event, "torrent": crate::rpc::torrent_json(&Row::new(id, h), h) }),
    );
    let hooks: Vec<_> = list()
        .into_iter()
        .filter_map(|(_, p)| p.ok())
        .filter(|p| s.plugins_enabled.contains(&p.id))
        .filter_map(|p| {
            Some((
                command(p.hooks.get(event)?, &p, s),
                p.id,
                p.manifest.timeout,
            ))
        })
        .collect();
    if hooks.is_empty() {
        return;
    }
    let env = env(event, id, h, s);
    for (mut cmd, plugin, timeout) in hooks {
        cmd.arg(event)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null());
        let name = env
            .iter()
            .find(|(k, _)| *k == "TR_TORRENT_NAME")
            .map_or(String::new(), |(_, v)| v.clone());
        tokio::spawn(exec(cmd, plugin, event, name, timeout));
    }
}

/// The `TR_*` script variables, plus Torrie's.
fn env(event: &str, id: usize, h: &ManagedTorrent, s: &Settings) -> Vec<(&'static str, String)> {
    let name = h.name().unwrap_or_else(|| h.info_hash().as_string());
    let trackers: Vec<String> = h.shared().trackers.iter().map(|u| u.to_string()).collect();
    let mut env = vec![
        ("TR_APP_VERSION", env!("CARGO_PKG_VERSION").to_owned()),
        ("TR_TIME_LOCALTIME", timestamp()),
        ("TR_TORRENT_ID", id.to_string()),
        (
            "TR_TORRENT_DIR",
            torrent_dir(h.output_folder(), &name)
                .to_string_lossy()
                .into_owned(),
        ),
        ("TR_TORRENT_HASH", h.info_hash().as_string()),
        (
            "TR_TORRENT_BYTES_DOWNLOADED",
            h.stats().progress_bytes.to_string(),
        ),
        ("TR_TORRENT_LABELS", String::new()),
        ("TR_TORRENT_PRIORITY", "0".into()),
        ("TR_TORRENT_TRACKERS", trackers.join(",")),
        ("TR_TORRENT_NAME", name),
        ("TORRIE_EVENT", event.to_owned()),
    ];
    if s.rpc_enabled {
        env.push((
            "TORRIE_RPC_URL",
            format!("http://127.0.0.1:{}{}", s.rpc_port, crate::rpc::PATH),
        ));
    }
    env
}

/// Runs one hook to completion or its timeout.
async fn exec(
    mut cmd: tokio::process::Command,
    plugin: String,
    event: &str,
    name: String,
    timeout: u64,
) {
    static SLOTS: Semaphore = Semaphore::const_new(MAX_RUNNING);
    let Ok(_slot) = SLOTS.acquire().await else {
        return;
    };
    let log_path = log_path(&plugin);
    let res = async {
        let mut log = open_log(&log_path)?;
        writeln!(log, "[{}] {event}: {name}", timestamp())?;
        let mut child = cmd
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()
            .context("starting")?;
        match tokio::time::timeout(Duration::from_secs(timeout), child.wait()).await {
            Ok(status) => {
                let status = status?;
                ensure!(status.success(), "{status}");
            }
            Err(_) => {
                if let Some(pid) = child.id() {
                    // SAFETY: plain syscall; the group is the child's own (process_group(0) in `command`).
                    unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
                }
                _ = child.wait().await;
                bail!("timed out after {timeout}s and was stopped");
            }
        }
        Ok(())
    }
    .await;
    if let Err(e) = res {
        eprintln!("Torrie: plugin {plugin} ({event}): {e:#}");
        if let Ok(mut log) = open_log(&log_path) {
            _ = writeln!(log, "Torrie: {e:#}");
        }
    }
}

// --- plugin processes ---

/// Torrie's API, as plugin processes call it: method and params to a result or a JSON-RPC (code, message) error.
type Api = Arc<dyn Fn(&str, &Value) -> Result<Value, (i64, String)> + Send + Sync>;
/// Requests sent to a plugin process, by id, waiting for its answer.
type Waiting = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A running plugin process.
struct Process {
    plugin: Arc<Plugin>,
    /// Tells this run apart from a later one of the same plugin.
    generation: u64,
    /// Lines for its stdin.
    to_plugin: mpsc::UnboundedSender<String>,
    waiting: Waiting,
    /// Dropping it stops the process.
    _stop: oneshot::Sender<()>,
}

static PROCESSES: Mutex<BTreeMap<String, Process>> = Mutex::new(BTreeMap::new());
/// Why plugin processes stopped on their own, for Settings.
static EXITED: Mutex<BTreeMap<String, String>> = Mutex::new(BTreeMap::new());
/// Request ids and generations.
static NEXT: AtomicU64 = AtomicU64::new(1);

/// Starts the plugin processes of enabled plugins that aren't running (including ones that stopped on their own),
/// and stops those of plugins switched off or removed.
pub fn sync(engine: &Arc<Engine>) {
    let s = engine.settings();
    let wanted: Vec<Plugin> = list()
        .into_iter()
        .filter_map(|(_, p)| p.ok())
        .filter(|p| p.main.is_some() && s.plugins_enabled.contains(&p.id))
        .collect();
    let stopped: Vec<Process> = {
        let mut procs = PROCESSES.lock().unwrap();
        let gone: Vec<String> = procs
            .keys()
            .filter(|id| !wanted.iter().any(|p| &&p.id == id))
            .cloned()
            .collect();
        gone.iter().filter_map(|id| procs.remove(id)).collect()
    };
    drop(stopped);
    let _rt = engine.runtime().enter();
    for p in wanted {
        if PROCESSES.lock().unwrap().contains_key(&p.id) {
            continue;
        }
        let (id, e) = (p.id.clone(), engine.clone());
        if let Err(err) = start(
            p,
            &s,
            Arc::new(move |method, params| crate::rpc::call(&e, method, params)),
        ) {
            EXITED.lock().unwrap().insert(id, format!("{err:#}"));
        }
    }
}

/// Restarts a plugin process, so it picks up new settings or files.
pub fn restart(engine: &Arc<Engine>, id: &str) {
    let stopped = PROCESSES.lock().unwrap().remove(id);
    drop(stopped);
    sync(engine);
}

/// Stops every plugin process; [`sync`] starts them again.
pub fn stop_all() {
    let stopped = std::mem::take(&mut *PROCESSES.lock().unwrap());
    drop(stopped);
}

pub fn running(id: &str) -> bool {
    PROCESSES.lock().unwrap().contains_key(id)
}

/// Why a plugin process isn't running, if it stopped on its own or couldn't start.
pub fn exited(id: &str) -> Option<String> {
    EXITED.lock().unwrap().get(id).cloned()
}

/// Commands of running plugin processes, with their plugin's id.
pub fn commands() -> Vec<(String, Command)> {
    let procs = PROCESSES.lock().unwrap();
    procs
        .values()
        .flat_map(|p| {
            p.plugin
                .manifest
                .contributes
                .commands
                .iter()
                .map(|c| (p.plugin.id.clone(), c.clone()))
        })
        .collect()
}

/// Panels of running plugin processes, with their plugin's id.
pub fn panels() -> Vec<(String, Panel)> {
    let procs = PROCESSES.lock().unwrap();
    procs
        .values()
        .flat_map(|p| {
            p.plugin
                .manifest
                .contributes
                .panels
                .iter()
                .map(|c| (p.plugin.id.clone(), c.clone()))
        })
        .collect()
}

/// Spawns `p`'s plugin process and the tasks that talk to it. Call inside the engine's runtime.
fn start(p: Plugin, s: &Settings, api: Api) -> anyhow::Result<()> {
    let main = p.main.clone().context("no \"main\"")?;
    let log_path = log_path(&p.id);
    // A running process's log only starts over when it restarts.
    let log: Stdio = open_log(&log_path).map_or(Stdio::null(), |mut f| {
        _ = writeln!(f, "[{}] started", timestamp());
        f.into()
    });
    let mut child = command(&main, &p, s)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(log)
        .spawn()
        .context("starting")?;
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        bail!("no pipes")
    };
    let id = p.id.clone();
    let (to_plugin, mut outbox) = mpsc::unbounded_channel::<String>();
    let (stop, stopped) = oneshot::channel();
    let waiting = Waiting::default();
    let generation = NEXT.fetch_add(1, Ordering::Relaxed);
    let process = Process {
        plugin: Arc::new(p),
        generation,
        to_plugin: to_plugin.clone(),
        waiting: waiting.clone(),
        _stop: stop,
    };
    // Listed before anything can notice it exit.
    PROCESSES.lock().unwrap().insert(id.clone(), process);
    EXITED.lock().unwrap().remove(&id);

    tokio::spawn(async move {
        while let Some(line) = outbox.recv().await {
            if stdin
                .write_all(format!("{line}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
    });
    let lp = log_path.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            receive(&line, &api, &to_plugin, &waiting, &lp);
        }
        waiting.lock().unwrap().clear();
    });
    tokio::spawn(async move {
        let pid = child.id();
        // SAFETY: plain syscall; the group is the child's own, and not yet reaped, so the id can't be reused.
        let signal = |sig| _ = pid.map(|pid| unsafe { libc::killpg(pid as libc::pid_t, sig) });
        let exit = tokio::select! {
            status = child.wait() => Some(status.map_or_else(|e| e.to_string(), |s| format!("exited ({s})"))),
            _ = stopped => {
                signal(libc::SIGTERM);
                if tokio::time::timeout(STOP_GRACE, child.wait()).await.is_err() {
                    signal(libc::SIGKILL);
                    _ = child.wait().await;
                }
                None
            }
        };
        if let Ok(mut log) = open_log(&log_path) {
            _ = writeln!(
                log,
                "[{}] {}",
                timestamp(),
                exit.as_deref().unwrap_or("stopped")
            );
        }
        let Some(exit) = exit else { return };
        let mut procs = PROCESSES.lock().unwrap();
        if procs.get(&id).is_some_and(|p| p.generation == generation) {
            procs.remove(&id);
            EXITED
                .lock()
                .unwrap()
                .insert(id, format!("Stopped: {exit}; see its log"));
        }
    });
    Ok(())
}

/// One line from a plugin process: a call to Torrie's API, or the answer to one of Torrie's requests. Anything
/// else (a stray print) goes to its log.
fn receive(
    line: &str,
    api: &Api,
    to_plugin: &mpsc::UnboundedSender<String>,
    waiting: &Waiting,
    log: &Path,
) {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        if let Ok(mut f) = open_log(log) {
            _ = writeln!(f, "{line}");
        }
        return;
    };
    if let Some(method) = msg.get("method").and_then(Value::as_str) {
        let out = api(method, msg.get("params").unwrap_or(&json!({})));
        // A call without an id is a notification: it runs, but gets no answer.
        if let Some(id) = msg.get("id") {
            _ = to_plugin.send(crate::rpc::response(id.clone(), out).to_string());
        }
    } else if let Some(tx) = msg
        .get("id")
        .and_then(Value::as_u64)
        .and_then(|id| waiting.lock().unwrap().remove(&id))
    {
        _ = tx.send(match msg.get("error") {
            Some(e) => Err(e
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("failed")
                .to_owned()),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        });
    }
}

/// Sends `method` to a plugin process and waits up to `limit` for its answer.
pub async fn request(
    plugin: String,
    method: &'static str,
    params: Value,
    limit: Duration,
) -> anyhow::Result<Value> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    let waiting = {
        let procs = PROCESSES.lock().unwrap();
        let p = procs
            .get(&plugin)
            .with_context(|| format!("The plugin “{plugin}” isn't running"))?;
        p.waiting.lock().unwrap().insert(id, tx);
        _ = p.to_plugin.send(
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }).to_string(),
        );
        p.waiting.clone()
    };
    match tokio::time::timeout(limit, rx).await {
        Ok(Ok(res)) => res.map_err(|e| anyhow!("{plugin}: {e}")),
        Ok(Err(_)) => bail!("The plugin “{plugin}” stopped before answering"),
        Err(_) => {
            waiting.lock().unwrap().remove(&id);
            bail!(
                "The plugin “{plugin}” didn't answer {method} within {}s",
                limit.as_secs()
            )
        }
    }
}

/// Sends a notification (no answer) to every plugin process.
fn notify_all(method: &str, params: Value) {
    let line = json!({ "jsonrpc": "2.0", "method": method, "params": params }).to_string();
    PROCESSES
        .lock()
        .unwrap()
        .values()
        .for_each(|p| _ = p.to_plugin.send(line.clone()));
}

// --- packages ---

/// A package read as far as its manifest, waiting for the user to install it.
pub struct Package {
    file: PathBuf,
    /// Downloaded, so deleted once dropped.
    temporary: bool,
    /// The plugin folder inside the zip: `""` or `"name/"`.
    root: String,
    pub manifest: Manifest,
    /// Name and version of the installed plugin it replaces.
    pub replaces: Option<String>,
}

impl Drop for Package {
    fn drop(&mut self) {
        if self.temporary {
            _ = std::fs::remove_file(&self.file);
        }
    }
}

/// Whether `url` is an install link or a package file.
pub fn is_package(url: &str) -> bool {
    url.starts_with("torrie://install")
        || (url.starts_with("file://") && url.ends_with(".torrieplugin"))
}

/// Downloads the package an install link (`torrie://install?url=https://…`) points at, or opens a package file
/// (`file://….torrieplugin`), and reads its manifest. Nothing is unpacked until [`install`].
pub async fn fetch(url: String) -> anyhow::Result<Package> {
    let link = url::Url::parse(&url).with_context(|| format!("Can't open {url}"))?;
    let (file, download) = if link.scheme() == "torrie" {
        let src = link
            .query_pairs()
            .find(|(k, _)| k == "url")
            .map(|(_, v)| v.into_owned())
            .context("The install link has no url")?;
        ensure!(
            src.starts_with("https://"),
            "Install links must point at an https:// address"
        );
        let name = format!(
            "torrie-{}-{}.torrieplugin",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        (std::env::temp_dir().join(name), Some(src))
    } else {
        (
            link.to_file_path()
                .map_err(|_| anyhow!("Can't open {url}"))?,
            None,
        )
    };
    let res = async {
        if let Some(src) = &download {
            let out = tokio::process::Command::new("/usr/bin/curl")
                .args([
                    "-fsSL",
                    "--proto",
                    "=https",
                    "--proto-redir",
                    "=https",
                    "--max-filesize",
                    &MAX_PACKAGE.to_string(),
                    "-o",
                ])
                .arg(&file)
                .arg(src)
                .output()
                .await
                .context("curl")?;
            ensure!(
                out.status.success(),
                "Downloading {src}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        read_manifest(&file).await
    }
    .await;
    let temporary = download.is_some();
    let (root, manifest) =
        res.inspect_err(|_| _ = temporary.then(|| std::fs::remove_file(&file)))?;
    let replaces = list()
        .into_iter()
        .find(|(id, _)| *id == manifest.id)
        .map(|(id, p)| match p {
            Ok(p) => format!("{} {}", p.manifest.name, p.manifest.version)
                .trim()
                .to_owned(),
            Err(_) => id,
        });
    Ok(Package {
        file,
        temporary,
        root,
        manifest,
        replaces,
    })
}

/// The plugin folder inside a package and its manifest.
async fn read_manifest(file: &Path) -> anyhow::Result<(String, Manifest)> {
    let list = unzip(&["-Z1"], file, None)
        .await
        .context("Not a plugin package")?;
    let entry = manifest_entry(&String::from_utf8_lossy(&list))
        .context("The package has no plugin.json")?
        .to_owned();
    let manifest = parse(&unzip(&["-p"], file, Some(&entry)).await?)?;
    Ok((entry.trim_end_matches("plugin.json").to_owned(), manifest))
}

/// `plugin.json` at the package's top, or in a top folder (as in GitHub's source zips).
fn manifest_entry(list: &str) -> Option<&str> {
    let mut names = list.lines();
    names
        .clone()
        .find(|l| *l == "plugin.json")
        .or_else(|| names.find(|l| l.split('/').count() == 2 && l.ends_with("/plugin.json")))
}

async fn unzip(args: &[&str], file: &Path, entry: Option<&str>) -> anyhow::Result<Vec<u8>> {
    let out = tokio::process::Command::new("/usr/bin/unzip")
        .args(args)
        .arg(file)
        .args(entry)
        .output()
        .await
        .context("unzip")?;
    ensure!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(out.stdout)
}

/// Unpacks a package into `plugins/<id>`, replacing the plugin there. Returns the id. The caller switches it off:
/// an update is new code too.
pub async fn install(pkg: Package) -> anyhow::Result<String> {
    let id = pkg.manifest.id.clone();
    let staging = dir().join(format!(".install-{id}"));
    let res = async {
        _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging)?;
        // unzip drops `..` and absolute paths, so nothing lands outside `staging`.
        let out = tokio::process::Command::new("/usr/bin/unzip")
            .arg("-qo")
            .arg(&pkg.file)
            .arg("-d")
            .arg(&staging)
            .output()
            .await?;
        ensure!(
            out.status.success(),
            "Unpacking: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        let root = staging.join(&pkg.root);
        load(id.clone(), &root)?;
        let target = dir().join(&id);
        if target.exists() {
            std::fs::remove_dir_all(&target)?;
        }
        std::fs::rename(&root, &target)?;
        Ok(id)
    }
    .await;
    _ = std::fs::remove_dir_all(&staging);
    res
}

fn open_log(path: &Path) -> std::io::Result<std::fs::File> {
    let full = std::fs::metadata(path).is_ok_and(|m| m.len() > LOG_LIMIT);
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(!full)
        .truncate(full)
        .open(path)
}

fn timestamp() -> String {
    let tm = local_now();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// `TR_TORRENT_DIR` is the folder *holding* the torrent, so `$DIR/$NAME` is the data.
/// librqbit's output folder is that for single files, but already includes `NAME` for multi-file torrents.
pub fn torrent_dir(output: &Path, name: &str) -> PathBuf {
    let output: PathBuf = output.components().collect(); // drops a trailing slash
    match output.parent() {
        Some(parent) if output.file_name().is_some_and(|f| f == name) => parent.to_owned(),
        _ => output,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("torrie-{name}-{}", std::process::id()))
    }

    /// A plugin folder in `root` with `plugin.json` and an executable `run.sh`.
    fn make(root: &Path, id: &str, json: &str, script: &str, exec: bool) -> Result<Plugin, String> {
        let d = root.join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("plugin.json"), json).unwrap();
        std::fs::write(d.join("run.sh"), script).unwrap();
        std::fs::set_permissions(
            d.join("run.sh"),
            std::fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 }),
        )
        .unwrap();
        load(id.into(), &d).map_err(|e| format!("{e:#}"))
    }

    #[test]
    fn dir_holds_the_data() {
        assert_eq!(
            torrent_dir(Path::new("/dl/Show S01"), "Show S01"),
            Path::new("/dl")
        );
        assert_eq!(
            torrent_dir(Path::new("/dl/"), "movie.mkv").as_os_str(),
            "/dl"
        );
    }

    #[test]
    fn manifests_are_checked() {
        let root = temp("plugin-test");
        let make = |id: &str, json: &str, exec: bool| make(&root, id, json, "#!/bin/sh\n", exec);
        let ok = make(
            "ok",
            r#"{"name":"My Plugin!","hooks":{"torrent-done":"run.sh"},"timeout":99999}"#,
            true,
        )
        .unwrap();
        assert_eq!(
            (
                ok.manifest.timeout,
                ok.manifest.id.as_str(),
                ok.main.is_none()
            ),
            (MAX_TIMEOUT, "my-plugin", true)
        );
        let legacy = make(
            "legacy",
            r#"{"name":"A","main":"run.sh","events":["torrent-done"]}"#,
            true,
        )
        .unwrap();
        assert!(
            legacy.main.is_none() && legacy.hooks.contains_key(DONE),
            "old manifests' main is a hook"
        );
        let process = make(
            "process",
            r#"{"name":"A","main":"run.sh","contributes":{"panels":[{"id":"p","title":"P"}]}}"#,
            true,
        );
        assert!(process.unwrap().main.is_some());
        let bad = |id, json, exec| make(id, json, exec).unwrap_err();
        assert!(bad("noexec", r#"{"name":"A","main":"run.sh"}"#, false).contains("executable"));
        assert!(
            bad("event", r#"{"name":"A","hooks":{"boom":"run.sh"}}"#, true)
                .contains("unknown event")
        );
        assert!(bad("escape", r#"{"name":"A","main":"../ok/run.sh"}"#, true).contains("inside"));
        assert!(
            bad(
                "abs",
                r#"{"name":"A","hooks":{"torrent-done":"/bin/sh"}}"#,
                true
            )
            .contains("inside")
        );
        assert!(
            bad(
                "nomain",
                r#"{"name":"A","contributes":{"commands":[{"id":"c","title":"C"}]}}"#,
                true
            )
            .contains("need \"main\"")
        );
        assert!(bad("id", r#"{"id":"Bad Id","name":"A"}"#, true).contains("\"id\""));
        assert!(bad("kind", r#"{"name":"A","contributes":{"settings":[{"key":"k","title":"K","type":"color"}]}}"#, true).contains("color"));
        std::os::unix::fs::symlink("/bin/sh", root.join("ok/link")).unwrap();
        assert!(bad("ok", r#"{"name":"A","main":"link"}"#, true).contains("outside"));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn shipped_plugins_load() {
        let root = temp("plugin-seeded");
        for (id, json, file, script) in SEEDED {
            std::fs::create_dir_all(root.join(id)).unwrap();
            std::fs::write(root.join(id).join("plugin.json"), json).unwrap();
            std::fs::write(root.join(id).join(file), script).unwrap();
            std::fs::set_permissions(
                root.join(id).join(file),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
            assert_eq!(load(id.into(), &root.join(id)).unwrap().manifest.id, id);
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn notify_becomes_a_setting() {
        let root = temp("plugin-notify");
        let json = r#"{"name": "Notify", "hooks": {"torrent-done": "run.sh"}}"#;
        let on = || Settings {
            plugins_enabled: vec![NOTIFY.into(), "other".into()],
            ..Default::default()
        };
        let seeded = |r: &Path| std::fs::write(r.join(".seeded"), "notify\nshortcuts\n").unwrap();

        // Shipped and on: the setting takes over, once.
        make(&root, NOTIFY, json, NOTIFY_SH, true).unwrap();
        seeded(&root);
        let mut s = on();
        assert!(migrate_notify(&root, &mut s));
        assert!(s.notify_done && s.plugins_enabled == ["other"]);
        assert!(!root.join(NOTIFY).exists());
        assert_eq!(
            std::fs::read_to_string(root.join(".seeded")).unwrap(),
            "shortcuts\n"
        );
        assert!(!migrate_notify(&root, &mut s));

        // Deleted: stays off.
        seeded(&root);
        let mut s = on();
        assert!(migrate_notify(&root, &mut s) && !s.notify_done);

        // Edited: stays a plugin, and the setting goes off so it isn't doubled.
        make(&root, NOTIFY, json, "#!/bin/sh\nsay done\n", true).unwrap();
        seeded(&root);
        let mut s = on();
        assert!(migrate_notify(&root, &mut s) && !s.notify_done);
        assert!(root.join(NOTIFY).exists() && s.plugins_enabled.len() == 2);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn settings_reach_the_environment() {
        let root = temp("plugin-env");
        let json = r#"{"name":"A","contributes":{"settings":[
            {"key":"topic","title":"T","default":"x"},{"key":"loud-mode","title":"L","type":"boolean","default":false}]}}"#;
        let p = make(&root, "env", json, "#!/bin/sh\n", true).unwrap();
        let mut s = Settings::default();
        s.plugin_settings
            .entry("env".into())
            .or_default()
            .insert("topic".into(), json!("mine"));
        let env = setting_env(&p, &s);
        assert_eq!(
            env,
            [
                ("TORRIE_SETTING_TOPIC".into(), "mine".into()),
                ("TORRIE_SETTING_LOUD_MODE".into(), "false".into())
            ]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn views_skip_unknown_blocks() {
        let view: Vec<Block> = serde_json::from_value(
            json!([{"type":"field","label":"A","value":"1"},{"type":"chart","data":[]}]),
        )
        .unwrap();
        assert_eq!(
            view,
            [
                Block::Field {
                    label: "A".into(),
                    value: "1".into()
                },
                Block::Unknown
            ]
        );
    }

    #[test]
    fn packages_find_their_manifest() {
        assert_eq!(manifest_entry("run.sh\nplugin.json\n"), Some("plugin.json"));
        assert_eq!(
            manifest_entry("repo-main/\nrepo-main/plugin.json\n__MACOSX/repo-main/._plugin.json"),
            Some("repo-main/plugin.json")
        );
        assert_eq!(manifest_entry("a/b/plugin.json"), None);
    }

    #[tokio::test]
    async fn packages_open_from_files() {
        let root = temp("plugin-package");
        make(
            &root,
            "repo-main",
            r#"{"id":"ntfy","name":"Phone notification","version":"2.0"}"#,
            "#!/bin/sh\n",
            true,
        )
        .unwrap();
        let file = root.join("ntfy.torrieplugin");
        let zipped = std::process::Command::new("/usr/bin/zip")
            .current_dir(&root)
            .arg("-qr")
            .arg(&file)
            .arg("repo-main")
            .status();
        assert!(zipped.unwrap().success());
        let pkg = fetch(url::Url::from_file_path(&file).unwrap().to_string())
            .await
            .unwrap();
        assert_eq!(
            (pkg.root.as_str(), pkg.manifest.id.as_str(), pkg.temporary),
            ("repo-main/", "ntfy", false)
        );
        assert!(
            is_package("torrie://install?url=https://x/y.torrieplugin")
                && !is_package("file:///a.torrent")
        );
        assert!(
            fetch("torrie://install?url=http://x/y".into())
                .await
                .err()
                .unwrap()
                .to_string()
                .contains("https")
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The plugin protocol, both ways: the plugin calls Torrie's API, then answers Torrie's request.
    #[tokio::test]
    async fn plugin_processes_talk_json_rpc() {
        let root = temp("plugin-proto");
        let script = r#"#!/bin/sh
echo '{"jsonrpc":"2.0","id":"a","method":"torrents.list"}'
echo 'stray print'
while read -r line; do
  case $line in *'"method"'*) ;; *) continue ;; esac
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":[{\"type\":\"text\",\"text\":\"$TORRIE_SETTING_GREETING\"}]}"
done
"#;
        let json = r#"{"name":"Proto","main":"run.sh","contributes":{"settings":[{"key":"greeting","title":"G","default":"hi"}]}}"#;
        let p = make(&root, "torrie-test-proto", json, script, true).unwrap();
        let calls = Arc::new(Mutex::new(vec![]));
        let seen = calls.clone();
        let api: Api = Arc::new(move |m, _| {
            seen.lock().unwrap().push(m.to_owned());
            Ok(json!([]))
        });
        start(p, &Settings::default(), api).unwrap();
        let id = "torrie-test-proto".to_string();
        let view = request(
            id.clone(),
            "panel.render",
            json!({"panel":"p"}),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_value::<Vec<Block>>(view).unwrap(),
            [Block::Text { text: "hi".into() }]
        );
        assert_eq!(*calls.lock().unwrap(), ["torrents.list"]);
        assert!(running(&id));
        PROCESSES.lock().unwrap().remove(&id);
        assert!(
            request(
                id.clone(),
                "panel.render",
                json!({}),
                Duration::from_secs(1)
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("isn't running")
        );
        std::fs::remove_dir_all(&root).unwrap();
        _ = std::fs::remove_file(log_path(&id));
    }
}
