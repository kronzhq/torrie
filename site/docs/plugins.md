# Plugins

Plugins extend Torrie. They can add commands to the menu bar, tabs to the Inspector, and options to Settings, and they can run code when transfers are added or finish.

| Term | Meaning |
|---|---|
| **Plugin** | A folder in `~/Library/Application Support/Torrie/plugins/`. It's what you install, switch on and share. |
| **Manifest** | The plugin's `plugin.json`: what the plugin is and what it adds. |
| **Event** | Something that happened to a transfer: `torrent-added`, `torrent-done` or `torrent-done-seeding`. |
| **Hook** | An executable run once for an event. Hooks are compatible with Transmission scripts. |
| **Plugin process** | The plugin's `main` executable. Torrie keeps it running while the plugin is on. |
| **Plugin protocol** | How a plugin process and Torrie talk: [JSON-RPC 2.0](https://www.jsonrpc.org/specification), one message per line on the process's stdin and stdout. |
| **Contribution** | Something a plugin adds to Torrie's interface, declared under `contributes`: a command, a panel or a setting. |
| **Command** | An item in the **Plugins** menu, run by the plugin process. |
| **Panel** | An Inspector tab that shows the plugin process's view of the selected transfer. |
| **View** | What a panel shows: a list of blocks (headings, text, fields, buttons). |
| **Setting** | A value you set in Settings → Plugins. Hooks and the plugin process read it from their environment. |
| **Package** | A plugin folder zipped into a `.torrieplugin` file. |
| **Install link** | `torrie://install?url=https://…`. Opening it installs the package at that URL. |

Plugins start **off** when they're installed, and again when they're updated. Switch them on in Settings → Plugins (Plugins → Manage Plugins…).

## Manifest

```json
{
  "id": "media-organizer",
  "name": "Media Organizer",
  "version": "1.0.0",
  "description": "Renames finished movies for Plex",
  "author": "you",
  "hooks": { "torrent-done": "on-done.sh" },
  "timeout": 300,
  "main": "plugin.py",
  "contributes": {
    "commands": [{ "id": "rename", "title": "Rename for Plex", "torrent": true }],
    "panels": [{ "id": "media", "title": "Media" }],
    "settings": [
      { "key": "library", "title": "Library", "type": "folder", "description": "Where renamed files go" },
      { "key": "dry_run", "title": "Dry run", "type": "boolean", "default": true }
    ]
  }
}
```

| Key | |
|---|---|
| `name` | Required. `version`, `description` and `author` are optional; Settings and the install prompt show them. |
| `id` | The folder a package installs to: a-z, 0-9 and dashes. Defaults to `name` in that form. A plugin folder you make by hand is identified by its folder name instead. |
| `hooks` | Event → executable. |
| `timeout` | Seconds a hook may run (default 60, max 3600). |
| `main` | The plugin process's executable. Commands and panels need one. |
| `contributes.commands` | `id`, `title`, and `"torrent": true` for commands that act on the selected transfer. |
| `contributes.panels` | `id`, `title`. |
| `contributes.settings` | `key`, `title`, `description`, `type` (`string`, the default, or `boolean` or `folder`), and `default`. |

Paths are relative to the plugin folder.

## Hooks

| Event | Runs when |
|---|---|
| `torrent-added` | a torrent is added (magnets once their metadata arrives) |
| `torrent-done` | a download finishes |
| `torrent-done-seeding` | the ratio limit stops a torrent seeding |

A hook runs inside its plugin's folder with the event name as its argument. It gets Transmission's [script variables](https://github.com/transmission/transmission/blob/main/docs/Scripts.md), so Transmission scripts work unchanged:

| Variable | |
|---|---|
| `TR_TORRENT_NAME`, `TR_TORRENT_DIR` | the data is at `$TR_TORRENT_DIR/$TR_TORRENT_NAME` |
| `TR_TORRENT_ID`, `TR_TORRENT_HASH` | identify the torrent (also over RPC) |
| `TR_TORRENT_BYTES_DOWNLOADED`, `TR_TORRENT_TRACKERS` (comma-separated), `TR_TORRENT_LABELS`, `TR_TORRENT_PRIORITY` | |
| `TR_APP_VERSION`, `TR_TIME_LOCALTIME` | |
| `TORRIE_EVENT` | the event |
| `TORRIE_RPC_URL` | Torrie's API, set when Remote control is on (see [Remote control](remote-control.md)) |
| `TORRIE_SETTING_<KEY>` | each of the plugin's settings, with its key in capitals and other characters as `_` (`dry_run` → `TORRIE_SETTING_DRY_RUN`); booleans are `true` or `false` |

## Plugin process

When a plugin with a `main` is on, Torrie starts that executable at launch, or when you switch the plugin on. It runs inside the plugin's folder with `TORRIE_SETTING_<KEY>` variables, and speaks the plugin protocol on stdin and stdout. Its stderr, and anything on stdout that isn't JSON, goes to its log. Changing one of its settings restarts it, and so does Reload in Settings → Plugins. When the plugin is switched off, it gets SIGTERM, then SIGKILL 5 seconds later. It should also exit when stdin closes, which happens when Torrie quits. If it exits on its own, Settings shows why, and Reload starts it again.

Torrie sends it:

| Method | Kind | Params | Answer |
|---|---|---|---|
| `event` | notification | `{ "event": "torrent-done", "torrent": {…} }` | none |
| `command.run` | request, 60 s | `{ "command": "rename", "torrent": {…} }` (`torrent` is `null` unless the command has `"torrent": true`) | anything; an error shows in the window |
| `panel.render` | request, 5 s | `{ "panel": "media", "torrent": {…} }` | a view |

`torrent` is a transfer as `torrents.list` returns it. Every plugin process receives every event. A panel is rendered once a second while it's on show, so answer quickly; it's also rendered again after one of its buttons runs a command.

The plugin process can call Torrie's API: the same methods, params and results as [remote control](remote-control.md) (`torrents.list`, `torrents.add`, `torrents.pause`, `torrents.resume`, `torrents.remove`, `settings.get`, `settings.set`), sent over stdout and answered on stdin. Remote control doesn't need to be on.

A view is a JSON array of blocks. Unknown block types are skipped:

| Block | |
|---|---|
| `{ "type": "heading", "text": "…" }` | |
| `{ "type": "text", "text": "…" }` | |
| `{ "type": "field", "label": "…", "value": "…" }` | a row, like those in the Overview tab |
| `{ "type": "button", "label": "…", "command": "…" }` | runs one of the plugin's commands on this transfer (it needn't be in the Plugins menu) |

`plugin.py` for the manifest above, standard library only:

```python
#!/usr/bin/env python3
import json, os, sys

def send(msg):
    print(json.dumps({"jsonrpc": "2.0", **msg}), flush=True)

library = os.environ.get("TORRIE_SETTING_LIBRARY") or "Not set"
for line in sys.stdin:  # ends when Torrie quits
    msg = json.loads(line)
    method, params = msg.get("method"), msg.get("params", {})
    if method == "panel.render":
        t = params["torrent"]
        send({"id": msg["id"], "result": [
            {"type": "field", "label": "Library", "value": library},
            {"type": "field", "label": "Ratio", "value": f"{t['ratio']:.2f}"},
            {"type": "button", "label": "Rename for Plex", "command": "rename"},
        ]})
    elif method == "command.run":
        # Rename the files at params["torrent"]["path"] here.
        send({"id": msg["id"], "result": None})
    elif method and "id" in msg:
        send({"id": msg["id"], "error": {"code": -32601, "message": f"unknown method {method}"}})
```

To call Torrie, send for example `{"jsonrpc": "2.0", "id": "p1", "method": "torrents.pause", "params": {"ids": "<hash>"}}`. The answer arrives on stdin with the same `id` and no `method`.

## Packages and install links

To share a plugin, zip its folder into a package:

```sh
cd media-organizer && zip -r ../media-organizer.torrieplugin .
```

Double-clicking a `.torrieplugin` file opens it in Torrie. To install from the web, publish the file and share an install link, on a web page or in a terminal:

```sh
open 'torrie://install?url=https://github.com/you/media-organizer/releases/download/v1.0.0/media-organizer.torrieplugin'
```

The `url` must be https, and the package at most 50 MB. GitHub's source zips work too, since `plugin.json` may sit in the zip's top folder. Zip keeps executables executable. Torrie asks before installing and shows the plugin's name, author and description. Installing a package replaces any installed plugin with the same `id`, including files kept in its folder, and leaves it off.

## Guard rails

- Every executable must be inside the plugin folder: no absolute paths, `..` or symlinks out.
- Hooks and plugin processes get a clean environment (only `PATH`, `HOME`, `USER`, `LOGNAME`, `LANG`, `TMPDIR` plus the variables above), each in its own process group.
- Hooks get no stdin. After `timeout` seconds, a hook and everything it started are killed. At most 4 hooks run at once, and Torrie never waits on them.
- Plugins can't change `plugins_enabled`, `plugin_settings` or remote control through the API.
- Output goes to `plugins/<folder>.log`, which starts over past 1 MB. A plugin process's log starts over only when the process restarts.
- Plugins run as you, with your files and network, so only switch on ones you trust.

## Bundled plugins

Torrie ships two plugins. `notify` posts a "Download Complete" notification; it's on by default. Copy it to start your own. `shortcuts` runs a Shortcut from the Shortcuts app on each finished download, with the download's file or folder as its input, so a Shortcut can move it to a NAS, add it to the TV app or message someone. It's off until you name a Shortcut in Settings → Plugins and switch it on.

## Recipes

Each recipe is a folder in `plugins/` with these two files. `chmod +x` the script, then click Reload and switch the plugin on in Settings → Plugins. `$TR_TORRENT_DIR/$TR_TORRENT_NAME` is the downloaded file or folder.

**Copy finished downloads to another drive.** Copies rather than moves, so the torrent keeps seeding.

```json
{ "name": "Copy to NAS", "hooks": { "torrent-done": "run.sh" }, "timeout": 3600 }
```
```sh
#!/bin/sh
cp -R "$TR_TORRENT_DIR/$TR_TORRENT_NAME" /Volumes/NAS/Downloads/
```

**Notify your phone** through [ntfy](https://ntfy.sh): install its app, subscribe to a topic only you know, and enter it in Settings → Plugins.

```json
{
  "name": "Phone notification",
  "hooks": { "torrent-done": "run.sh" },
  "contributes": { "settings": [{ "key": "topic", "title": "ntfy topic", "description": "A topic only you know" }] }
}
```
```sh
#!/bin/sh
curl -s -d "Downloaded: $TR_TORRENT_NAME" "https://ntfy.sh/$TORRIE_SETTING_TOPIC"
```

**Unpack RAR archives** with `unar` (`brew install unar`). Multi-part sets are opened from their first part.

```json
{ "name": "Unrar", "hooks": { "torrent-done": "run.sh" }, "timeout": 1800 }
```
```sh
#!/bin/sh
find "$TR_TORRENT_DIR/$TR_TORRENT_NAME" \( -name '*.rar' ! -name '*.part*.rar' -o -name '*.part1.rar' -o -name '*.part01.rar' \) \
  -execdir unar -quiet -force-overwrite {} \;
```

**Keep a download history** in a CSV file.

```json
{ "name": "History", "hooks": { "torrent-done": "run.sh" } }
```
```sh
#!/bin/sh
printf '%s,"%s",%s\n' "$TR_TIME_LOCALTIME" "$TR_TORRENT_NAME" "$TR_TORRENT_BYTES_DOWNLOADED" >> "$HOME/Downloads/Torrie history.csv"
```

**Remove torrents once they've seeded enough**, keeping the files. Needs Settings → General → Stop seeding at ratio, and Settings → Network → Remote control.

```json
{ "name": "Clean up", "hooks": { "torrent-done-seeding": "run.sh" } }
```
```sh
#!/bin/sh
curl -s "$TORRIE_RPC_URL" -H 'Content-Type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"torrents.remove\",\"params\":{\"ids\":\"$TR_TORRENT_HASH\"}}"
```
