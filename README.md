# Renderer

A music-first Spotify client for Windows and macOS 14+. It keeps the useful
parts of a desktop player—library, discovery, queue, and playback—without
turning the music view into a podcast-video feed or a home page full of
promotions. Audio podcasts are browsable; video podcasts are not played.

I built it because the official desktop client used more CPU than I wanted
from an app that stays open all day. On my Ryzen 9 5950X, starting a song
could spin the fans loudly enough to distract me through open-back headphones.
The aim here is a calm interface and predictable resource use, not a
universal replacement for every Spotify feature.

<p align="center">
  <img src="docs/library.png" alt="Playlist with album artwork in the Now Playing sidebar" width="900">
  <br><br>
  <img src="docs/now-playing.png" alt="Playlist with video Canvas in the Now Playing sidebar" width="900">
</p>

## Install

Windows: grab the NSIS setup from [Releases](../../releases) and run it. You need
a Spotify Premium account; the app opens a Spotify login in your browser on
first launch.

macOS 14+ on Apple Silicon: download `renderer-macos-aarch64.zip` from
[Releases](../../releases), extract `renderer.app`, and open it. You can
also [build it yourself](#building). The Mac ZIP is ad-hoc signed, not notarized;
Gatekeeper may require manual approval. Only bypass Gatekeeper for builds you
trust.

## Not affiliated with Spotify

This is an unofficial client, with no connection to Spotify AB. "Spotify" is
their trademark and is used here only to say what this connects to.

Running an unofficial client very likely breaks Spotify's Terms of Use. This one
authenticates as a desktop client and uses internal endpoints that aren't part of
the public Web API, so it can get your account suspended. It needs your own
Premium account and gives you nothing you aren't already paying for. Your call.
No audio, metadata or artwork ships with this repository.

## Features

Music comes first: playlists, albums, artist pages, search, queue, credits,
radio, and audio podcasts with shows and episodes you can browse and play.
Artist pages have discographies, popular tracks, bios, monthly listeners and
top cities; the interface has no merch, concert tickets, AI DJ, or promotional
home feed. Podcast video and audiobooks are not played.

Music plays at 320 kbps with gapless transitions. Media keys work when the app isn't focused, and
it shows up in Windows Quick Settings and on the lock screen. Played tracks are
cached, so replaying them uses no network. It can launch at login, minimized if
you want. Playlists can be created, renamed, deleted and reordered, and tracks
added or removed by drag and drop or in bulk by rules — artist, album, title,
length — with a preview of exactly which entries go. Settings has an audio cache
size limit and a volume normalisation toggle.

The sidebar preserves playlist folders and lets you pin or unpin playlists,
including Liked Songs. Pins stay local to each account. User profiles show
public playlists, and playlist owners link to their profiles using display
names when Spotify supplies them.

Some extra things I added because we control playback here:

- Paste a shared Spotify link — song, album, artist, playlist, show, episode,
  or user — into search and open it here instead of the web player.
- Cut a section out of a song, or loop an exact range. Set per playlist, edited
  in a waveform view.
- Playback speed from 0.5× to 2×, pitch preserving.
- Listening history, kept locally.
- A mark on tracks that are already in the local audio cache.

Canvas works, but only if you have it enabled in the real Spotify app — it's
an account setting on their servers, not a local one, and their backend returns
nothing at all while it's off (in that case you still get the album cover of course).

## Limitations

This isn't meant to replace the Spotify app. It's a daily player, with enough
discovery in it that you don't have to leave for that, but there will be things
you occasionally need the real app for.

Windows and macOS 14 or later only, and you need Spotify Premium. The frontend
is shared, but macOS WebKit and Windows WebView2 can render the same CSS
differently; check changed layouts on both. The macOS build has to be verified
on a Mac; CI can compile and test it, but cannot verify audio devices, window
appearance or GPU rendering on your machine.

The app watches the system default audio output. Switching outputs rebuilds
the player against the new device and resumes the current track at its previous
position; unplugged or stalled outputs use the same recovery path. Output
handover still needs a real-device check on each OS.

No lossless. Spotify has it and the official app plays it, but this client isn't
offered it — the track metadata we get back lists AAC 24 and Ogg Vorbis
96/160/320 and no FLAC. It's gated on presenting as a client they serve it to,
which would take far heavier reverse engineering than anything else here, if
it's reachable at all.

It doesn't matter much in practice. 320 kbps Vorbis and lossless aren't
something people reliably pick apart in a blind ABX test. There's a difference
on paper, but not at a level that affects listening.

Library editing and controlling other Spotify devices are optional. With only
the playback sign-in, Liked Songs and followed artists remain readable; saving
songs, following artists or users, and the remote-device picker require
connecting your own Spotify Developer Mode app in Settings. This is a second
authorization for the **same** Spotify account, not another account. Create
the app with your Premium account, register
`http://127.0.0.1:5589/personal-api/callback`, and paste its **Client ID** into
Settings, never its Client Secret or a bearer token. Saved podcasts are also
available with this optional authorization.

The device picker is off by default and makes no background device requests.
It transfers an existing **remote Spotify session**, without moving Renderer's
local song or queue. Local playback still follows your system default audio
output; its controls remain independent of the remote session.

## Building

You need [Rust](https://rustup.rs) and [Bun](https://bun.sh). Build on the OS
you intend to run (Windows for NSIS, macOS for `.app`); Cargo compiles every
dependency from source, so the build directory ends up several GB. On macOS,
install Xcode Command Line Tools (`xcode-select --install`) and use macOS 14
or later: the ambient haze uses WebKit worker OffscreenCanvas WebGL, which
needs macOS 14+. Local builds use your Mac's architecture.

```bash
bun install
bun tauri build
```

Tauri's `beforeBuildCommand` runs `bun run build` and
`bun run build:engine` (which runs `cargo build --release -p renderer-engine`).
The workspace engine ends up at `target/release/PlaybackEngine.exe` on Windows
or `target/release/PlaybackEngine` on macOS. Tauri packages that executable as
`PlaybackEngine.exe` in the NSIS installer or as
`renderer.app/Contents/Resources/PlaybackEngine` in the Mac app. Outputs are
`target/release/bundle/nsis/` and `target/release/bundle/macos/renderer.app`.
`bun tauri dev` starts the frontend development server; build the engine first
with `bun run build:engine` if you have not already built it. Checks are
`cargo test -p renderer-engine`, `cargo test -p renderer`, `bun test`, and
`bun run build`.

The [desktop release workflow](../../actions/workflows/macos-build.yml)
builds and tests Windows x64 and Apple Silicon macOS when a GitHub release is
published. It attaches a Windows NSIS installer, an ad-hoc-signed macOS ZIP,
and signed updater artifacts for both systems; the updater manifest is
published only after all platform artifacts are ready. Install v0.1.19
manually: older releases did not include the updater. Later versions can
be checked and installed from Settings. Updater signatures use
a separate release-signing key, not an Apple Developer ID certificate;
Gatekeeper may still require manual approval for an initial macOS install.

Playback credentials and caches stay under `%LOCALAPPDATA%\SpotifyRenderer`
on Windows or `~/Library/Application Support/SpotifyRenderer` on macOS. The
optional personal Web API grant is stored in the operating system credential
store. Neither sign-in gives this project your Spotify password.

### Real-account UI harness (Windows)

Quit the native app, start Vite with `bun run dev`, then run
`bun dev/real-app.js --debug --build`. Open
`http://127.0.0.1:1420/dev/ui-harness.html?real`. The harness reads from the
native app, with cached answers available when it is closed; inspect
`window.__harness.real.sources()` to distinguish live and cached data.
Missing real data produces an error, not fixture content.

Harness playback is simulated and account writes are refused. Use the native
app for actual audio, authorization, library writes, and device transfers.
The explicit debug build is needed from elevated shells because recent
WebView2 runtimes ignore environment-supplied debug flags there. It opens a
localhost debugging port that can control the native app; close that copy
after UI work. Production builds do not enable the port by default. Keep the ignored
`dev/.real-cache/` account data out of commits and public screenshots.

## How it works

It has to be cheap to run while sitting open all day, so a few things follow
from that. The playhead is animated with a transform instead of a width, so it
doesn't force layout on every tick. Long lists are virtualized. The engine sends
a small position update on each heartbeat rather than the whole state.

The glass panes use a cached, frosted twin of the ambient haze rather than
re-blurring the entire window as content moves; the haze worker redraws when
its source or layout changes, not on every frame. Track dragging similarly
updates its hit test on movement or scroll, with continuous frames only while
edge autoscroll is moving. These keep the visual treatment intact at rest.

Two processes. `engine/` wraps [librespot](https://github.com/librespot-org/librespot)
and handles everything to do with sound. The Tauri shell in `src-tauri/`
supervises it, holds the caches, and serves a Svelte 5 frontend from `src/`.
Audio being in its own process means the interface can't interrupt playback, and
if the engine dies the shell restarts it and puts the queue back.

At startup the sidebar can use the on-disk playlist snapshot, while Home waits
for the authenticated rootlist so stale shelves do not flash. That rootlist
fetch runs alongside playback-state restoration rather than waiting for the
queue and settings to finish restoring.

If restoring playback fails, retries use capped backoff without delaying the
library refresh. Adding, moving or removing queue rows preserves Previous's
history for tracks still in the queue, including during shuffle. Playlist track
drags update the view immediately but send index-based edits sequentially; a
failed edit refreshes the playlist before later drags are applied.

Tauri and a web frontend are an odd pick for this. I used them because the UI
needed the most iteration and HTML and CSS were much faster to work in.
Rewriting the frontend lower-level would be straightforward to hand to agents
now, but it's already light enough that I'd rather keep it easy to change.

| Path         | What's in it                                                |
| ------------ | ----------------------------------------------------------- |
| `engine/`    | Playback engine: librespot, audio pipeline, browse, history  |
| `src-tauri/` | Tauri shell: engine supervision, caches, commands            |
| `src/`       | Svelte 5 frontend                                            |
| `dev/`       | Scratch harnesses, not part of the build                     |

`AGENTS.md` has the conventions the code follows, and is a better starting point
than this file if you want to change something.

### The resampling problem

librespot's rodio backend has a resampling bug that shows up on Windows. Spotify
decodes at 44.1 kHz and Windows usually runs its output at 48 kHz, so everything gets
resampled on the way out. rodio does that with linear interpolation, and it also
rebuilds its converter mid-stream, leaking a fraction of a frame each time it
does. At the packet sizes Spotify's Vorbis actually produces that came to
+0.4882% more output frames than there should be, measured offline and then
again on hardware. Extra frames at a fixed device rate means the audio is
stretched, so everything plays slightly slow and slightly flat.

Both halves needed replacing. There's a polyphase windowed-sinc resampler that
tracks position as an exact rational, so the output frame count is determined to
the frame regardless of where packet boundaries land, and the sink reports the
device's rate rather than 44.1 kHz, which sends rodio's own converters down their
pass-through path so they stop resampling the result a second time. Linear
interpolation measured −15 dB error at 10 kHz; this measures −116 dB. Both
numbers are pinned by tests.

## On the code

Nearly all of it was written by AI agents. The decisions about what to build and
how weren't left to them, and plenty of it got thrown out and redone.

It's an easy codebase to extend. If Spotify is missing something you want, point
an agent at it.

## License

MIT, see [LICENSE](LICENSE). Covers the code here and nothing belonging to
Spotify AB.
