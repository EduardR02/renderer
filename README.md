# SpotifyRenderer

SpotifyRenderer is a native Windows 11 Spotify browser, playlist editor, queue UI, and standalone local player. The Win32 application handles browsing and starts the sibling `SpotifyPlaybackEngine.exe` for audio. Playback never depends on another desktop application or a network control endpoint.

## First run

1. Keep `SpotifyRenderer.exe` and `SpotifyPlaybackEngine.exe` in the same directory.
2. Start `SpotifyRenderer.exe`.
3. The playback engine opens a browser for its own one-time Spotify sign-in. Its callback uses `http://127.0.0.1:5588/login`; that port must be available during sign-in.
4. To enable search, library, albums, artists, artwork, and playlist editing, open **Settings**, enter a Spotify developer Client ID and loopback redirect URI, save, and select **Authenticate**.

Playback authentication and Web API authentication are intentionally separate. Web API setup is optional for playback, but browsing features require it.

## Playback

The local engine uses librespot 0.8.0 and sends 320 kbps Ogg Vorbis audio through WASAPI. The native UI sends newline-delimited JSON over redirected anonymous stdin/stdout pipes. The engine owns the queue and publishes immediate state changes plus a position heartbeat every two seconds while playing.

The UI supports:

- play, pause, previous, next, seek, volume, shuffle, and repeat;
- starting a search result, album track, or playlist track with its full visible collection as the local queue;
- adding, removing, and moving local queue entries;
- search plus album and artist navigation;
- playlist create, rename, delete, add, remove, and reorder operations.

Engine diagnostics go to `%LOCALAPPDATA%\SpotifyRenderer\logs\playback_engine.log`; stdout is reserved for the JSON protocol.

## App-owned state

All state is under `%LOCALAPPDATA%\SpotifyRenderer`:

- Web API settings: `settings.json`
- DPAPI-protected Web API tokens: `tokens.dat`
- Playback credentials and bounded audio cache: `engine\`
- Cover cache: `covers\`
- Application and engine logs: `logs\`

The engine cache is bounded to 1 GiB. SpotifyRenderer does not read, copy, or modify another application's cache, downloads, credentials, or files.

## Build and package

Requirements:

- Visual Studio 2022 C++ desktop tools
- CMake 3.20 or newer
- Ninja
- Rust 1.85 or newer with Cargo

Commands:

```bat
build.cmd quick
build.cmd Release
build.cmd package
```

CMake builds the C++ executable and the Rust engine. The `package` target copies both executables and this README into `dist\`.

## Tests

`sr_tests` covers process-independent JSON protocol parsing, state mapping, complete track metadata, request correlation, and local queue command shapes. Engine integration itself is exercised by running the packaged pair; it requires an interactive Spotify sign-in and is not part of the unit test target.
