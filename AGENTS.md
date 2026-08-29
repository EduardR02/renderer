# AGENTS.md

## Why this exists

This is a personal Windows Spotify client built to be the music application its owner actually wants: fast, calm, reliable, controllable, and pleasant to use.

Mainstream clients must serve broad product goals. This project does not. It can optimize for one person, remove unwanted complexity, and treat playback quality, responsiveness, efficiency, and interface quality as first-class product features.

Performance matters because a music player runs continuously. Wasted CPU, memory, disk or network activity, avoidable latency, and UI jank are not minor implementation details; they directly make the product worse.

## Stack

- Rust is the base: the playback engine lives in `engine/`, and the Tauri shell in `src-tauri/`.
- The frontend is Svelte 5 in `src/`, built with Bun and Vite. Use Bun.

Common checks:

```text
cargo test -p renderer-engine
cargo test -p renderer
bun run build
bun tauri build
```

## Principles

- Correct observable behavior comes first.
- Prefer simple, direct implementations over speculative abstraction.
- Fix root causes. Do not hide failures or patch around a bad design.
- Keep hot paths genuinely cheap. Avoid unnecessary allocation, copying, polling, locking, serialization, and repeated work.
- Consider the whole system. Engine, process boundary, persistence, and UI behavior must agree end to end.
- Read the relevant code before changing it. The repository is the source of truth.
- Preserve a high bar for interface quality. For UI work, follow the frontend design skill and inspect the real surface when possible.
- Verify significant changes with observable evidence. Never claim a result that was not actually checked.
- Bounded delegation keeps file details with workers while the parent retains plan and judgment; lean parent context improves decision quality and cost.

## No backwards compatibility

There is no requirement to preserve old internal APIs, cache formats, persisted prototypes, or abandoned designs unless explicitly requested.

If something can be removed and done better, remove it. Migrate callers and delete the old path. Do not leave shims, aliases, deprecated exports, fallback implementations, or parallel systems “just in case.” Favor the cleanest current design over historical accidents.

Leave unrelated user work untouched.
