# Renderer

A desktop Spotify client for Windows. I built it because the official app used
more CPU than I want a music player to use. My computer has a liquid-cooled
Ryzen 9 5950X, and the native desktop app would constantly boost it to 80 °C.
WTF? Literal benchmarks that use all cores at max don't go that high. A music
player is constantly open, and randomly spinning my CPU to absurd temps is
insane, so I had to make this. It's worse if you have open-back headphones,
because you need your PC to be as silent as possible — every time I started a
song in the normal Spotify app it would spin my fans and distract me.

<p align="center">
  <img src="docs/library.png" alt="A playlist open in Renderer" width="900">
  <br><br>
  <img src="docs/now-playing.png" alt="The now playing panel" width="900">
</p>

## Install

Grab the setup from [Releases](../../releases) and run it. It's an ordinary
Windows install wizard, nothing unusual. You need a Spotify Premium account;
the app opens a Spotify login in your browser on first launch.

If you'd rather build it yourself, see [Building](#building).

## Not affiliated with Spotify

This is an unofficial client, with no connection to Spotify AB. "Spotify" is
their trademark and is used here only to say what this connects to.

Running an unofficial client very likely breaks Spotify's Terms of Use. This one
authenticates as a desktop client and uses internal endpoints that aren't part of
the public Web API, so it can get your account suspended. It needs your own
Premium account and gives you nothing you aren't already paying for. Your call.
No audio, metadata or artwork ships with this repository.

## Features

Mostly a normal client: playlists, albums, artist pages, search, queue, credits,
radio.

The pages only contain the music parts. An artist page has the discography,
popular tracks, bio, monthly listeners and top cities. No merch, no concert
tickets. There are no podcasts or audiobooks anywhere, no AI DJ, and no home
feed.

Audio is 320 kbps and gapless. Media keys work when the app isn't focused, and
it shows up in Windows Quick Settings and on the lock screen. Played tracks are
cached, so replaying them uses no network. It can launch at login, minimized if
you want. Playlists can be created, renamed, deleted and reordered, and tracks
added or removed by drag and drop. Settings has an audio cache size limit and a
volume normalisation toggle.

Some extra things I added because we control playback here:

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

Windows only, and you need Spotify Premium.

No lossless. Spotify doesn't offer it to this client at all — track metadata
comes back listing AAC 24 and Ogg Vorbis 96/160/320 and nothing else, so there
is no FLAC file to select. Getting at it would mean impersonating their client
far more thoroughly than this does. I compared 320 kbps against lossless in the
official app and couldn't tell them apart.

Liked Songs is read-only. You can browse and play it, but liking and unliking
has to happen in the Spotify app. Adding it needs another API surface and I
didn't want that tradeoff yet.

No Spotify Connect. You can't control other devices from here or send playback
to them. I don't use it.

## Building

You need [Rust](https://rustup.rs) and [Bun](https://bun.sh). Cargo compiles
every dependency from source, so the build directory ends up several GB; it's
all generated, and `cargo clean` takes it back.

```bash
bun install
bun run build:engine
bun tauri build
```

`bun tauri dev` for development. Checks are `cargo test -p renderer-engine`,
`cargo test -p renderer`, and `bun run build`.

Credentials go to Spotify, never through this app. What's stored locally is the
token they hand back, under `%LOCALAPPDATA%\SpotifyRenderer` along with the audio
cache, covers and history.

## How it works

It has to be cheap to run while sitting open all day, so a few things follow
from that. The playhead is animated with a transform instead of a width, so it
doesn't force layout on every tick. Long lists are virtualized. The engine sends
a small position update on each heartbeat rather than the whole state.

Two processes. `engine/` wraps [librespot](https://github.com/librespot-org/librespot)
and handles everything to do with sound. The Tauri shell in `src-tauri/`
supervises it, holds the caches, and serves a Svelte 5 frontend from `src/`.
Audio being in its own process means the interface can't interrupt playback, and
if the engine dies the shell restarts it and puts the queue back.

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
