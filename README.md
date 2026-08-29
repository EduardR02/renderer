# Renderer

A personal desktop music client for Windows, built because the official Spotify
app used more CPU than a music player has any business using.

It is a Tauri 2 + Svelte 5 interface over a separate Rust playback engine that
wraps [librespot](https://github.com/librespot-org/librespot). Playback runs in
its own process, so a stall in the UI cannot interrupt audio.

## Not affiliated with Spotify

This is an unofficial third-party client. It is not affiliated with, authorized
by, endorsed by, or connected to Spotify AB in any way. "Spotify" is a trademark
of Spotify AB and is used here only to describe what this program connects to.

**Using an unofficial client very likely violates Spotify's Terms of Use.** This
program authenticates as a desktop client and calls internal endpoints that are
not part of Spotify's public Web API. Running it puts your account at risk of
suspension or termination. It requires your own Spotify Premium account, and it
neither provides nor circumvents access to any content you are not already
entitled to. Use it at your own risk. I am not a lawyer and this is not legal
advice.

No audio, metadata, artwork, or other Spotify content is distributed with this
repository.

## What it does

- Gapless playback at 320 kbps, resampled to the device rate on the player
  thread rather than by the output layer — measured at about −116 dB
  reconstruction error, well under the codec floor
- Pitch-preserving playback speed (WSOLA), strictly bypassed at 1.0×
- Per-track edit regions: cut a section, or loop an exact range, enabled per
  playlist
- Local listening history, virtualized, filterable and sortable
- Artist pages with bio, monthly listeners, top cities and discography
- Spotify Canvas in the now-playing panel, when the account allows it
- Marks which tracks are already in the local audio cache, live

### A note on Canvas

Canvas is gated **per account, on Spotify's servers**. Their backend returns
nothing for every client while that account preference is off, so this app's
toggle can only ever turn Canvas off, never on. Enable video in Spotify's own
settings first.

## Building

Requires a recent Rust toolchain and [Bun](https://bun.sh).

```bash
bun install
bun run build:engine
bun tauri build
```

For development:

```bash
bun run build:engine
bun tauri dev
```

Checks:

```bash
cargo test -p renderer-engine
cargo test -p renderer
bun run build
```

## Layout

| Path         | What lives there                                          |
| ------------ | --------------------------------------------------------- |
| `engine/`    | Playback engine: librespot, audio pipeline, browse, history |
| `src-tauri/` | Tauri shell: engine supervision, caches, commands           |
| `src/`       | Svelte 5 frontend                                           |

`AGENTS.md` documents the conventions the code is written to.

## License

MIT — see [LICENSE](LICENSE). This covers the code in this repository only, and
grants no rights to anything belonging to Spotify AB.
