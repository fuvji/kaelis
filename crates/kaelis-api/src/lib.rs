//! Kaelis API: Mojang versions, Modrinth + CurseForge clients,
//! parallel downloader, mrpack importer, hashing, and auth engine.
pub mod auth;
pub mod curseforge;
pub mod downloader;
pub mod hash;
pub mod launch;
pub mod loader;
pub mod modmeta;
pub mod modrinth;
pub mod mods;
pub mod mrpack;
pub mod repair;
pub mod retry;
pub mod runtime;
pub mod secrets;
pub mod vanilla;
pub mod versions;

pub use auth::{AuthSession, MicrosoftAuth, MinecraftProfile, offline_profile};
pub use mods::{
    ContentKind, InstallReport, ModPlatformClient, ModProvider, ModSearchItem, ModSearchResults,
};
pub use repair::{DuplicateRepair, RepairReport};
