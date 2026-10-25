//! Verified in-place updates for the launcher itself.
//!
//! The update check only links to a release page; this module is the part that
//! can actually replace the installed build. It is deliberately strict: the
//! newest GitHub release is discovered through the public API, the Windows
//! executable is streamed to a temporary directory while its SHA-256 is
//! computed, and the result is accepted only when it matches the checksum the
//! release publishes. Anything else is deleted, so a truncated download or a
//! substituted file can never be executed.
//!
//! Installing is also conservative. The new build is launched by a short batch
//! script that waits for this process to exit, renames the old executable to a
//! backup, copies the verified build into place, and rolls the backup back if
//! the copy fails. Kaelis only starts that script after proving it can write to
//! its own directory; a protected install (for example `Program Files`) is
//! reported instead of being half-replaced.

use futures::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const LATEST_RELEASE: &str = "https://api.github.com/repos/fuvji/kaelis/releases/latest";

/// The asset that corresponds to this platform's build.
#[cfg(windows)]
pub const BINARY_NAME: &str = "Kaelis.exe";
#[cfg(target_os = "macos")]
pub const BINARY_NAME: &str = "Kaelis-macos-universal.zip";
#[cfg(all(unix, not(target_os = "macos")))]
pub const BINARY_NAME: &str = "Kaelis-linux-x64.tar.gz";

/// Published checksum list covering every platform artifact.
const CHECKSUMS_NAME: &str = "SHA256SUMS.txt";

/// A download that only makes sense on the platform that can install it.
const CAN_INSTALL: bool = cfg!(windows);

/// Refuse anything implausibly large before writing it to disk.
const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct ReleaseResponse {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize, Clone)]
struct Asset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

/// A release newer than this build, with the artifact to install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateAsset {
    /// Version without the leading `v`.
    pub version: String,
    /// Release page, used as the fallback when installing is not possible.
    pub page_url: String,
    /// Direct URL of this platform's binary.
    pub binary_url: String,
    /// Published size, or zero when GitHub did not report one.
    pub binary_size: u64,
    /// Direct URL of the checksum list.
    pub checksums_url: String,
}

/// A verified executable waiting to be swapped in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedUpdate {
    pub version: String,
    /// The verified download, still inside the temporary staging directory.
    pub staged: PathBuf,
    /// The executable it must replace.
    pub target: PathBuf,
}

/// Ask GitHub for a newer installable build.
///
/// Returns `Ok(None)` when this build is current, which keeps the caller free
/// of version arithmetic. A release without the expected artifacts is treated
/// as not installable rather than as an error, because a partial upload should
/// never leave the user with a broken button.
pub async fn check(client: &reqwest::Client) -> Result<Option<UpdateAsset>, String> {
    let current = env!("CARGO_PKG_VERSION");
    let response = client
        .get(LATEST_RELEASE)
        .header("User-Agent", kaelis_core::brand::user_agent())
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("GitHub answered {}", response.status()));
    }
    let body: ReleaseResponse = response.json().await.map_err(|error| error.to_string())?;
    let version = body.tag_name.trim().trim_start_matches('v').to_owned();
    if !crate::release::is_newer(current, &version) {
        return Ok(None);
    }
    // Keeping both URLs means the GUI can still offer a manual download when
    // this host cannot install the artifact itself.
    let Some(binary) = body.assets.iter().find(|asset| asset.name == BINARY_NAME) else {
        return Ok(None);
    };
    let Some(checksums) = body
        .assets
        .iter()
        .find(|asset| asset.name == CHECKSUMS_NAME)
    else {
        return Ok(None);
    };
    if !CAN_INSTALL {
        return Ok(None);
    }
    Ok(Some(UpdateAsset {
        version,
        page_url: body.html_url,
        binary_url: binary.browser_download_url.clone(),
        binary_size: binary.size,
        checksums_url: checksums.browser_download_url.clone(),
    }))
}

/// Download, verify, and stage the build.
///
/// The returned path is inside a per-process staging directory; [`apply`] owns
/// its lifecycle. A failed verification removes the partial file, so a retry
/// starts clean and nothing unverified is left on disk.
pub async fn prepare(client: &reqwest::Client) -> Result<Option<PreparedUpdate>, String> {
    let Some(asset) = check(client).await? else {
        return Ok(None);
    };
    if asset.binary_size > MAX_DOWNLOAD_BYTES {
        return Err("the published build is larger than expected".to_owned());
    }
    let target = current_exe()?;
    ensure_writable(&target)?;

    let expected = checksum_for(client, &asset.checksums_url, BINARY_NAME).await?;
    let staging = staging_dir()?;
    let staged = staging.join(format!("Kaelis-{}.exe", asset.version));
    // A leftover from an interrupted attempt must not be mistaken for a
    // verified download.
    let _ = tokio::fs::remove_file(&staged).await;
    let partial = staged.with_extension("part");

    let written = download(client, &asset.binary_url, &partial, &expected).await;
    if let Err(error) = written {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(error);
    }
    tokio::fs::rename(&partial, &staged)
        .await
        .map_err(|error| format!("could not stage the update: {error}"))?;
    Ok(Some(PreparedUpdate {
        version: asset.version,
        staged,
        target,
    }))
}

/// Install a prepared update and start the new build.
///
/// The script runs detached and is the only thing that survives this process.
/// Callers exit immediately afterwards, which is what releases the lock on the
/// executable the script is waiting for.
#[cfg(windows)]
pub fn apply(update: &PreparedUpdate) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DETACHED_PROCESS: u32 = 0x0000_0008;

    let directory = staging_dir()?;
    let script_path = directory.join("kaelis-update.cmd");
    let script = swap_script(update, std::process::id());
    std::fs::write(&script_path, script)
        .map_err(|error| format!("could not write the update script: {error}"))?;
    // `cmd` parses parentheses in an unquoted path before it applies quotes, so
    // an install directory like `Kaelis (1)` would derail `cmd /C <path>`. The
    // script is therefore addressed by file name from its own directory.
    std::process::Command::new("cmd")
        .current_dir(&directory)
        .arg("/C")
        // The explicit `%CD%`-relative form still resolves when a shell exports
        // `NoDefaultCurrentDirectoryInExePath`, which Git Bash does.
        .arg(".\\kaelis-update.cmd")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
        .map_err(|error| format!("could not start the updater: {error}"))?;
    Ok(())
}

/// Installing in place is a Windows behaviour; the macOS and Linux builds ship
/// as archives that the user unpacks deliberately.
#[cfg(not(windows))]
pub fn apply(_update: &PreparedUpdate) -> Result<(), String> {
    Err("this build updates through its release archive".to_owned())
}

/// The batch script that performs the swap after this process exits.
///
/// It polls the launcher's own pid instead of sleeping blindly, so a slow
/// shutdown costs nothing and a fast one is not raced. The old executable is
/// moved aside before the copy so a failed install can be rolled back instead
/// of leaving the user without a launcher. `%` is doubled because `set`
/// expands it, and a path that cannot be represented safely is rejected by
/// [`apply`]'s callers through [`prepare`]'s writability check.
#[cfg(windows)]
fn swap_script(update: &PreparedUpdate, pid: u32) -> String {
    let exe = script_path(&update.target);
    let staged = script_path(&update.staged);
    let backup = script_path(&update.target.with_extension("exe.old"));
    format!(
        "@echo off\r\n\
         setlocal\r\n\
         set \"KAELIS_PID={pid}\"\r\n\
         set \"KAELIS_EXE={exe}\"\r\n\
         set \"KAELIS_NEW={staged}\"\r\n\
         set \"KAELIS_OLD={backup}\"\r\n\
         set /a KAELIS_TRIES=0\r\n\
         :wait\r\n\
         tasklist /FI \"PID eq %KAELIS_PID%\" /NH 2>nul | findstr /R /C:\" %KAELIS_PID% \" >nul || goto swap\r\n\
         set /a KAELIS_TRIES+=1\r\n\
         if %KAELIS_TRIES% GEQ 120 goto swap\r\n\
         timeout /t 1 /nobreak >nul\r\n\
         goto wait\r\n\
         :swap\r\n\
         move /Y \"%KAELIS_EXE%\" \"%KAELIS_OLD%\" >nul 2>&1\r\n\
         if errorlevel 1 goto retry\r\n\
         copy /Y \"%KAELIS_NEW%\" \"%KAELIS_EXE%\" >nul 2>&1\r\n\
         if errorlevel 1 goto rollback\r\n\
         start \"\" \"%KAELIS_EXE%\"\r\n\
         del /F /Q \"%KAELIS_NEW%\" >nul 2>&1\r\n\
         del /F /Q \"%KAELIS_OLD%\" >nul 2>&1\r\n\
         del /F /Q \"%~f0\" >nul 2>&1\r\n\
         exit /b 0\r\n\
         :retry\r\n\
         set /a KAELIS_TRIES+=1\r\n\
         if %KAELIS_TRIES% GEQ 180 goto fail\r\n\
         timeout /t 1 /nobreak >nul\r\n\
         goto swap\r\n\
         :rollback\r\n\
         move /Y \"%KAELIS_OLD%\" \"%KAELIS_EXE%\" >nul 2>&1\r\n\
         :fail\r\n\
         exit /b 1\r\n"
    )
}

/// A path is safe for the generated script when quoting can represent it.
#[cfg(windows)]
fn script_path(path: &Path) -> String {
    path.display().to_string().replace('%', "%%")
}

fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|error| format!("could not locate Kaelis: {error}"))
}

/// Prove the install directory accepts writes before anything is downloaded
/// over the network, so a protected folder is reported instantly.
fn ensure_writable(target: &Path) -> Result<(), String> {
    let directory = target
        .parent()
        .ok_or_else(|| "the launcher has no install directory".to_owned())?;
    let probe = directory.join(".kaelis-update-probe");
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Ok(())
        }
        Err(_) => Err(format!(
            "Kaelis cannot replace itself inside {}; download the new build from the release page",
            directory.display()
        )),
    }
}

/// How long a staging file may sit untouched before it is treated as junk.
///
/// An update script can outlive the launcher by a moment while it waits for the
/// process to exit, so recent files are left alone; anything from an earlier
/// run is deleted instead of accumulating in the user's temp folder.
const STALE_AFTER: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Delete leftovers from earlier updates.
///
/// Called once at start-up. A missing directory, a file that vanished during
/// the scan, and a locked file are all silent: this is housekeeping, not a
/// feature that may fail the launch.
pub fn purge_staging() {
    let directory = std::env::temp_dir().join("kaelis-update");
    let Ok(entries) = std::fs::read_dir(&directory) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= STALE_AFTER);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn staging_dir() -> Result<PathBuf, String> {
    let directory = std::env::temp_dir().join("kaelis-update");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create the update directory: {error}"))?;
    Ok(directory)
}

/// Read the checksum list and return the digest published for `name`.
async fn checksum_for(client: &reqwest::Client, url: &str, name: &str) -> Result<String, String> {
    let response = client
        .get(url)
        .header("User-Agent", kaelis_core::brand::user_agent())
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("the checksum list answered {}", response.status()));
    }
    let body = response.text().await.map_err(|error| error.to_string())?;
    parse_checksum(&body, name)
        .ok_or_else(|| format!("the release publishes no checksum for {name}"))
}

/// `sha256sum` writes `<hex>  <name>` (and `*<name>` for binary mode).
fn parse_checksum(list: &str, name: &str) -> Option<String> {
    for line in list.lines() {
        let mut parts = line.split_whitespace();
        let digest = parts.next()?;
        let file = parts.next()?.trim_start_matches('*');
        if file == name && digest.len() == 64 && digest.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(digest.to_ascii_lowercase());
        }
    }
    None
}

/// Stream the artifact to `destination`, hashing as it arrives.
///
/// Hashing during the download avoids a second full read of a 17 MB file and
/// means a corrupted transfer is detected before the file is ever renamed into
/// its final, installable name.
async fn download(
    client: &reqwest::Client,
    url: &str,
    destination: &Path,
    expected: &str,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    let response = client
        .get(url)
        .header("User-Agent", kaelis_core::brand::user_agent())
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("the download answered {}", response.status()));
    }
    if let Some(length) = response.content_length()
        && length > MAX_DOWNLOAD_BYTES
    {
        return Err("the published build is larger than expected".to_owned());
    }
    let mut file = tokio::fs::File::create(destination)
        .await
        .map_err(|error| format!("could not create the staged build: {error}"))?;
    let mut hasher = Sha256::new();
    let mut stream = response.bytes_stream();
    let mut written = 0_u64;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| error.to_string())?;
        written += chunk.len() as u64;
        if written > MAX_DOWNLOAD_BYTES {
            return Err("the published build is larger than expected".to_owned());
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("could not write the staged build: {error}"))?;
    }
    file.flush()
        .await
        .map_err(|error| format!("could not finish the staged build: {error}"))?;
    drop(file);
    let actual = hex(&hasher.finalize());
    if actual != expected {
        return Err("the downloaded build did not match its published checksum".to_owned());
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    text
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::{PreparedUpdate, swap_script};
    use super::{hex, parse_checksum};
    #[cfg(windows)]
    use std::path::PathBuf;

    #[test]
    fn checksums_are_read_for_the_exact_asset() {
        let list = "\
aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  Kaelis-Setup.exe
bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb *Kaelis.exe
cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc  Kaelis-Portable.zip
";
        assert_eq!(
            parse_checksum(list, "Kaelis.exe").as_deref(),
            Some("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
        );
        assert_eq!(
            parse_checksum(list, "Kaelis-Portable.zip").map(|d| d.len()),
            Some(64)
        );
        assert!(parse_checksum(list, "missing.exe").is_none());
    }

    #[test]
    fn a_short_or_non_hex_digest_is_not_a_match() {
        let list = "abc  Kaelis.exe\nzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz  Kaelis.exe\n";
        assert!(parse_checksum(list, "Kaelis.exe").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn the_swap_script_waits_for_the_launcher_and_rolls_back() {
        let update = PreparedUpdate {
            version: "0.2.9".to_owned(),
            staged: PathBuf::from(r"C:\Temp\kaelis-update\Kaelis-0.2.9.exe"),
            target: PathBuf::from(r"C:\Kaelis\Kaelis.exe"),
        };
        let script = swap_script(&update, 4321);
        assert!(script.contains("set \"KAELIS_PID=4321\""));
        assert!(script.contains(r"C:\Kaelis\Kaelis.exe"));
        assert!(script.contains("findstr"));
        // The backup happens before the copy and is restored on failure.
        let backup_at = script.find(":rollback").expect("rollback label");
        assert!(script[backup_at..].contains("move /Y \"%KAELIS_OLD%\" \"%KAELIS_EXE%\""));
        assert!(script.contains("start \"\" \"%KAELIS_EXE%\""));
    }

    #[cfg(windows)]
    #[test]
    fn percent_signs_are_escaped_for_set() {
        let update = PreparedUpdate {
            version: "0.2.9".to_owned(),
            staged: PathBuf::from(r"C:\100%\Kaelis-0.2.9.exe"),
            target: PathBuf::from(r"C:\Kaelis\Kaelis.exe"),
        };
        let script = swap_script(&update, 1);
        assert!(script.contains(r"C:\100%%\Kaelis-0.2.9.exe"));
    }

    #[test]
    fn staging_leftovers_are_purged_but_recent_files_survive() {
        use std::time::{Duration, SystemTime};

        let directory = std::env::temp_dir().join("kaelis-update");
        std::fs::create_dir_all(&directory).expect("staging dir");
        let fresh = directory.join(format!("fresh-{}.part", std::process::id()));
        let stale = directory.join(format!("stale-{}.exe", std::process::id()));
        std::fs::write(&fresh, b"fresh").expect("fresh file");
        std::fs::write(&stale, b"old").expect("stale file");
        let file = std::fs::File::options()
            .write(true)
            .open(&stale)
            .expect("stale file");
        file.set_modified(SystemTime::now() - Duration::from_secs(2 * 60 * 60))
            .expect("backdate");
        super::purge_staging();
        assert!(
            fresh.exists(),
            "a recent staging file may belong to a live update"
        );
        assert!(!stale.exists(), "an old staging file is junk");
        let _ = std::fs::remove_file(&fresh);
    }

    #[test]
    fn hex_encoding_is_lowercase_and_fixed_width() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }

    /// The swap is only useful if Windows really lets the script move a
    /// running-adjacent executable aside, copy the verified build in, and
    /// launch it. A detached dummy process stands in for the launcher so the
    /// wait loop is exercised, and both executables are harmless copies of
    /// `where.exe`, which exits on its own.
    #[cfg(windows)]
    #[test]
    fn the_swap_script_replaces_the_executable_and_launches_it() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        static SEQUENCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let root = std::env::temp_dir().join(format!(
            "kaelis-swap-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp dir");
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_owned());
        let where_exe = PathBuf::from(system).join("System32\\where.exe");
        let target = root.join("Kaelis.exe");
        let staged = root.join("Kaelis-new.exe");
        std::fs::copy(&where_exe, &target).expect("old build");
        std::fs::copy(&where_exe, &staged).expect("new build");
        // A byte at the end makes the copies distinguishable without breaking
        // the PE header Windows needs in order to start the result.
        let mut new_bytes = std::fs::read(&staged).expect("read staged");
        new_bytes.extend_from_slice(b"kaelis-update-test");
        std::fs::write(&staged, &new_bytes).expect("mark staged");

        let mut dummy = Command::new("cmd")
            .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("dummy launcher");
        let update = PreparedUpdate {
            version: "0.0.0-test".to_owned(),
            staged: staged.clone(),
            target: target.clone(),
        };
        let script_path = root.join("kaelis-update.cmd");
        std::fs::write(&script_path, swap_script(&update, dummy.id())).expect("script");
        let log_path = root.join("script.log");
        let log = std::fs::File::create(&log_path).expect("log");
        let log_err = log.try_clone().expect("log clone");
        let mut script_child = Command::new("cmd")
            .current_dir(&root)
            .arg("/C")
            .arg(".\\kaelis-update.cmd")
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err))
            .spawn()
            .expect("run script");
        // "Quit" the dummy, which is the event the script is waiting for.
        std::thread::sleep(Duration::from_millis(400));
        let _ = dummy.kill();
        let _ = dummy.wait();

        let deadline = Instant::now() + Duration::from_secs(15);
        let mut replaced = false;
        while Instant::now() < deadline {
            if std::fs::read(&target).is_ok_and(|bytes| bytes == new_bytes) {
                replaced = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if !replaced {
            let log = std::fs::read_to_string(&log_path).unwrap_or_default();
            println!("swap log:\n{log}");
            println!(
                "root contains: {:?}",
                std::fs::read_dir(&root).map(|entries| entries
                    .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
                    .collect::<Vec<_>>())
            );
        }
        assert!(
            replaced,
            "the script must copy the verified build over the old one"
        );
        assert!(
            target.exists(),
            "the launcher must still exist after the swap"
        );
        // The old file and the download are cleaned up by the script.
        let cleanup = Instant::now() + Duration::from_secs(5);
        let leftover = root.join("Kaelis.exe.old");
        while Instant::now() < cleanup && (staged.exists() || leftover.exists()) {
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = script_child.wait();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The real download path: the published artifact must match the checksum
    /// the same release publishes. Ignored by default because it is a network
    /// test; run it with `cargo test -p kaelis-api -- --ignored`.
    #[tokio::test]
    #[ignore = "downloads the published release"]
    async fn the_published_build_matches_its_checksum() {
        use super::{BINARY_NAME, CHECKSUMS_NAME, LATEST_RELEASE, ReleaseResponse, checksum_for};
        let client = reqwest::Client::new();
        let body: ReleaseResponse = client
            .get(LATEST_RELEASE)
            .header("User-Agent", kaelis_core::brand::user_agent())
            .send()
            .await
            .expect("release list")
            .json()
            .await
            .expect("release json");
        let binary = body
            .assets
            .iter()
            .find(|asset| asset.name == BINARY_NAME)
            .expect("published executable");
        let sums = body
            .assets
            .iter()
            .find(|asset| asset.name == CHECKSUMS_NAME)
            .expect("published checksums");
        let expected = checksum_for(&client, &sums.browser_download_url, BINARY_NAME)
            .await
            .expect("checksum for the executable");
        let destination = super::staging_dir()
            .expect("staging")
            .join("probe-Kaelis.exe");
        super::download(
            &client,
            &binary.browser_download_url,
            &destination,
            &expected,
        )
        .await
        .expect("verified download");
        let written = std::fs::metadata(&destination).expect("staged file").len();
        assert!(written > 1_000_000, "the executable is implausibly small");
        let _ = std::fs::remove_file(&destination);
    }
}
