#!/bin/sh
# Torrie's bundled Shortcuts plugin. Hands the finished download (its file or folder) to a Shortcut, which can
# move it, import it into TV or Music, message someone: anything Shortcuts does.
[ -n "$TORRIE_SETTING_SHORTCUT" ] || { echo "Name a Shortcut in Settings → Plugins"; exit 1; }
exec shortcuts run "$TORRIE_SETTING_SHORTCUT" --input-path "$TR_TORRENT_DIR/$TR_TORRENT_NAME"
