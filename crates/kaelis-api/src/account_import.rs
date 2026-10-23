//! Universal account import.
//!
//! Players arrive with identities spread across launchers. This module reads
//! the account files that actually exist on disk — the official Minecraft
//! Launcher's `launcher_accounts.json`, and Prism/MultiMC/PolyMC's
//! `accounts.json` — plus any JSON the user points at, and turns them into
//! Kaelis account rows.
//!
//! What can honestly be transferred:
//!
//! * **Offline profiles** are complete. The name is the identity, so an
//!   imported offline row works immediately.
//! * **Microsoft identities** transfer as identities, not sessions. Microsoft
//!   refresh tokens are bound to the client that requested them, so a token
//!   written by another launcher cannot be redeemed with Kaelis' client id.
//!   The row keeps the name, UUID, and head; connecting it takes one sign-in.
//!
//! The parser is deliberately conservative: a name must be a legal Minecraft
//! username, and an object only counts as a player when it also carries a
//! UUID-shaped id or a token. That keeps an instance JSON dropped on the
//! window from being mistaken for an account list.

use crate::accounts::{AccountKind, SavedAccount};
use crate::auth::offline_profile;
use serde_json::Value;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("the file is not JSON")]
    Json,
    #[error("no Minecraft account found in the file")]
    Empty,
}

/// One identity read from another launcher.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedAccount {
    pub kind: AccountKind,
    pub name: String,
    /// Normalised 32-character id, or empty when the source did not have one.
    pub uuid: String,
    /// Where it came from, for the one-line report ("Prism", "Minecraft
    /// Launcher", or the file name).
    pub source: String,
}

impl ImportedAccount {
    /// Convert to a persisted row. Offline rows always carry a derived UUID;
    /// Microsoft rows keep theirs when the source had one.
    pub fn saved(&self) -> SavedAccount {
        let uuid = match self.kind {
            AccountKind::Offline => offline_profile(&self.name).uuid,
            AccountKind::Microsoft => self.uuid.clone(),
        };
        SavedAccount::new(self.kind, &self.name, &uuid)
    }
}

/// Everything one scan found, so the UI can report both counts and sources.
#[derive(Debug, Clone, Default)]
pub struct ImportScan {
    pub accounts: Vec<ImportedAccount>,
    pub files: Vec<String>,
}

/// Account files that exist on this machine, in a stable order. A missing file
/// is simply absent; nothing here creates or edits another launcher's data.
pub fn known_account_files() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(appdata) = std::env::var_os("APPDATA") {
        roots.push(PathBuf::from(appdata));
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        roots.push(PathBuf::from(data));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        roots.push(home.join(".local").join("share"));
        roots.push(home.join("Library").join("Application Support"));
        roots.push(home.clone());
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        roots.push(PathBuf::from(profile).join("AppData").join("Roaming"));
    }
    let mut files: Vec<PathBuf> = Vec::new();
    let mut add = |candidate: PathBuf| {
        if candidate.is_file() && !files.contains(&candidate) {
            files.push(candidate);
        }
    };
    for root in &roots {
        for launcher in ["PrismLauncher", "MultiMC", "PolyMC"] {
            add(root.join(launcher).join("accounts.json"));
        }
        add(root.join(".minecraft").join("launcher_accounts.json"));
        // A home root also covers the portable layout where `.minecraft` is
        // itself the root that was passed in.
        add(root.join("launcher_accounts.json"));
    }
    files
}

/// Read every known account file. Unreadable or unrecognised files are
/// skipped: one launcher's odd file must not block the others.
pub fn scan_known_files() -> ImportScan {
    let mut scan = ImportScan::default();
    for path in known_account_files() {
        let label = source_label(&path);
        match import_file(&path) {
            Ok(mut accounts) => {
                for account in &mut accounts {
                    account.source = label.clone();
                }
                scan.files.push(label);
                scan.accounts.extend(accounts);
            }
            Err(_) => continue,
        }
    }
    dedupe(&mut scan.accounts);
    scan
}

/// Read one file. Works for an account list, a launcher's whole data file, or
/// a hand-written JSON with player records.
pub fn import_file(path: &Path) -> Result<Vec<ImportedAccount>, ImportError> {
    import_text(&std::fs::read_to_string(path)?)
}

/// Parse JSON text. Public so a pasted document can be imported without a
/// temporary file.
pub fn import_text(text: &str) -> Result<Vec<ImportedAccount>, ImportError> {
    let value: Value = serde_json::from_str(text).map_err(|_| ImportError::Json)?;
    let mut found = Vec::new();
    collect(&value, "file", 0, &mut found);
    dedupe(&mut found);
    if found.is_empty() {
        Err(ImportError::Empty)
    } else {
        Ok(found)
    }
}

fn source_label(path: &Path) -> String {
    let parent = path
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().to_string());
    match parent.as_deref() {
        Some(".minecraft") | Some("minecraft") => "Minecraft Launcher".to_owned(),
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => path
            .file_stem()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_owned()),
    }
}

fn collect(value: &Value, source: &str, depth: usize, out: &mut Vec<ImportedAccount>) {
    if depth > 6 {
        return;
    }
    match value {
        Value::Array(items) => {
            for item in items {
                collect(item, source, depth + 1, out);
            }
        }
        Value::Object(map) => {
            // Official launcher: `accounts` maps a local id to a record with
            // `minecraftProfile`.
            if let Some(Value::Object(accounts)) = map.get("accounts") {
                for entry in accounts.values() {
                    if let Some(profile) = entry.get("minecraftProfile") {
                        push_profile(profile, AccountKind::Microsoft, source, out);
                    }
                }
            }
            // Prism / MultiMC / PolyMC: a typed entry with `ygg.extra`.
            let kind = kind_of(map.get("type").and_then(Value::as_str));
            if let Some(extra) = map
                .get("ygg")
                .and_then(|ygg| ygg.get("extra"))
                .filter(|extra| extra.is_object())
            {
                push_profile(extra, kind, source, out);
            } else if looks_like_player(map) {
                push_profile(value, kind, source, out);
            }
            for (key, child) in map {
                if !(child.is_array() || child.is_object()) {
                    continue;
                }
                // `accounts` was consumed above and `ygg` holds a token, not
                // another profile.
                if matches!(key.as_str(), "accounts" | "ygg") {
                    continue;
                }
                collect(child, source, depth + 1, out);
            }
        }
        _ => {}
    }
}

fn push_profile(value: &Value, kind: AccountKind, source: &str, out: &mut Vec<ImportedAccount>) {
    let Some(name) = player_name(value) else {
        return;
    };
    if !crate::auth::valid_username(&name) {
        return;
    }
    let uuid = player_uuid(value);
    out.push(ImportedAccount {
        kind,
        name,
        uuid,
        source: source.to_owned(),
    });
}

fn player_name(value: &Value) -> Option<String> {
    value.as_object().and_then(object_name)
}

fn object_name(map: &serde_json::Map<String, Value>) -> Option<String> {
    const KEYS: [&str; 6] = [
        "name",
        "username",
        "userName",
        "playerName",
        "displayName",
        "profileName",
    ];
    KEYS.iter()
        .find_map(|key| map.get(*key).and_then(Value::as_str))
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
}

fn player_uuid(value: &Value) -> String {
    value.as_object().map(object_uuid).unwrap_or_default()
}

fn object_uuid(map: &serde_json::Map<String, Value>) -> String {
    const KEYS: [&str; 5] = ["uuid", "profileId", "playerId", "id", "minecraftProfile"];
    for key in KEYS {
        if let Some(text) = map.get(key).and_then(Value::as_str) {
            let normalised = normalise_uuid(text);
            if !normalised.is_empty() {
                return normalised;
            }
        }
    }
    String::new()
}

fn normalise_uuid(text: &str) -> String {
    let digits: String = text
        .chars()
        .filter(|character| !matches!(character, '-' | '{' | '}'))
        .collect();
    if digits.len() == 32
        && digits
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        digits.to_ascii_lowercase()
    } else {
        String::new()
    }
}

/// An object counts as a player only when a legal username is paired with a
/// UUID-shaped id, a token, or an explicit account type. Without that, an
/// instance record (`{"name": "Skyblock Lab", "id": "demo"}`) would be
/// imported as an account.
fn looks_like_player(map: &serde_json::Map<String, Value>) -> bool {
    if object_name(map).is_none() {
        return false;
    }
    let has_uuid = !object_uuid(map).is_empty();
    let has_token = map.keys().any(|key| {
        let key = key.to_ascii_lowercase();
        key.contains("token") || key == "type" || key == "accounttype"
    });
    has_uuid || has_token
}

fn kind_of(label: Option<&str>) -> AccountKind {
    let label = label.unwrap_or_default().to_ascii_lowercase();
    if label.contains("offline") || label.contains("local") {
        AccountKind::Offline
    } else if label.contains("msa")
        || label.contains("xbox")
        || label.contains("microsoft")
        || label.contains("mojang")
    {
        AccountKind::Microsoft
    } else {
        // Unknown type with no credential: a local profile works and cannot
        // promise an online session it does not have.
        AccountKind::Offline
    }
}

fn dedupe(accounts: &mut Vec<ImportedAccount>) {
    let mut seen = std::collections::HashSet::new();
    accounts.retain(|account| seen.insert((account.kind, account.name.to_ascii_lowercase())));
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRISM: &str = r#"[
        {
            "type": "MSA",
            "ygg": {
                "extra": { "userName": "Notch", "uuid": "069a79f444e94726a5befca90e38aaf5" }
            },
            "accessToken": "secret"
        },
        {
            "type": "Offline",
            "ygg": { "extra": { "userName": "Builder", "uuid": "9c5d1d5f5c0a3c6e8f0d2b4a6c8e0f2a" } }
        }
    ]"#;

    const OFFICIAL: &str = r#"{
        "accounts": {
            "abc": {
                "type": "Xbox",
                "minecraftProfile": {
                    "id": "069a79f4-44e9-4726-a5be-fca90e38aaf5",
                    "name": "Notch"
                }
            }
        }
    }"#;

    #[test]
    fn prism_keeps_online_and_offline_rows_apart() {
        let accounts = import_text(PRISM).expect("prism list");
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].kind, AccountKind::Microsoft);
        assert_eq!(accounts[0].name, "Notch");
        assert_eq!(accounts[0].uuid, "069a79f444e94726a5befca90e38aaf5");
        assert_eq!(accounts[1].kind, AccountKind::Offline);
        assert_eq!(accounts[1].name, "Builder");
    }

    #[test]
    fn official_launcher_file_is_understood() {
        let accounts = import_text(OFFICIAL).expect("official file");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].kind, AccountKind::Microsoft);
        assert_eq!(accounts[0].uuid, "069a79f444e94726a5befca90e38aaf5");
    }

    #[test]
    fn a_loose_player_record_is_enough() {
        let accounts = import_text(
            r#"{"profiles":[{"username":"Alex","id":"ec561538f3fd461d8a4e6f0a5a8f7e09"}]}"#,
        )
        .expect("loose record");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].name, "Alex");
        assert_eq!(accounts[0].kind, AccountKind::Offline);
    }

    #[test]
    fn instance_records_are_not_accounts() {
        // A Kaelis instance has a name and a non-UUID id; importing it would
        // silently create a fake player.
        let text = r#"[{"id":"demo","name":"Skyblock Lab","mc_version":"1.21.1"}]"#;
        assert!(matches!(import_text(text), Err(ImportError::Empty)));
        assert!(matches!(import_text("not json"), Err(ImportError::Json)));
    }

    #[test]
    fn duplicates_collapse_and_offline_rows_get_a_uuid() {
        let text = r#"[
            {"type":"Offline","ygg":{"extra":{"userName":"Builder"}}},
            {"type":"Offline","ygg":{"extra":{"userName":"builder"}}}
        ]"#;
        let accounts = import_text(text).expect("two spellings of one name");
        assert_eq!(accounts.len(), 1);
        let saved = accounts[0].saved();
        assert_eq!(saved.uuid, offline_profile("Builder").uuid);
        assert!(normalise_uuid(&saved.uuid).len() == 32);
    }

    #[test]
    fn illegal_names_are_refused() {
        let text = r#"[{"type":"Offline","ygg":{"extra":{"userName":"not a name"}}}]"#;
        assert!(matches!(import_text(text), Err(ImportError::Empty)));
    }
}
