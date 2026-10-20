//! Launcher identity constants. Single source of truth for name + hub link.

/// Launcher display name.
pub const LAUNCHER_NAME: &str = "Kaelis";
/// Product studio shown in About, the site, and release metadata.
pub const COMPANY: &str = "Kaelis Labs";
/// Founder handle. Kept as a display value, never a credential.
pub const FOUNDER: &str = "@epufk";
pub const FOUNDER_URL: &str = "https://github.com/epufk";
/// Canonical source repository for the launcher and the static site.
pub const GITHUB_REPOSITORY: &str = "https://github.com/epufk/kaelis";
/// Public project home used by the in-app About card.
pub const COMPANY_URL: &str = GITHUB_REPOSITORY;
/// Public product site deployed from `site/`.
pub const WEBSITE: &str = "https://epufk.github.io/kaelis/";
/// Short product line used in the UI and site metadata.
pub const TAGLINE: &str = "Minecraft, composed.";
/// Official community hub. Shown in footer, settings, and error reports.
pub const DISCORD_HUB: &str = "https://discord.gg/fvrdYzrQjq";
/// Keyring service name for Microsoft refresh tokens.
pub const KEYRING_SERVICE: &str = "kaelis";
/// User-Agent sent to Mojang / Modrinth / CurseForge.
pub fn user_agent() -> String {
    format!(
        "{}/{} (+{})",
        LAUNCHER_NAME,
        env!("CARGO_PKG_VERSION"),
        DISCORD_HUB
    )
}
