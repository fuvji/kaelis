//! Kaelis API: Mojang versions, Modrinth + CurseForge clients,
//! parallel downloader, mrpack importer, hashing, and auth engine.
pub mod account_import;
pub mod accounts;
pub mod auth;
pub mod content;
pub mod curseforge;
pub mod downloader;
pub mod foreign;
pub mod hash;
pub mod health;
pub mod launch;
pub mod loader;
pub mod modmeta;
pub mod modrinth;
pub mod mods;
pub mod mrpack;
pub mod portable;
pub mod power;
pub mod release;
pub mod repair;
pub mod retry;
pub mod runtime;
pub mod secrets;
pub mod updater;
pub mod updates;
pub mod vanilla;
pub mod versions;

pub use accounts::{
    AccountKind, AccountMode, LauncherSettings, SavedAccount, SettingsError, active_account,
    load_settings, migrated_accounts, resolved_mode, save_settings, settings_path, switch_account,
};
pub use auth::{AuthSession, MicrosoftAuth, MinecraftProfile, offline_profile};
pub use health::Readiness;
pub use mods::{
    ContentKind, InstallReport, ModPlatformClient, ModProvider, ModSearchItem, ModSearchResults,
};
pub use portable::{ExportReport, ImportReport, PortableError, export_instance, import_instance};
pub use release::{LauncherRelease, is_newer, latest as latest_release};
pub use repair::{DuplicateRepair, RepairReport};
pub use updater::{PreparedUpdate, UpdateAsset};
pub use updates::{ModUpdate, UpdateError, UpdateReport, apply_update, check_updates};
