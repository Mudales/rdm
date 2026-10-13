//! Shutting the computer down when a queue finishes (if the user chose so).

use rdm_media::background_std_command as command;

/// Asks the operating system to shut down now. On macOS this goes through
/// System Events, which may ask the user to allow automation once.
pub fn shutdown_computer() -> std::io::Result<()> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = command("osascript");
        c.args(["-e", "tell application \"System Events\" to shut down"]);
        c
    } else if cfg!(windows) {
        let mut c = command("shutdown");
        c.args(["/s", "/t", "0"]);
        c
    } else {
        let mut c = command("systemctl");
        c.arg("poweroff");
        c
    };
    let status = cmd.status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "shutdown command failed ({status})"
        )))
    }
}
