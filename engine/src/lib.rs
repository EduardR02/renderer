//! Public API of the Spotify playback engine crate.
//!
//! The engine itself is a binary (`SpotifyPlaybackEngine.exe`) speaking a
//! line-delimited JSON protocol over stdin/stdout; the protocol wire types
//! live here so the Tauri shell (`src-tauri`) can depend on the exact
//! serde shapes without duplicating them.

pub mod protocol;

/// Atomic single-file replacement shared by every durable writer in the
/// engine and by the Tauri shell.
///
/// Windows has no atomic `rename` over an existing target (`fs::rename` fails
/// with "already exists"), so durable writers go through `MoveFileExW` with
/// `MOVEFILE_REPLACE_EXISTING` (the commit) and `MOVEFILE_WRITE_THROUGH`
/// (the metadata reaches disk before the call returns). Elsewhere a rename is
/// already atomic and durably ordered.
pub mod atomic {
    #[cfg(not(windows))]
    use std::fs;
    use std::io;
    use std::path::Path;

    #[cfg(not(windows))]
    pub fn replace_file_atomically(source: &Path, destination: &Path) -> io::Result<()> {
        fs::rename(source, destination)
    }

    #[cfg(windows)]
    pub fn replace_file_atomically(source: &Path, destination: &Path) -> io::Result<()> {
        use std::os::windows::ffi::OsStrExt;

        #[link(name = "Kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(
                existing_file_name: *const u16,
                new_file_name: *const u16,
                flags: u32,
            ) -> i32;
        }
        const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
        const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let moved = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
