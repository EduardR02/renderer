# Renderer

A desktop music client for Windows that plays Spotify through your own Premium
account.

I wrote it because the official desktop app kept my CPU busy in a way a music
player has no business doing. A music player runs all day in the background, so
that cost is paid continuously — it isn't the kind of thing you stop noticing.
Mainstream clients have to serve a very broad set of goals; this one doesn't. It
only has to suit one person, which means it can leave out the parts I never use
and spend the effort on playback quality, responsiveness and the interface
instead.

<p align="center">
  <img src="docs/library.png" alt="A playlist open in Renderer" width="900">
</p>

## Not affiliated with Spotify

This is an unofficial third-party client. It has no connection to Spotify AB and
is not authorized or endorsed by them. "Spotify" is a trademark of Spotify AB,
used here only to say what this program connects to.

Running an unofficial client very likely breaks Spotify's Terms of Use. This one
authenticates as a desktop client and calls internal endpoints that aren't part
of the public Web API, so using it puts your account at risk of suspension. It
needs your own Premium account and gives you no access to anything you aren't
already paying for. Your call, your risk — and I'm not a lawyer, so this isn't
legal advice.

No audio, metadata or artwork is distributed with this repository.

## How it's built

Two processes. The playback engine in `engine/` wraps
[librespot](https://github.com/librespot-org/librespot) and owns everything to do
with sound; the Tauri 2 shell in `src-tauri/` supervises that process, holds the
caches, and serves a Svelte 5 frontend from `src/`. Audio living in its own
process is the point of the split: nothing the interface does can interrupt
playback, and if the engine ever dies the shell restarts it and restores the
queue where it left off.

The part I'd point at first is the audio path. librespot decodes; everything
after that is ours, and it had to be. Spotify's audio arrives at 44.1 kHz and
Windows almost always runs its output at 48, so every sample gets resampled on
the way out. The stock rodio backend does that with linear interpolation, and
rebuilds its converter mid-stream — each rebuild leaking a fraction of a frame.
With the packet sizes Spotify's Vorbis actually produces that measured **+0.49%
on rate**, offline and again on real hardware: the whole library playing very
slightly fast, and slightly sharp, forever.

Fixing it took two things, and only doing one wasn't enough. First, a
windowed-sinc polyphase resampler that tracks position as an exact rational, so
the output frame count is determined to the frame no matter where packet
boundaries fall. Second, having the sink report the device's rate rather than
44.1 kHz, which sends rodio's own converters down their pass-through branch so
they stop resampling on top of the result. Reconstruction error now measures
about −116 dB at 10 kHz, under 16-bit resolution and well beneath what the codec
itself leaves behind. Both figures are pinned by tests, because this is the kind
of error that ships easily and hides in plain hearing.

## What it does that the official client doesn't

**Per-track edit regions.** Cut a section out of a song, or loop an exact range,
and it plays that way from then on. Enabled per playlist, off by default, edited
in a waveform view.

**Pitch-preserving speed**, from 0.5× to 2× in 0.05 steps, using WSOLA. It's
strictly bypassed at 1.0× — the sample pipeline isn't merely set to a no-op, it
isn't constructed at all — so the ordinary listening path stays bit-exact.

**Local listening history**, one row per play, with filtering and sorting. It's
recorded here and stays here.

**Downloaded-track marks**, showing which songs are already in the local audio
cache and will cost no network to play.

Beyond that it's a normal client: playlists, albums, artist pages with bios and
monthly listeners, search, queue, credits, radio.

### Canvas

Canvas — the short looping video some tracks have — is gated **per account on
Spotify's servers**, not locally. Their backend returns nothing at all while that
account preference is off, for every client including the official one, so the
toggle in this app can only ever turn Canvas off. If it isn't showing, enable
video in Spotify's own settings first. This cost me an evening; it's written down
so it doesn't cost you one.

<p align="center">
  <img src="docs/now-playing.png" alt="The now playing panel with a Canvas video" width="900">
</p>

## Building

You'll need a [Rust toolchain](https://rustup.rs) and [Bun](https://bun.sh).
Nothing else — `cargo` fetches and compiles every dependency itself, which is why
the build directory ends up several GB. It's all generated; `cargo clean` takes
it back.

```bash
bun install
bun run build:engine
bun tauri build
```

For development, `bun tauri dev`. To run the checks:

```bash
cargo test -p renderer-engine
cargo test -p renderer
bun run build
```

First launch opens a Spotify login in your browser. Credentials go to Spotify,
never through this app; what's stored locally is the token it hands back, under
`%LOCALAPPDATA%\SpotifyRenderer` along with the audio cache, covers and history.

## Layout

| Path         | What's in it                                                 |
| ------------ | ------------------------------------------------------------ |
| `engine/`    | Playback engine: librespot, audio pipeline, browse, history   |
| `src-tauri/` | Tauri shell: engine supervision, caches, commands             |
| `src/`       | Svelte 5 frontend                                             |
| `dev/`       | Scratch harnesses, not part of the build                      |

`AGENTS.md` describes the conventions the code is written to, and is the better
starting point than this file if you intend to change anything.

## Scope

This is a personal project that happens to be public. It's built for one
person's machine and one person's taste, I make no promises about it working on
yours, and I'm not looking to take it in directions I wouldn't use myself. Fork
it freely — that's what the license is for.

## Thanks

To [librespot](https://github.com/librespot-org/librespot), without which none of
this would exist. It's used unmodified, as a normal dependency; everything custom
here is built on the extension points it already exposes.

## License

MIT, see [LICENSE](LICENSE). That covers the code in this repository and nothing
belonging to Spotify AB.
