# Contributing to Torrie

Bug reports, fixes and features are welcome at [github.com/kronzhq/torrie](https://github.com/kronzhq/torrie).

## Reporting a bug

Open an issue with:

- your macOS version
- the Torrie version (Get Info on `Torrie.app`)
- what you did, what you expected and what happened

If a plugin is involved, attach its log from `~/Library/Application Support/Torrie/plugins/<folder>.log`.

## Making a change

1. For anything bigger than a small fix, open an issue first so we can agree on the approach.
2. Fork the repo and branch from `master`.
3. Build and check from the repo root:
   ```sh
   cargo run
   cargo test
   cargo fmt --all
   cargo clippy --all-targets   # no new warnings
   ```
4. Keep the change focused. Add a test when you change logic, and update the docs in `site/docs/` if behaviour or settings change, then rebuild the website's copy with `uv run site/build.py`.
5. Open a pull request that says what changed and why.

Many ideas don't need a change to Torrie at all and can be a [plugin](site/docs/plugins.md) instead.

## Layout

The repo is a Cargo workspace. The app is in `apps/torrie/`, and the logo artwork is in `brand/`.

| Path | |
|---|---|
| `src/main.rs` | App setup, menus, key bindings, open-URL handling |
| `src/engine.rs` | librqbit session, settings, add/pause/remove, file selection, peers, trackers, ratio limit |
| `src/format.rs` | Transfer snapshots and display strings |
| `src/window.rs` | Main window: sidebar, list, Inspector |
| `src/add.rs` | Add Torrent window |
| `src/prefs.rs` | Settings window |
| `src/popover.rs` | Menu bar panel |
| `src/macos.rs` | Menu bar item and opening at login |
| `src/plugins.rs` | Plugin manifests, hooks, processes, protocol and packages |
| `src/rpc.rs` | Remote control server and Torrie's JSON-RPC API (`openrpc.json`) |
| `src/rpc/transmission.rs` | Transmission protocol |
| `src/stream.rs` | Watch while downloading: a localhost HTTP server over librqbit's file streams |
| `src/update.rs` | In-app updates |
| `src/theme.rs` | Palette, bundled font and icons, shared controls |
| `macos/Info.plist` | `.torrent`, `magnet:`, `.torrieplugin` and `torrie:` registrations |
| `scripts/` | Bundle, DMG and release scripts |

## License

Torrie is MIT licensed, © Kronz. By submitting a contribution, you agree that it's licensed under the same [MIT License](LICENSE).
