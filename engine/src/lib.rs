//! Public API of the Spotify playback engine crate.
//!
//! The engine itself is a binary (`SpotifyPlaybackEngine.exe`) speaking a
//! line-delimited JSON protocol over stdin/stdout; the protocol wire types
//! live here so the Tauri shell (`src-tauri`) can depend on the exact
//! serde shapes without duplicating them.

pub mod protocol;
