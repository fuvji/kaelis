//! Saved identities and launcher settings.
//!
//! One canonical implementation is shared by the desktop app and the headless
//! CLI, so a command-line switch and a click in the Account tab produce the
//! same `settings.json`. Refresh tokens never live here: a row stores only the
//! identity, and Microsoft tokens stay in the OS credential store keyed by the
//! account name.

use crate::auth::{offline_profile, valid_username};
use crate::mods::ContentKind;
use kaelis_core::instance::Instance;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    InvalidName(String),
}

/// Which identity the next launch should prefer when both a saved Microsoft
/// session and an offline profile exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountMode {
    #[default]
    Microsoft,
    Offline,
}

/// Where a saved identity comes from. Microsoft accounts can join online-mode
/// servers; offline profiles cannot, whatever their name looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    Microsoft,
    Offline,
}

impl AccountKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Microsoft => "Microsoft",
            Self::Offline => "Offline",
        }
    }

    /// Stable prefix for the per-account head cache key.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Microsoft => "ms",
            Self::Offline => "local",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Microsoft => "microsoft",
            Self::Offline => "offline",
        }
    }
}

/// One row in the account manager. Only the identity is stored: refresh tokens
/// stay in the OS credential store, keyed by account name, and heads stay in
/// memory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedAccount {
    pub kind: AccountKind,
    pub name: String,
    /// Minecraft profile id. Microsoft rows learn it when the token is
    /// restored; offline rows derive it from the name.
    #[serde(default)]
    pub uuid: String,
    /// Seconds since the epoch, for ordering newest-first in the list.
    #[serde(default)]
    pub added_unix: i64,
}

impl SavedAccount {
    pub fn new(kind: AccountKind, name: &str, uuid: &str) -> Self {
        Self {
            kind,
            name: name.to_owned(),
            uuid: uuid.to_owned(),
            added_unix: unix_now(),
        }
    }

    /// Cache key for this row's head texture.
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind.prefix(), self.name.to_ascii_lowercase())
    }
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherSettings {
    #[serde(default)]
    pub last_account: String,
    /// Install missing required mods automatically before the next launch.
    #[serde(default = "default_auto_repair")]
    pub auto_repair: bool,
    /// Disable older duplicate primary mod ids after an install when the
    /// cleanup is provably safe.
    #[serde(default = "default_auto_repair")]
    pub auto_resolve_duplicates: bool,
    /// Last Content channel, so the tab reopens where the user left it.
    #[serde(default)]
    pub content_kind: ContentKind,
    /// Last offline profile name, so the premium-skinned local account
    /// survives a restart instead of having to be typed again.
    #[serde(default)]
    pub offline_account: String,
    /// The identity the user chose last. Without this, a saved Microsoft
    /// token silently replaced an explicit offline profile on every restart.
    /// Older settings files omit it, so they fall back to whichever identity
    /// was actually saved.
    #[serde(default)]
    pub account_mode: Option<AccountMode>,
    /// Every saved identity, newest last. Older files have none; the two
    /// legacy fields above are migrated into this list on load.
    #[serde(default)]
    pub accounts: Vec<SavedAccount>,
    /// Whole-interface scale, identical to egui's zoom factor. `0.0` means
    /// automatic: the window picks a step from its own display once, then the
    /// user's Ctrl +/- (or the Settings control) replaces it.
    #[serde(default)]
    pub ui_scale: f32,
    /// Start Kaelis when the user signs in, already hidden in the tray. The
    /// shortcut is a per-user registry value, so no service or elevation is
    /// involved and login is not delayed by a task scheduler.
    #[serde(default)]
    pub launch_at_startup: bool,
    /// Inner size of the launcher window in logical points, written once a
    /// resize settles. `[0.0, 0.0]` means "never resized", so a first launch
    /// opens at the default size instead of a guess. A maximized window is
    /// never recorded, so restoring never pins the launcher to a full screen.
    #[serde(default)]
    pub window_size: [f32; 2],
    /// Version-list filters, remembered so the list reopens the way it was
    /// left instead of resetting to releases-only on every start.
    #[serde(default = "default_true")]
    pub show_snapshots: bool,
    #[serde(default)]
    pub show_alpha: bool,
    #[serde(default)]
    pub show_beta: bool,
    /// Content studio switches: whether an install brings required
    /// dependencies, which providers a search covers, and whether typing runs
    /// the search. Each is one checkbox the user should not have to set twice.
    #[serde(default = "default_true")]
    pub install_dependencies: bool,
    #[serde(default = "default_true")]
    pub search_modrinth: bool,
    #[serde(default = "default_true")]
    pub search_curseforge: bool,
    #[serde(default = "default_true")]
    pub search_as_typed: bool,
}

fn default_auto_repair() -> bool {
    true
}

fn default_true() -> bool {
    true
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self {
            last_account: String::new(),
            auto_repair: default_auto_repair(),
            auto_resolve_duplicates: default_auto_repair(),
            content_kind: ContentKind::Mod,
            offline_account: String::new(),
            account_mode: None,
            accounts: Vec::new(),
            ui_scale: 0.0,
            launch_at_startup: false,
            window_size: [0.0, 0.0],
            show_snapshots: true,
            show_alpha: false,
            show_beta: false,
            install_dependencies: true,
            search_modrinth: true,
            search_curseforge: true,
            search_as_typed: true,
        }
    }
}

/// Merge the saved list with the legacy single-account fields, dropping
/// duplicates and anything that cannot be an identity.
pub fn migrated_accounts(settings: &LauncherSettings) -> Vec<SavedAccount> {
    let mut accounts: Vec<SavedAccount> = Vec::with_capacity(settings.accounts.len() + 2);
    let mut seen = std::collections::HashSet::new();
    for account in &settings.accounts {
        let usable = match account.kind {
            AccountKind::Offline => valid_username(account.name.trim()),
            AccountKind::Microsoft => !account.name.trim().is_empty(),
        };
        if usable && seen.insert(account.key()) {
            let mut account = account.clone();
            // An offline UUID is derived from the name, so it can always be
            // filled in; a Microsoft id only arrives with a live session.
            if account.kind == AccountKind::Offline && account.uuid.is_empty() {
                account.uuid = offline_profile(&account.name).uuid;
            }
            accounts.push(account);
        }
    }
    let offline = settings.offline_account.trim();
    if valid_username(offline)
        && !accounts.iter().any(|account| {
            account.kind == AccountKind::Offline && account.name.eq_ignore_ascii_case(offline)
        })
    {
        let profile = offline_profile(offline);
        accounts.push(SavedAccount::new(
            AccountKind::Offline,
            &profile.name,
            &profile.uuid,
        ));
    }
    let microsoft = settings.last_account.trim();
    if !microsoft.is_empty()
        && !accounts.iter().any(|account| {
            account.kind == AccountKind::Microsoft && account.name.eq_ignore_ascii_case(microsoft)
        })
    {
        accounts.push(SavedAccount::new(AccountKind::Microsoft, microsoft, ""));
    }
    accounts
}

/// The mode an older settings file implicitly used before `account_mode` was
/// written. A saved Microsoft token kept its previous precedence; otherwise an
/// offline profile did.
pub fn resolved_mode(settings: &LauncherSettings) -> AccountMode {
    settings.account_mode.unwrap_or(
        if valid_username(settings.offline_account.trim()) && settings.last_account.is_empty() {
            AccountMode::Offline
        } else {
            AccountMode::Microsoft
        },
    )
}

/// The row the saved mode and names point at, if any.
pub fn active_account<'a>(
    settings: &LauncherSettings,
    accounts: &'a [SavedAccount],
) -> Option<&'a SavedAccount> {
    let (kind, name) = match resolved_mode(settings) {
        AccountMode::Offline => (AccountKind::Offline, settings.offline_account.trim()),
        AccountMode::Microsoft => (AccountKind::Microsoft, settings.last_account.trim()),
    };
    if name.is_empty() {
        return accounts.iter().find(|account| account.kind == kind);
    }
    accounts
        .iter()
        .find(|account| account.kind == kind && account.name.eq_ignore_ascii_case(name))
        .or_else(|| {
            // A live Microsoft session can be the active identity even when
            // legacy settings never named it.
            (kind == AccountKind::Microsoft)
                .then(|| accounts.iter().find(|account| account.kind == kind))
                .flatten()
        })
}

/// `settings.json`, beside the instances directory.
pub fn settings_path() -> PathBuf {
    data_dir().join("settings.json")
}

fn data_dir() -> PathBuf {
    let instances = Instance::instances_dir();
    instances
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| instances.clone())
}

/// Read settings, falling back to defaults for a missing or malformed file.
pub fn load_settings() -> LauncherSettings {
    try_load_settings().unwrap_or_default()
}

/// Read settings, reporting a corrupt file instead of silently resetting it.
pub fn try_load_settings() -> Result<LauncherSettings, SettingsError> {
    let path = settings_path();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LauncherSettings::default());
        }
        Err(error) => return Err(SettingsError::Io(error)),
    };
    parse_settings(&bytes)
}

/// Parse a settings document, tolerating the UTF-8 BOM that Notepad and
/// Windows PowerShell add. serde_json rejects the BOM, and the old fallback
/// then reset every setting — accounts and UI scale included.
fn parse_settings(bytes: &[u8]) -> Result<LauncherSettings, SettingsError> {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    Ok(serde_json::from_slice(bytes)?)
}

/// Write settings through a same-directory temporary file.
///
/// The temporary file is flushed to disk before the rename, so a crash or a
/// power loss leaves either the previous settings or the new ones, never a
/// half-written document. The rename replaces the old file atomically on both
/// Windows and Unix.
pub fn save_settings(settings: &LauncherSettings) -> Result<(), SettingsError> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(settings)?;
    // A unique staging name means two processes writing settings at once can
    // never interleave inside one temporary file; the last rename wins whole.
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = path.with_file_name(format!(".settings.{}.{sequence}.tmp", std::process::id()));
    let written = (|| -> Result<(), SettingsError> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, &path)?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// Switch the saved identity, adding an offline name when it is not saved yet.
///
/// This only changes what the next launch prefers; it never reads, writes, or
/// deletes a credential. A Microsoft row therefore needs a token that was
/// stored by a previous GUI sign-in.
pub fn switch_account(name: &str) -> Result<SavedAccount, SettingsError> {
    let name = name.trim();
    let mut settings = try_load_settings()?;
    let mut accounts = migrated_accounts(&settings);
    let existing = accounts
        .iter()
        .position(|account| account.name.eq_ignore_ascii_case(name));
    let index = match existing {
        Some(index) => index,
        None => {
            if !valid_username(name) {
                return Err(SettingsError::InvalidName(format!(
                    "“{name}” is not a saved account and is not a valid offline username"
                )));
            }
            let profile = offline_profile(name);
            accounts.push(SavedAccount::new(
                AccountKind::Offline,
                &profile.name,
                &profile.uuid,
            ));
            accounts.len() - 1
        }
    };
    let account = accounts[index].clone();
    // Keep the canonical, deduped list so a legacy file is migrated once.
    settings.accounts = accounts;
    settings.account_mode = Some(match account.kind {
        AccountKind::Microsoft => AccountMode::Microsoft,
        AccountKind::Offline => AccountMode::Offline,
    });
    match account.kind {
        AccountKind::Microsoft => settings.last_account = account.name.clone(),
        AccountKind::Offline => settings.offline_account = account.name.clone(),
    }
    // A freshly added offline row persists its derived UUID too.
    if let Some(saved) = settings
        .accounts
        .iter_mut()
        .find(|saved| saved.kind == account.kind && saved.name.eq_ignore_ascii_case(&account.name))
        && saved.uuid.is_empty()
    {
        saved.uuid = account.uuid.clone();
    }
    save_settings(&settings)?;
    Ok(account)
}

#[cfg(test)]
mod tests {
    use super::{
        AccountKind, AccountMode, LauncherSettings, SavedAccount, active_account,
        migrated_accounts, parse_settings, resolved_mode,
    };

    #[test]
    fn legacy_settings_migrate_into_the_account_list() {
        let settings = LauncherSettings {
            last_account: "Notch".to_owned(),
            offline_account: "Builder".to_owned(),
            ..LauncherSettings::default()
        };
        let accounts = migrated_accounts(&settings);
        assert_eq!(accounts.len(), 2);
        let offline = accounts
            .iter()
            .find(|account| account.kind == AccountKind::Offline)
            .expect("offline row");
        assert_eq!(offline.name, "Builder");
        assert_eq!(offline.uuid, crate::auth::offline_profile("Builder").uuid);
        let microsoft = accounts
            .iter()
            .find(|account| account.kind == AccountKind::Microsoft)
            .expect("microsoft row");
        // A Microsoft uuid is learned when its token is restored, not guessed.
        assert!(microsoft.uuid.is_empty());
    }

    #[test]
    fn the_account_list_drops_duplicates_and_invalid_names() {
        let settings = LauncherSettings {
            offline_account: "Builder".to_owned(),
            accounts: vec![
                SavedAccount::new(AccountKind::Offline, "Builder", "abc"),
                SavedAccount::new(AccountKind::Offline, "builder", "abc"),
                SavedAccount::new(AccountKind::Microsoft, "Notch", ""),
                SavedAccount::new(AccountKind::Microsoft, "Notch", ""),
                SavedAccount::new(AccountKind::Offline, "toolongname1234567", ""),
            ],
            ..LauncherSettings::default()
        };
        let accounts = migrated_accounts(&settings);
        assert_eq!(accounts.len(), 2, "{accounts:?}");
        assert_eq!(accounts[0].name, "Builder");
        assert_eq!(accounts[1].name, "Notch");
        // The two kinds never collide, even with the same display name.
        assert_ne!(accounts[0].key(), accounts[1].key());
    }

    #[test]
    fn retired_window_flags_do_not_break_old_settings() {
        let defaults = LauncherSettings::default();
        assert!(!defaults.launch_at_startup);
        // Files written before tray minimize and junk cleanup became core
        // behaviour still carry both keys. They must load and be ignored, so
        // an old "off" cannot disable either one.
        let legacy: LauncherSettings = serde_json::from_str(
            "{\"auto_clean\":false,\"minimize_to_tray\":false,\"launch_at_startup\":true}",
        )
        .expect("legacy settings");
        assert!(legacy.launch_at_startup);
    }

    #[test]
    fn ui_switches_and_window_size_survive_a_round_trip() {
        let settings = LauncherSettings {
            window_size: [1280.0, 720.0],
            show_snapshots: false,
            show_alpha: true,
            ..LauncherSettings::default()
        };
        // The defaults are still the useful ones: releases only, required
        // dependencies on, both providers on, search as typed on.
        assert!(settings.install_dependencies);
        assert!(settings.search_modrinth);
        assert!(settings.search_curseforge);
        assert!(settings.search_as_typed);
        let decoded: LauncherSettings =
            serde_json::from_str(&serde_json::to_string(&settings).expect("encode"))
                .expect("decode");
        assert_eq!(decoded.window_size, [1280.0, 720.0]);
        assert!(!decoded.show_snapshots);
        assert!(decoded.show_alpha);
        // A file written before these keys existed opens with the same
        // defaults it always had, not with a feature switched off.
        let legacy: LauncherSettings = serde_json::from_str("{}").expect("legacy settings");
        assert_eq!(legacy.window_size, [0.0, 0.0]);
        assert!(legacy.show_snapshots);
        assert!(legacy.install_dependencies);
        assert!(legacy.search_as_typed);
    }

    #[test]
    fn accounts_round_trip_through_settings_json() {
        let settings = LauncherSettings {
            accounts: vec![
                SavedAccount::new(AccountKind::Microsoft, "Notch", "069a79f4"),
                SavedAccount::new(AccountKind::Offline, "Builder", "1b2c3d4e"),
            ],
            ..LauncherSettings::default()
        };
        let encoded = serde_json::to_string(&settings).expect("encode");
        let decoded: LauncherSettings = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(decoded.accounts, settings.accounts);
    }

    #[test]
    fn legacy_mode_follows_the_saved_identity() {
        let offline_only = LauncherSettings {
            offline_account: "Builder".to_owned(),
            ..LauncherSettings::default()
        };
        assert_eq!(resolved_mode(&offline_only), AccountMode::Offline);
        let microsoft = LauncherSettings {
            offline_account: "Builder".to_owned(),
            last_account: "Notch".to_owned(),
            ..LauncherSettings::default()
        };
        assert_eq!(resolved_mode(&microsoft), AccountMode::Microsoft);
    }

    #[test]
    fn active_account_matches_the_saved_mode_and_name() {
        let accounts = vec![
            SavedAccount::new(AccountKind::Microsoft, "Notch", "069a79f4"),
            SavedAccount::new(AccountKind::Offline, "Builder", "1b2c3d4e"),
        ];
        let microsoft = LauncherSettings {
            account_mode: Some(AccountMode::Microsoft),
            last_account: "notch".to_owned(),
            ..LauncherSettings::default()
        };
        assert_eq!(
            active_account(&microsoft, &accounts).map(|account| account.name.as_str()),
            Some("Notch")
        );
        let offline = LauncherSettings {
            account_mode: Some(AccountMode::Offline),
            offline_account: "Builder".to_owned(),
            ..LauncherSettings::default()
        };
        assert_eq!(
            active_account(&offline, &accounts).map(|account| account.name.as_str()),
            Some("Builder")
        );
    }

    #[test]
    fn a_bom_prefixed_settings_file_keeps_every_setting() {
        let document = "\u{feff}{\"offline_account\":\"Builder\",\"ui_scale\":1.25}";
        let settings = parse_settings(document.as_bytes()).expect("BOM settings");
        assert_eq!(settings.offline_account, "Builder");
        assert_eq!(settings.ui_scale, 1.25);
        // Without the strip the same bytes are a hard parse error, which is
        // exactly the silent-reset regression this guards.
        assert!(serde_json::from_slice::<LauncherSettings>(document.as_bytes()).is_err());
    }

    #[test]
    fn a_retired_accent_field_does_not_reset_settings() {
        // The interface has one fixed blue now. A settings file written by a
        // build that still had the picker must keep loading.
        let document = "{\"offline_account\":\"Builder\",\"ui_scale\":1.25,\"accent\":[255,0,0]}";
        let settings = parse_settings(document.as_bytes()).expect("retired accent");
        assert_eq!(settings.offline_account, "Builder");
        assert_eq!(settings.ui_scale, 1.25);
    }

    #[test]
    fn a_missing_name_falls_back_to_the_saved_kind() {
        let accounts = vec![SavedAccount::new(AccountKind::Offline, "Builder", "abc")];
        let settings = LauncherSettings {
            account_mode: Some(AccountMode::Offline),
            ..LauncherSettings::default()
        };
        assert_eq!(
            active_account(&settings, &accounts).map(|account| account.name.as_str()),
            Some("Builder")
        );
    }
}
