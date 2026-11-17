//! macOS: the managed tools' file names and assets, a process group for each child so it can be
//! killed with what it started, and the execute bit on a downloaded tool.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use super::Managed;

/// The name `yt-dlp` has on PATH.
pub(crate) const YTDLP_PATH_NAME: &str = "yt-dlp";
/// The name of the managed copies in the tools folder.
pub(crate) const YTDLP_FILE: &str = "yt-dlp";
pub(crate) const QJS_FILE: &str = "qjs";

#[cfg(target_arch = "aarch64")]
pub(crate) fn managed() -> Option<Managed> {
    Managed::from_hex(
        "yt-dlp_macos",
        "qjs-darwin-arm64",
        "91fe89916d3b0a6c01c4dc8382a99c3c1fd91ecfc214aa81ca051902bcce590d",
    )
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn managed() -> Option<Managed> {
    Managed::from_hex(
        "yt-dlp_macos",
        "qjs-darwin-x86_64",
        "fa2c5dc78095daff566b32fab995ced12927875253b84216b91b834af2ef2b74",
    )
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
pub(crate) fn managed() -> Option<Managed> {
    None
}

/// The child leads its own process group, so one signal reaches it and what it started.
pub(crate) fn configure_command(cmd: &mut Command) {
    cmd.process_group(0);
}

/// Best effort: SIGKILL to the child's whole group. The caller kills the process itself
/// afterwards.
pub(crate) fn kill_tree(pid: u32) {
    let _ = Command::new("/bin/kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// A new file is readable by its owner only (`0o600`).
pub(crate) fn private_file(opts: &mut std::fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    opts.mode(0o600);
}

/// `chmod 0o755`.
pub(crate) fn make_executable(path: &Path) -> std::io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}
