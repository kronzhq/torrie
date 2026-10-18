# Remote control

Settings → Network → **Remote control** (off by default) serves a web remote, Torrie's own API and the Transmission protocol, all on one port, `9091` unless you change it beside the switch. It listens on this Mac only unless **Allow other devices** is on. Restart Torrie after switching either on or changing the port.

| | |
|---|---|
| Web remote | `http://<Mac's IP>:9091`, or `http://localhost:9091` on this Mac |
| Torrie API | `/torrie/rpc`, JSON-RPC 2.0 |
| Transmission | `/transmission/rpc`, rpc-version 17 |

## Web remote

Open `http://<this Mac's IP>:9091` in any browser, or `http://localhost:9091` on this Mac. You can add magnet links, URLs and `.torrent` files (drop them on the page), filter the list, and pause, resume or remove transfers. From another device, the browser asks for the password once (any username) and remembers it for a year.

## Security

- Connections from other machines need HTTP Basic auth: any username, and the password shown under Allow other devices (like `abcde-fghij`: 10 characters plus the dash, 50 bits). **New** replaces it at once and signs every browser out.
- Local connections are judged by their address, with a localhost `Host` header required against DNS rebinding.
- It's plain HTTP, so the password travels readable on the network. Use it on networks you trust, or over a VPN.
- API requests need `Content-Type: application/json`, and either no `Origin` header or Torrie's own, which keeps other web pages out.
- `settings.json` is saved readable by you only.

## Torrie API

[JSON-RPC 2.0](https://www.jsonrpc.org/specification) at `http://127.0.0.1:9091/torrie/rpc`, described in [`openrpc.json`](../../apps/torrie/openrpc.json), which `rpc.discover` also returns. Torrents are named by `ids`: an id, an info hash, or a list of them. Hooks get its URL as `TORRIE_RPC_URL`; plugin processes call the same methods directly.

| Method | |
|---|---|
| `torrents.list` | Every torrent, or those in `ids`. Returns [torrent objects](#torrent-object). |
| `torrents.add` | `link` (magnet, http(s) URL of a `.torrent`, or a local path) or `torrent` (base64 `.torrent`); optional `dir` (absolute folder) and `paused`. Returns `{ name, hash, id, duplicate }`. Finishes in the background, so a new torrent's `id` is `null`; one already added comes back with its `id` and `duplicate: true`. `hash` is empty for an http(s) URL until it's fetched. |
| `torrents.pause` | `ids` |
| `torrents.resume` | `ids` |
| `torrents.remove` | `ids`, optional `delete_files` |
| `settings.get` | Every setting, as in `settings.json`. |
| `settings.set` | Changes the settings given and returns them all. Plugin, remote control and appearance settings can only change in the Settings window. |
| `rpc.discover` | The OpenRPC document. |

```sh
curl -s localhost:9091/torrie/rpc -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"torrents.add","params":{"link":"magnet:?xt=urn:btih:...","dir":"/Volumes/Media/TV"}}'
```

## Torrent object

What `torrents.list` returns, and what plugin processes receive as `torrent`.

| Field | |
|---|---|
| `id`, `hash` | Integer id and info hash |
| `name` | String |
| `state` | `checking`, `downloading`, `seeding`, `paused` or `error` |
| `error` | String or `null` |
| `progress` | 0 to 1 |
| `size`, `downloaded` | Bytes |
| `uploaded` | Bytes, ever, kept across pauses and relaunches |
| `ratio` | `uploaded` ÷ `size` |
| `download_rate`, `upload_rate` | Bytes per second |
| `eta` | Seconds, or `null` while nothing is coming in |
| `peers` | Integer |
| `path` | The downloaded file or folder |

## Transmission protocol

The Transmission [RPC protocol](https://github.com/transmission/transmission/blob/main/docs/rpc-spec.md) (rpc-version 17) at `http://127.0.0.1:9091/transmission/rpc`, so anything built for that protocol can drive Torrie: media managers, `transmission-remote`, desktop and phone remotes. It needs the protocol's `X-Transmission-Session-Id` handshake.

| | |
|---|---|
| Supported | `session-get`, `session-set`, `session-stats`, `torrent-get`, `torrent-add` (with `download-dir` and `paused`), `torrent-start`, `torrent-start-now`, `torrent-stop`, `torrent-verify`, `torrent-reannounce`, `torrent-remove` |
| Accepted, ignored | `torrent-set`, `queue-move-*` |
| Magnets | Listed while fetching metadata (status downloading, `metadataPercentComplete` 0), so a grab shows up at once. |

### Adding Torrie as a download client

In an app that manages downloads, add a download client of type **Transmission** with URL base `/transmission/` and port `9091`:

- On this Mac: host `localhost`, no username or password.
- In Docker or on another machine: switch on Allow other devices; host `host.docker.internal` (Docker) or this Mac's IP; any username and the password.

Phone and desktop remotes work the same way: this Mac's IP, port 9091, any username and the password. macOS may ask once whether Torrie may accept incoming connections.

```sh
transmission-remote 9091 -l
```
