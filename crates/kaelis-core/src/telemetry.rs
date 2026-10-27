//! Minecraft client telemetry opt-out.
//!
//! Kaelis collects nothing, and this module exists so the game does not
//! collect anything either. Modern Minecraft writes an online telemetry
//! preference into the instance's `options.txt`; older builds used a snooper
//! switch. Both are forced to `false` before a launch, so a fresh instance
//! never has to be asked and a saved `true` (from a hand-edited file or an
//! imported pack) is corrected the next time Play is pressed.
//!
//! The rewrite is conservative: every other line, and its order, is preserved,
//! and a file that is already opted out is left byte-for-byte alone.

use std::path::{Path, PathBuf};

/// The key modern Minecraft reads for its online telemetry preference.
pub const TELEMETRY_KEY: &str = "telemetryOptIn";

/// Historical switch that older clients still honour.
pub const LEGACY_SNOOPER_KEY: &str = "snooperEnabled";

/// A file larger than this is not an options file; leave it untouched rather
/// than read a hostile multi-gigabyte file into memory.
const MAX_OPTIONS_BYTES: u64 = 4 * 1024 * 1024;

/// `options.txt` inside one instance's game directory.
pub fn options_path(root: &Path) -> PathBuf {
    root.join("options.txt")
}

/// Force every telemetry switch in this instance's `options.txt` to `false`.
///
/// Returns `Ok(true)` when the file changed, `Ok(false)` when it already opted
/// out (or is too large to be an options file). Missing files are created with
/// the single line the game expects, which is exactly what the first launch
/// would have produced after the user clicked through the telemetry prompt.
pub fn enforce_disabled(root: &Path) -> std::io::Result<bool> {
    let path = options_path(root);
    let existing = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    if existing.len() as u64 > MAX_OPTIONS_BYTES {
        return Ok(false);
    }
    let text = String::from_utf8_lossy(&existing);
    let mut output = String::with_capacity(existing.len() + 32);
    let mut changed = false;
    let mut telemetry_seen = false;
    for line in text.lines() {
        match option_key(line) {
            Some(key) if key == TELEMETRY_KEY => {
                telemetry_seen = true;
                if !option_is_false(line) {
                    changed = true;
                }
                output.push_str(TELEMETRY_KEY);
                output.push_str(":false\n");
            }
            Some(key) if key == LEGACY_SNOOPER_KEY => {
                if !option_is_false(line) {
                    changed = true;
                }
                output.push_str(LEGACY_SNOOPER_KEY);
                output.push_str(":false\n");
            }
            _ => {
                output.push_str(line);
                output.push('\n');
            }
        }
    }
    if !telemetry_seen {
        changed = true;
        output.push_str(TELEMETRY_KEY);
        output.push_str(":false\n");
    }
    if !changed {
        return Ok(false);
    }
    write_staged(&path, output.as_bytes())?;
    Ok(true)
}

/// The key of an `options.txt` line, if it has one.
fn option_key(line: &str) -> Option<&str> {
    let (key, _) = line.split_once(':')?;
    let key = key.trim();
    (!key.is_empty()).then_some(key)
}

fn option_is_false(line: &str) -> bool {
    line.split_once(':')
        .is_some_and(|(_, value)| value.trim().eq_ignore_ascii_case("false"))
}

/// Write through a sibling temp file so a crash mid-write cannot leave the
/// user's key binds and settings truncated.
fn write_staged(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(".options-{}.part", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    // `rename` refuses to replace an existing file on Windows, so the old copy
    // is removed first; the staged file is already on disk by then.
    if std::fs::remove_file(path).is_err() && path.exists() {
        let _ = std::fs::remove_file(&temporary);
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "could not replace options.txt",
        ));
    }
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::{LEGACY_SNOOPER_KEY, TELEMETRY_KEY, enforce_disabled, options_path};

    fn scratch(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("kaelis-telemetry-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch dir");
        root
    }

    #[test]
    fn a_missing_file_is_created_opted_out() {
        let root = scratch("missing");
        assert!(enforce_disabled(&root).expect("write"));
        let text = std::fs::read_to_string(options_path(&root)).expect("read");
        assert_eq!(text.trim(), "telemetryOptIn:false");
        // A second pass has nothing to do.
        assert!(!enforce_disabled(&root).expect("re-read"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_enabled_setting_is_flipped_and_neighbours_survive() {
        let root = scratch("flip");
        std::fs::write(
            options_path(&root),
            "fov:90\ntelemetryOptIn:true\nsnooperEnabled:true\nmusic:0.4\n",
        )
        .expect("fixture");
        assert!(enforce_disabled(&root).expect("flip"));
        let text = std::fs::read_to_string(options_path(&root)).expect("read");
        assert_eq!(
            text,
            "fov:90\ntelemetryOptIn:false\nsnooperEnabled:false\nmusic:0.4\n"
        );
        assert!(!enforce_disabled(&root).expect("idempotent"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_opted_out_file_is_untouched() {
        let root = scratch("already-off");
        let original = "fov:90\ntelemetryOptIn:false\n";
        std::fs::write(options_path(&root), original).expect("fixture");
        assert!(!enforce_disabled(&root).expect("no change"));
        assert_eq!(
            std::fs::read_to_string(options_path(&root)).expect("read"),
            original
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unrelated_keys_and_legacy_files_are_handled() {
        let root = scratch("legacy");
        std::fs::write(options_path(&root), "snooperEnabled:true\n").expect("fixture");
        assert!(enforce_disabled(&root).expect("write"));
        let text = std::fs::read_to_string(options_path(&root)).expect("read");
        assert_eq!(
            text,
            format!("{LEGACY_SNOOPER_KEY}:false\n{TELEMETRY_KEY}:false\n")
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
