//! Offline profiles use the same MD5-based UUID name algorithm as the
//! vanilla Java launcher (`UUID.nameUUIDFromBytes`).

use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_HEAD_BYTES: u64 = 2 * 1024 * 1024;
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

pub async fn fetch_crafatar_head(
    client: &reqwest::Client,
    uuid: &str,
    size: u32,
) -> Result<Vec<u8>, AvatarError> {
    let mut response = client
        .get(crafatar_head_url(uuid, size))
        .send()
        .await?
        .error_for_status()?;
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
