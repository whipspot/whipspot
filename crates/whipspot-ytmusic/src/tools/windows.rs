//! Windows: the managed tools' file names and assets, no console windows for the child
//! processes, and killing a process together with what it started.

use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use super::Managed;

/// `CREATE_NO_WINDOW`: the child gets no console window.
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The name `yt-dlp` has on PATH.
pub(crate) const YTDLP_PATH_NAME: &str = "yt-dlp.exe";
/// The name of the managed copies in the tools folder.
pub(crate) const YTDLP_FILE: &str = "yt-dlp.exe";
pub(crate) const QJS_FILE: &str = "qjs.exe";

/// The pinned assets, for the targets there is a managed install on.
#[cfg(target_arch = "x86_64")]
pub(crate) fn managed() -> Option<Managed> {
    Managed::from_hex(
        "yt-dlp.exe",
        "qjs-windows-x86_64.exe",
        "ff03398d74ea6e6bd3a8e58058f7003225abcac229d7d57775b7aaae812a49c7",
    )
}

#[cfg(not(target_arch = "x86_64"))]
pub(crate) fn managed() -> Option<Managed> {
    None
}

pub(crate) fn configure_command(cmd: &mut Command) {
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Best effort: `taskkill /T /F` also ends what the process started (yt-dlp's one-file build
/// runs the real program as a child). The caller kills the process itself afterwards.
pub(crate) fn kill_tree(pid: u32) {
    let Some(root) = std::env::var_os("SystemRoot") else {
        return;
    };
    let exe = Path::new(&root).join("System32").join("taskkill.exe");
    let mut cmd = Command::new(exe);
    cmd.args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.creation_flags(CREATE_NO_WINDOW);
    let _ = cmd.status();
}

/// Nothing to do: a file in the user's profile is private to the user already.
pub(crate) fn private_file(_opts: &mut std::fs::OpenOptions) {}

/// Nothing to do: Windows has no execute bit.
pub(crate) fn make_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
