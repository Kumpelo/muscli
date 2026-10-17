# Desktop widget: local protocol

[Playback and widget guide](../playback-and-widget.es.md)

The Omarchy widget in `desktop/quickshell` displays the state of the running
muscli instance. It reads queue snapshots from muscli's control socket and uses
MPRIS for transport and volume controls. It does not open the database or start
another audio backend.

## Status request

The per-user control socket accepts a newline-terminated `status v1` request.
The CLI exposes it as:

```bash
muscli remote status --json
```

The command exits with an error when no player is running. It does not start
playback. The main loop publishes a snapshot; the socket worker serves that
copy without querying SQLite or waiting for the next render.

The snapshot contains:

- Schema version, availability and queue revision.
- Playback status, position, duration, volume and album/song context.
- Logical current track and the currently presented track during MIX.
- Track title, artist, local cover URI and MIX progress.
- Up to ten upcoming queue entries, including duplicates.

Occurrence indices distinguish repeated tracks within a queue revision.
Consumers must check the schema version before reading the payload.

## Connection limits

On Unix, the runtime directory uses mode `0700` and the socket uses `0600`.
Requests are limited to 256 bytes; serialized snapshots must be smaller than
64 KiB. The server services up to eight nonblocking clients with a one-second
connection deadline. The CLI also bounds connection and response waits.

The status endpoint accepts a fixed request, not paths, shell commands or
arbitrary database queries.

## Quickshell frontend

The frontend targets Quickshell 0.3.2 on Omarchy. The launcher accepts screen,
corner, margin, width and muscli-binary options. Its Wayland panel uses the
active Omarchy palette and displays up to four upcoming tracks.

While visible, it requests status once per second and interpolates playback
position locally. It retains the previous cover while loading the next one.
Missing or stale status produces a disconnected view; missing MPRIS disables
controls. Transport and volume commands target muscli's MPRIS player.

The optional integration test in `tests/widget.rs` exercises controls and
reconnection using a private D-Bus session and synthetic library metadata.
