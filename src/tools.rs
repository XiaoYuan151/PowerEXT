use std::path::PathBuf;

#[cfg(target_os = "macos")]
static FFMPEG: &[u8] = include_bytes!("../assets/ffmpeg");
#[cfg(target_os = "macos")]
static MAGICK: &[u8] = include_bytes!("../assets/magick");

/// Extract an embedded binary to `data_dir/bin/<name>` if not already there,
/// then return its path. Falls back to PATH if the platform has no embedded copy.
pub fn tool_path(name: &str, _data_dir: &std::path::Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let data_dir = _data_dir;
        let (bytes, fname) = match name {
            "ffmpeg"  => (FFMPEG,  "ffmpeg"),
            "convert" | "magick" => (MAGICK, "magick"),
            _ => return path_lookup(name),
        };
        let bin_dir = data_dir.join("bin");
        let dest = bin_dir.join(fname);
        if !dest.exists() {
            std::fs::create_dir_all(&bin_dir).ok()?;
            std::fs::write(&dest, bytes).ok()?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).ok()?;
            }
        }
        return Some(dest);
    }
    #[allow(unreachable_code)]
    path_lookup(name)
}

fn path_lookup(name: &str) -> Option<PathBuf> {
    let cmd = if cfg!(target_os = "windows") { "where" } else { "which" };
    std::process::Command::new(cmd).arg(name).output().ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| PathBuf::from(s.trim()))
}
