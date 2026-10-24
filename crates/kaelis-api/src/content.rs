//! Shared content-addressed store for verified game files.
//!
//! Every file Kaelis keeps is verified against a SHA-1 first, so that hash is a
//! perfect content address. Instead of giving each instance its own copy of the
//! libraries, assets, client jar, and Java runtime — 700 MB per instance before
//! this existed — a verified file is hard-linked into one store and every later
//! instance links the same bytes instead of downloading them again.
//!
//! A hard link is a second name for the same data on the same volume: the disk
//! holds one copy, both paths work, and removing either path leaves the other
//! intact. Nothing in a store-backed folder is edited in place (mods are
//! replaced, never patched), so sharing cannot leak a change between instances.
//! When the store sits on another volume, or the filesystem has no hard links,
//! every helper reports failure and the caller downloads exactly as before.
use std::path::{Path, PathBuf};
use std::time::Duration;

/// A SHA-1 is 40 hex characters; anything else is not a content address and
/// must never become a path segment.
const HASH_LEN: usize = 40;

/// Store entries younger than this are never pruned: an install may have just
/// written them and be about to link them.
const PRUNE_GRACE: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShareReport {
    pub files: u64,
    pub bytes: u64,
}

impl ShareReport {
    pub fn is_empty(self) -> bool {
        self.files == 0
    }

    pub fn add(&mut self, other: ShareReport) {
        self.files += other.files;
        self.bytes = self.bytes.saturating_add(other.bytes);
    }

    pub fn summary(self) -> String {
        if self.is_empty() {
            return "Nothing new to share.".to_owned();
        }
        format!(
            "{} file(s) shared · {:.1} MiB deduplicated.",
            self.files,
            self.bytes as f64 / (1024.0 * 1024.0)
        )
    }
}

/// `<data root>/content/sha1/<first two>/<rest>`.
pub fn store_root() -> PathBuf {
    data_root().join("content").join("sha1")
}

fn data_root() -> PathBuf {
    kaelis_core::instance::Instance::instances_dir()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// True when `hash` is a well-formed SHA-1, which is the only shape allowed to
/// become a store path.
pub fn valid_hash(hash: &str) -> bool {
    hash.len() == HASH_LEN && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn entry_in(store: &Path, hash: &str) -> Option<PathBuf> {
    if !valid_hash(hash) {
        return None;
    }
    let hash = hash.to_ascii_lowercase();
    Some(store.join(&hash[..2]).join(&hash[2..]))
}

/// Link a store entry into `destination` instead of downloading it.
///
/// The caller asks only when the destination is absent, so a failed attempt
/// leaves the normal download path untouched.
pub fn link_from_store(hash: &str, destination: &Path) -> bool {
    link_from_store_at(&store_root(), hash, destination)
}

fn link_from_store_at(store: &Path, hash: &str, destination: &Path) -> bool {
    let Some(entry) = entry_in(store, hash) else {
        return false;
    };
    if !entry.is_file() {
        return false;
    }
    if let Some(parent) = destination.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    std::fs::hard_link(&entry, destination).is_ok()
}

/// Add an already-verified file to the store when the store lacks it.
///
/// The file keeps its name and bytes; the store gains a second link to the same
/// data, which is what a later instance links from.
pub fn adopt(hash: &str, path: &Path) -> bool {
    adopt_at(&store_root(), hash, path)
}

fn adopt_at(store: &Path, hash: &str, path: &Path) -> bool {
    let Some(entry) = entry_in(store, hash) else {
        return false;
    };
    if entry.is_file() || !path.is_file() {
        return false;
    }
    if let Some(parent) = entry.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    std::fs::hard_link(path, &entry).is_ok()
}

/// Number of directory entries that name this file's data, or `None` when the
/// platform or filesystem cannot answer.
pub fn link_count(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path)
            .ok()
            .map(|metadata| metadata.nlink())
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_READ_ATTRIBUTES,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
            OPEN_EXISTING,
        };

        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // SAFETY: `wide` is NUL terminated for the call. Read attributes do not
        // open the data stream, and every share flag is set so a running game
        // cannot make the question fail.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: the handle and the out-structure are valid for the call.
        let ok = unsafe { GetFileInformationByHandle(handle, &mut information) };
        // SAFETY: the handle was opened above and is owned here.
        unsafe { CloseHandle(handle) };
        (ok != 0).then_some(u64::from(information.nNumberOfLinks))
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

/// Delete store entries no instance references any more.
///
/// An entry only the store names has one link: every instance copy is gone, so
/// the entry is pure cache. Removing it reclaims exactly what would otherwise
/// sit on disk forever. An entry an instance still uses has two or more links
/// and can never match. Recently written entries are skipped so a cleanup can
/// never race the install that just wrote them.
pub fn prune_unreferenced() -> ShareReport {
    prune_at(&store_root(), PRUNE_GRACE)
}

fn prune_at(store: &Path, grace: Duration) -> ShareReport {
    let mut report = ShareReport::default();
    let Ok(shards) = std::fs::read_dir(store) else {
        return report;
    };
    let now = std::time::SystemTime::now();
    for shard in shards.flatten() {
        let Ok(kind) = shard.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let recent = metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age < grace);
            if recent || link_count(&entry.path()) != Some(1) {
                continue;
            }
            if std::fs::remove_file(entry.path()).is_ok() {
                report.files += 1;
                report.bytes = report.bytes.saturating_add(metadata.len());
            }
        }
        // An emptied shard is noise; a failed removal is harmless.
        let _ = std::fs::remove_dir(shard.path());
    }
    report
}

fn marker_in(store: &Path) -> PathBuf {
    store
        .parent()
        .map(|root| root.join("seeded"))
        .unwrap_or_else(|| PathBuf::from("seeded"))
}

/// Mark one instance's already-installed files as shareable, once ever.
///
/// Instances created before the store existed would otherwise keep a private
/// copy forever. Assets are addressed by their own file name, so they are
/// adopted without reading a byte; libraries, the client jar, and the Java
/// runtime are hashed once and then adopted. A per-instance marker makes this a
/// one-time cost.
pub fn seed_instance(instance_root: &Path) -> ShareReport {
    seed_instance_at(&store_root(), instance_root)
}

fn seed_instance_at(store: &Path, instance_root: &Path) -> ShareReport {
    let Some(name) = instance_root
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
    else {
        return ShareReport::default();
    };
    if name.is_empty() {
        return ShareReport::default();
    }
    let markers = marker_in(store);
    let marker = markers.join(format!("{}.done", sanitize(&name)));
    if marker.is_file() {
        return ShareReport::default();
    }
    let mut report = ShareReport::default();
    seed_assets_at(store, instance_root, &mut report);
    for folder in ["libraries", "versions", "runtime"] {
        seed_hashed_at(store, &instance_root.join(folder), &mut report);
    }
    if std::fs::create_dir_all(&markers).is_ok() {
        let _ = std::fs::write(&marker, b"seeded\n");
    }
    report
}

/// `assets/objects/ab/abcdef…` — the file name is the whole hash and the folder
/// is its first two characters, so the address is known without reading a byte.
fn seed_assets_at(store: &Path, instance_root: &Path, report: &mut ShareReport) {
    let objects = instance_root.join("assets").join("objects");
    let Ok(shards) = std::fs::read_dir(&objects) else {
        return;
    };
    for shard in shards.flatten() {
        let shard_name = shard.file_name().to_string_lossy().to_ascii_lowercase();
        if shard_name.len() != 2 {
            continue;
        }
        let Ok(files) = std::fs::read_dir(shard.path()) else {
            continue;
        };
        for file in files.flatten() {
            let hash = file.file_name().to_string_lossy().to_ascii_lowercase();
            // The object layout repeats the hash as the file name; anything
            // else in this tree is not an asset object and is left alone.
            if hash.len() != 40 || !hash.starts_with(&shard_name) {
                continue;
            }
            let Ok(metadata) = file.metadata() else {
                continue;
            };
            if metadata.is_file() && adopt_at(store, &hash, &file.path()) {
                report.files += 1;
                report.bytes = report.bytes.saturating_add(metadata.len());
            }
        }
    }
}

fn seed_hashed_at(store: &Path, root: &Path, report: &mut ShareReport) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            // A Java runtime is many nested folders; every file is content.
            seed_hashed_at(store, &path, report);
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        // Hashing only pays for files large enough to duplicate meaningfully and
        // small enough to keep the pass bounded if one is corrupt.
        if !(4096..=512 * 1024 * 1024).contains(&metadata.len()) {
            continue;
        }
        let Ok(hash) = crate::hash::sha1_file(&path) else {
            continue;
        };
        if adopt_at(store, &hash, &path) {
            report.files += 1;
            report.bytes = report.bytes.saturating_add(metadata.len());
        }
    }
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .take(64)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{adopt_at, link_count, link_from_store_at, prune_at, seed_instance_at, store_root};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    fn scratch(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kaelis-content-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn store(root: &Path) -> PathBuf {
        root.join("content").join("sha1")
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    const HASH: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn a_verified_file_links_into_a_second_instance_without_a_copy() {
        let root = scratch("link");
        let store = store(&root);
        let first = root.join("instances/one/mods/sodium.jar");
        let second = root.join("instances/two/mods/sodium.jar");
        write(&first, b"jar bytes");
        assert!(adopt_at(&store, HASH, &first));
        assert!(link_from_store_at(&store, HASH, &second));
        assert_eq!(std::fs::read(&second).unwrap(), b"jar bytes");
        // One inode: the first instance, the second instance, and the store.
        assert_eq!(link_count(&first), Some(3));
        // Removing one name keeps the other readable.
        std::fs::remove_file(&first).unwrap();
        assert_eq!(std::fs::read(&second).unwrap(), b"jar bytes");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_unknown_or_malformed_hash_never_touches_the_disk() {
        let root = scratch("invalid");
        let store = store(&root);
        let destination = root.join("instances/one/a.jar");
        assert!(!link_from_store_at(&store, "deadbeef", &destination));
        assert!(!link_from_store_at(
            &store,
            "../../etc/passwd",
            &destination
        ));
        assert!(!destination.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn pruning_removes_only_unreferenced_entries() {
        let root = scratch("prune");
        let store = store(&root);
        let kept = root.join("instances/one/a.jar");
        write(&kept, b"kept");
        assert!(adopt_at(&store, HASH, &kept));
        let orphan = root.join("orphan.bin");
        let orphan_hash = "fedcba9876543210fedcba9876543210fedcba98";
        write(&orphan, b"orphan");
        assert!(adopt_at(&store, orphan_hash, &orphan));
        // The orphan source is the second link; the kept file stays referenced.
        std::fs::remove_file(&orphan).unwrap();
        let report = prune_at(&store, Duration::ZERO);
        assert_eq!(report.files, 1);
        assert_eq!(report.bytes, 6);
        assert_eq!(std::fs::read(&kept).unwrap(), b"kept");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn seeding_reads_asset_names_and_skips_a_second_pass() {
        let root = scratch("seed");
        let store = store(&root);
        let instance = root.join("instances/one");
        // The real object layout repeats the hash as the file name.
        let asset = instance.join(format!("assets/objects/{}/{HASH}", &HASH[..2]));
        write(&asset, b"asset");
        // A file that is not an object name must be left where it is.
        let foreign = instance.join(format!("assets/objects/{}/notes.txt", &HASH[..2]));
        write(&foreign, b"notes");
        let report = seed_instance_at(&store, &instance);
        assert_eq!(report.files, 1);
        assert!(store.join(&HASH[..2]).join(&HASH[2..]).is_file());
        assert!(foreign.is_file());
        // The marker makes the next startup free.
        assert_eq!(seed_instance_at(&store, &instance).files, 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_store_on_another_volume_falls_back_to_the_download() {
        // A missing store directory is the portable-copy case: every helper
        // reports failure instead of creating a link to nothing.
        let root = scratch("missing");
        let destination = root.join("instances/one/a.jar");
        assert!(!link_from_store_at(&store(&root), HASH, &destination));
        assert!(!destination.exists());
        assert!(prune_at(&store(&root), Duration::ZERO).is_empty());
        assert!(store_root().is_absolute());
        let _ = std::fs::remove_dir_all(root);
    }
}
