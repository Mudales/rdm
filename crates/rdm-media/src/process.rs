//! Starting helper programs (ffmpeg, yt-dlp, Deno, a virus scanner) out of
//! sight.

use std::ffi::OsStr;

/// `CREATE_NO_WINDOW`: Windows gives a console program started by a GUI app
/// a console window of its own, which flashes on screen for every run; this
/// flag runs it without one. Its own child processes (yt-dlp starting ffmpeg
/// or Deno) inherit that and stay hidden too.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A command for a helper program that runs in the background, without a
/// console window on Windows. Use it for every program rdm starts on its
/// own.
pub fn background_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    #[allow(unused_mut)] // only changed on Windows
    let mut cmd = tokio::process::Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// [`background_command`] for a synchronous [`std::process::Command`].
pub fn background_std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    #[allow(unused_mut)] // only changed on Windows
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}
