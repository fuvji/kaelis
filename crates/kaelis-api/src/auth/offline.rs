//! Offline profiles use the same MD5-based UUID name algorithm as the
//! vanilla Java launcher (`UUID.nameUUIDFromBytes`).

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_HEAD_BYTES: u64 = 2 * 1024 * 1024;
const MOJANG_PROFILE_URL: &str = "https://api.mojang.com/users/profiles/minecraft";
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfflineProfile {
    pub name: String,
    pub uuid: String,
}

#[derive(Debug, Error)]
pub enum AvatarError {
    #[error("avatar request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("avatar exceeds the 2 MiB safety limit")]
    TooLarge,
    #[error("avatar lookup returned HTTP {0}")]
    Status(reqwest::StatusCode),
}

#[derive(Debug, Deserialize)]
struct MojangProfile {
    id: String,
}

/// Whether `name` is a syntactically valid Java username.
pub fn valid_username(name: &str) -> bool {
    (3..=16).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Resolve a Java username to the premium account's UUID.
///
/// `Ok(None)` means the name is free (no premium account owns it), so the
/// vanilla default skin applies. Only network failures are errors, letting the
/// caller keep the previously shown head instead of flashing an empty one.
pub async fn lookup_premium_uuid(
    client: &reqwest::Client,
    name: &str,
) -> Result<Option<String>, AvatarError> {
    let name = name.trim();
    if !valid_username(name) {
        return Ok(None);
    }
    let response = client
        .get(format!("{MOJANG_PROFILE_URL}/{name}"))
        .send()
        .await?;
    if matches!(
        response.status(),
        reqwest::StatusCode::NO_CONTENT | reqwest::StatusCode::NOT_FOUND
    ) {
        return Ok(None);
    }
    if !response.status().is_success() {
        return Err(AvatarError::Status(response.status()));
    }
    let profile: MojangProfile = response.json().await?;
    let uuid: String = profile
        .id
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit() || *ch == '-')
        .collect();
    Ok((!uuid.is_empty()).then_some(uuid))
}

/// Head render for a username, including offline profiles.
///
/// The premium account owning the name supplies the skin; names nobody owns
/// return `Ok(None)` so the caller draws the vanilla Steve face.
pub async fn fetch_head_for_username(
    client: &reqwest::Client,
    name: &str,
    size: u32,
) -> Result<Option<Vec<u8>>, AvatarError> {
    match lookup_premium_uuid(client, name).await? {
        Some(uuid) => fetch_crafatar_head(client, &uuid, size).await.map(Some),
        None => Ok(None),
    }
}

pub fn offline_profile(name: &str) -> OfflineProfile {
    let uuid = uuid_v3(&format!("OfflinePlayer:{name}"));
    OfflineProfile {
        name: name.to_owned(),
        uuid,
    }
}

/// Crafatar head render URL for GUI avatar fetching.
pub fn crafatar_head_url(uuid: &str, size: u32) -> String {
    let safe_id: String = uuid
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit() || *ch == '-')
        .collect();
    let size = size.clamp(8, 512);
    format!("https://crafatar.com/avatars/{safe_id}?size={size}&overlay")
}

/// Head URLs in fallback order. Crafatar answers with the classic flat render;
/// when it errors (it does, per-uuid, for skins it cannot rasterise) the other
/// two mirrors render the same skin from the same UUID.
fn head_urls(uuid: &str, size: u32) -> [String; 3] {
    let safe_id: String = uuid
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit() || *ch == '-')
        .collect::<String>()
        .trim_matches('-')
        .to_owned();
    let size = size.clamp(8, 512);
    [
        crafatar_head_url(uuid, size),
        format!("https://mc-heads.net/avatar/{safe_id}/{size}"),
        format!("https://minotar.net/helm/{safe_id}/{size}.png"),
    ]
}

async fn download_head(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, AvatarError> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_HEAD_BYTES)
    {
        return Err(AvatarError::TooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() as u64 + chunk.len() as u64 > MAX_HEAD_BYTES {
            return Err(AvatarError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Fetch a rendered head, trying each mirror until one answers.
pub async fn fetch_crafatar_head(
    client: &reqwest::Client,
    uuid: &str,
    size: u32,
) -> Result<Vec<u8>, AvatarError> {
    let urls = head_urls(uuid, size);
    let mut last_error = None;
    for url in urls {
        match download_head(client, &url).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or(AvatarError::TooLarge))
}

fn uuid_v3(input: &str) -> String {
    use md5::{Digest, Md5};

    let digest = Md5::digest(input.as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest);
    bytes[6] = (bytes[6] & 0x0f) | 0x30;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

#[cfg(test)]
mod tests {
    use super::{head_urls, offline_profile, valid_username};

    #[test]
    fn usernames_match_vanilla_rules() {
        assert!(valid_username("Notch"));
        assert!(valid_username("a_b_C123"));
        assert!(!valid_username("ab"));
        assert!(!valid_username("has space"));
        assert!(!valid_username("toolongusername17"));
        assert!(!valid_username("naïve"));
        assert!(!valid_username(""));
    }

    #[test]
    fn head_urls_are_mirrored_and_sanitised() {
        let id = "069a79f4-44e9-4726-a5be-fca90e38aaf5";
        let urls = head_urls(id, 64);
        assert!(urls[0].starts_with("https://crafatar.com/avatars/"));
        assert!(urls[1].starts_with("https://mc-heads.net/avatar/"));
        assert!(urls[2].starts_with("https://minotar.net/helm/"));
        for url in &urls {
            assert!(url.contains(id), "{url}");
            assert!(!url.contains(".."), "{url}");
        }
        // A hostile or malformed id cannot add a path segment or a query of
        // its own; the crafatar render URL keeps its own `?size=` query.
        for url in &head_urls("../evil?x=1", 64) {
            assert!(!url.contains(".."), "{url}");
            assert!(!url.contains("x=1"), "{url}");
            assert!(!url.contains("?x"), "{url}");
        }
        // Out-of-range sizes are clamped, not rejected.
        assert!(head_urls("abc", 1)[0].contains("size=8"));
        assert!(head_urls("abc", 4096)[0].contains("size=512"));
    }

    #[test]
    fn offline_uuid_is_the_vanilla_offline_player_uuid() {
        // Known reference value for `OfflinePlayer:Notch`.
        assert_eq!(
            offline_profile("Notch").uuid,
            "b50ad385-829d-3141-a216-7e7d7539ba7f"
        );
    }
}
