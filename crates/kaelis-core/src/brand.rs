//! Launcher identity constants. Single source of truth for name + hub link.

/// Launcher display name.
pub const LAUNCHER_NAME: &str = "Kaelis";
/// Product studio shown in About, the site, and release metadata.
pub const COMPANY: &str = "Kaelis Labs";
/// Founder handle. Kept as a display value, never a credential.
pub const FOUNDER: &str = "@epufk";
/// Founder profile link. The account URL moved; the handle shown beside it did not.
pub const FOUNDER_URL: &str = "https://github.com/fuvji";
/// Canonical source repository for the launcher and the static site.
pub const GITHUB_REPOSITORY: &str = "https://github.com/fuvji/kaelis";
/// Public project home used by the in-app About card.
pub const COMPANY_URL: &str = GITHUB_REPOSITORY;
/// Public product site deployed from `site/`.
pub const WEBSITE: &str = "https://fuvji.github.io/kaelis/";
/// Short product line used in the UI and site metadata.
pub const TAGLINE: &str = "Minecraft, composed.";
/// Official community hub. Shown in footer, settings, and error reports.
pub const DISCORD_HUB: &str = "https://discord.gg/fvrdYzrQjq";
/// Keyring service name for Microsoft refresh tokens.
pub const KEYRING_SERVICE: &str = "kaelis";
/// Checksums and build provenance published beside the downloads.
pub const CHECKSUMS_URL: &str = "https://fuvji.github.io/kaelis/downloads/SHA256SUMS.txt";

/// Host platform as `os-arch`, e.g. `windows-x86_64` or `macos-aarch64`.
pub fn build_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Version and build identity used by `--version`, the About card, and the
/// release provenance file.
pub fn build_identity() -> String {
    format!(
        "{} {} · {} · {}",
        LAUNCHER_NAME,
        env!("CARGO_PKG_VERSION"),
        build_platform(),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    )
}
/// User-Agent sent to Mojang / Modrinth / CurseForge.
pub fn user_agent() -> String {
    format!(
        "{}/{} (+{})",
        LAUNCHER_NAME,
        env!("CARGO_PKG_VERSION"),
        DISCORD_HUB
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The site, the source repository, and the founder link all live under one
    /// account. A rename must never leave a published URL on the old one.
    #[test]
    fn public_links_point_at_the_current_account() {
        for url in [FOUNDER_URL, GITHUB_REPOSITORY, WEBSITE, CHECKSUMS_URL] {
            assert!(url.starts_with("https://"), "{url} is not https");
            assert!(
                !url.contains("epufk."),
                "{url} still holds the old site host"
            );
            assert!(
                !url.contains("github.com/epufk"),
                "{url} still holds the old account path"
            );
        }
        assert_eq!(FOUNDER_URL, "https://github.com/fuvji");
        assert_eq!(GITHUB_REPOSITORY, "https://github.com/fuvji/kaelis");
        assert_eq!(WEBSITE, "https://fuvji.github.io/kaelis/");
        assert!(CHECKSUMS_URL.starts_with(WEBSITE));
        assert_eq!(COMPANY_URL, GITHUB_REPOSITORY);
        // The link moved; the credited handle did not.
        assert_eq!(FOUNDER, "@epufk");
        assert!(user_agent().contains(LAUNCHER_NAME));
    }
}
