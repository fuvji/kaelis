//! Microsoft device authorization → Xbox Live → XSTS → Minecraft Services.
//! Microsoft refresh credentials stay in the OS keychain; live access tokens
//! are held in zeroizing RAM wrappers and are never serializable.

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

mod offline;
pub use offline::{
    AvatarError, OfflineProfile, crafatar_head_url, fetch_crafatar_head, fetch_head_for_username,
    lookup_premium_uuid, offline_profile, valid_username,
};

/// The public Microsoft account client every Xbox Live relying party may use,
/// the same one the Xbox app and the console family sign in with. Bundling it
/// removes every registration step: a fresh install shows a code immediately.
/// Tokens from it are still exchanged through Xbox Live → XSTS → Minecraft
/// Services, so an account that does not own Java Edition is refused as before.
const PUBLIC_CLIENT_ID: &str = "00000000402b5328";
const LIVE_DEVICE_CODE_URL: &str = "https://login.live.com/oauth20_connect.srf";
const LIVE_TOKEN_URL: &str = "https://login.live.com/oauth20_token.srf";
const LIVE_SCOPE: &str = "service::user.auth.xboxlive.com::MBI_SSL";
/// The registered-application flow: a client ID the player owns, the documented
/// consumer-tenant endpoints, and the `XboxLive.signin` scope.
const ENTRA_DEVICE_CODE_URL: &str =
    "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const ENTRA_TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const ENTRA_SCOPE: &str = "XboxLive.signin offline_access";
/// A poll that never answers must not spin forever: three failed polls in a
/// row end the attempt instead of hammering the endpoint.
const MAX_POLL_FAILURES: u32 = 3;
const XBOX_AUTH_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MC_LOGIN_URL: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const MC_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("keyring operation failed: {0}")]
    Keyring(String),
    #[error("Microsoft authentication rejected the request ({0})")]
    Rejected(String),
    #[error("device authorization expired; start a new sign-in")]
    Expired,
    #[error("device authorization was cancelled")]
    Cancelled,
}

/// Which Microsoft front door issued the access token.
///
/// The ticket handed to Xbox Live is not the same for both. A token from the
/// legacy Live client is an `MBI_SSL` ticket and is sent verbatim, while a
/// v2.0 (Entra) token is domain-prefixed with `d=`. Presenting the wrong form
/// is answered with HTTP 401 at `user.auth.xboxlive.com`, so the flow and its
/// encoding are decided together here instead of at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Live,
    Entra,
}

impl Flow {
    fn device_code_url(self) -> &'static str {
        match self {
            Self::Live => LIVE_DEVICE_CODE_URL,
            Self::Entra => ENTRA_DEVICE_CODE_URL,
        }
    }

    fn token_url(self) -> &'static str {
        match self {
            Self::Live => LIVE_TOKEN_URL,
            Self::Entra => ENTRA_TOKEN_URL,
        }
    }

    fn scope(self) -> &'static str {
        match self {
            Self::Live => LIVE_SCOPE,
            Self::Entra => ENTRA_SCOPE,
        }
    }

    /// Candidate `RpsTicket` encodings, most likely first. Both are tried
    /// because the accepted form follows the issuing client, and a refusal is a
    /// single cheap request rather than a dead end for the player.
    fn rps_tickets(self, access_token: &str) -> [String; 2] {
        match self {
            Self::Live => [access_token.to_owned(), format!("d={access_token}")],
            Self::Entra => [format!("d={access_token}"), access_token.to_owned()],
        }
    }
}

#[derive(Clone, Deserialize)]
pub struct DeviceCodeResponse {
    pub user_code: String,
    pub device_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    #[serde(default = "default_interval")]
    pub interval: u64,
    pub expires_in: u64,
    #[serde(default)]
    pub message: Option<String>,
}

impl Drop for DeviceCodeResponse {
    fn drop(&mut self) {
        self.device_code.zeroize();
    }
}

fn default_interval() -> u64 {
    5
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
}

impl TokenResponse {
    fn into_secrets(mut self) -> (Zeroizing<String>, Option<Zeroizing<String>>) {
        let access = Zeroizing::new(std::mem::take(&mut self.access_token));
        let refresh = self.refresh_token.take().map(Zeroizing::new);
        (access, refresh)
    }
}

impl Drop for TokenResponse {
    fn drop(&mut self) {
        self.access_token.zeroize();
        if let Some(refresh) = &mut self.refresh_token {
            refresh.zeroize();
        }
    }
}

#[derive(Deserialize)]
struct TokenErrorResponse {
    error: String,
    #[serde(default)]
    error_description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct MinecraftProfile {
    pub id: String,
    pub name: String,
}

pub struct AuthSession {
    pub profile: MinecraftProfile,
    /// Whether the refresh token reached the OS credential store. A failure
    /// here must not erase a sign-in that already succeeded: the session is
    /// usable for this run and the UI says it will not survive a restart.
    pub token_saved: bool,
    minecraft_token: Zeroizing<String>,
}

impl AuthSession {
    pub fn access_token(&self) -> &str {
        self.minecraft_token.as_str()
    }

    /// Debug-only stand-in session, used by the visual-check harness to drive
    /// the real sign-in completion path without a Microsoft account. It never
    /// reaches a network call and does not exist in a release build.
    #[cfg(debug_assertions)]
    pub fn fixture(profile: MinecraftProfile) -> Self {
        Self {
            profile,
            token_saved: false,
            minecraft_token: Zeroizing::new(String::new()),
        }
    }
}

#[derive(Deserialize)]
struct XboxResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: XboxDisplayClaims,
}

#[derive(Deserialize)]
struct XboxDisplayClaims {
    xui: Vec<XboxUserClaim>,
}

#[derive(Deserialize)]
struct XboxUserClaim {
    uhs: String,
}

#[derive(Deserialize)]
struct MinecraftLoginResponse {
    access_token: String,
}

pub struct MicrosoftAuth {
    client: reqwest::Client,
    client_id: String,
    flow: Flow,
}

impl MicrosoftAuth {
    /// Sign in with the bundled public client unless `KAELIS_MS_CLIENT_ID`
    /// names a registered application. The bundled client needs no setup: a
    /// fresh install shows a code immediately.
    pub fn new() -> Result<Self, AuthError> {
        match std::env::var("KAELIS_MS_CLIENT_ID")
            .ok()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
        {
            Some(client_id) => Self::build(client_id, Flow::Entra),
            None => Self::public_client(),
        }
    }

    /// The bundled Live client: code entry on a Microsoft page, then the same
    /// Xbox → Minecraft token chain.
    pub fn public_client() -> Result<Self, AuthError> {
        Self::build(PUBLIC_CLIENT_ID.to_owned(), Flow::Live)
    }

    /// Build around a client ID the player registered themselves.
    pub fn with_client_id(client_id: impl Into<String>) -> Result<Self, AuthError> {
        let client_id = client_id.into();
        let client_id = client_id.trim();
        let flow = if client_id == PUBLIC_CLIENT_ID {
            Flow::Live
        } else {
            Flow::Entra
        };
        Self::build(client_id.to_owned(), flow)
    }

    fn build(client_id: String, flow: Flow) -> Result<Self, AuthError> {
        let client = reqwest::Client::builder()
            .user_agent(kaelis_core::brand::user_agent())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            client,
            client_id,
            flow,
        })
    }

    /// Whether this attempt uses the bundled client, which needs no setup.
    pub fn uses_public_client(&self) -> bool {
        self.flow == Flow::Live
    }

    /// Request a new device code. The GUI displays the user code and opens the
    /// verification URI; the device code itself is never presented to users.
    pub async fn request_device_code(&self) -> Result<DeviceCodeResponse, AuthError> {
        let mut form: Vec<(&str, &str)> = vec![
            ("client_id", self.client_id.as_str()),
            ("scope", self.flow.scope()),
        ];
        if self.flow == Flow::Live {
            // The Live endpoint selects the device flow with this field; the
            // v2.0 endpoint infers it from the URL.
            form.push(("response_type", "device_code"));
        }
        let response = self
            .client
            .post(self.flow.device_code_url())
            .form(&form)
            .send()
            .await?;
        if !response.status().is_success() {
            return Err(rejected_token_error(response).await);
        }
        let mut device = response.json::<DeviceCodeResponse>().await?;
        if device.user_code.trim().is_empty() || device.device_code.is_empty() {
            return Err(AuthError::Rejected(
                "Microsoft returned an incomplete device code".to_owned(),
            ));
        }
        // A missing page would leave the player with a code and nowhere to
        // type it; both endpoints normally send one, so this is a guard.
        if device.verification_uri.trim().is_empty() {
            device.verification_uri = "https://www.microsoft.com/link".to_owned();
        }
        Ok(device)
    }

    /// Poll the device authorization and exchange the resulting Microsoft
    /// token for a Minecraft session. The refresh token is saved to the OS
    /// credential store only after the Minecraft profile has been verified.
    pub async fn login_with_device_code(
        &self,
        device: &DeviceCodeResponse,
    ) -> Result<AuthSession, AuthError> {
        self.login_with_device_code_inner(device, None).await
    }

    pub async fn login_with_device_code_cancellable(
        &self,
        device: &DeviceCodeResponse,
        cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<AuthSession, AuthError> {
        self.login_with_device_code_inner(device, Some(cancel))
            .await
    }

    async fn login_with_device_code_inner(
        &self,
        device: &DeviceCodeResponse,
        cancel: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Result<AuthSession, AuthError> {
        let (microsoft_access, refresh) = self.poll_device_token(device, cancel).await?;
        let mut session = self.exchange_for_minecraft(&microsoft_access).await?;
        session.token_saved = refresh
            .as_deref()
            .is_some_and(|refresh| store_refresh_token(&session.profile.name, refresh).is_ok());
        Ok(session)
    }

    /// Re-authenticate from a keychain refresh token without displaying a
    /// device code. Microsoft may rotate the refresh token on each exchange.
    pub async fn login_with_refresh_token(&self, user: &str) -> Result<AuthSession, AuthError> {
        let refresh = Zeroizing::new(load_refresh_token(user)?);
        let response = self
            .client
            .post(self.flow.token_url())
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh.as_str()),
                ("scope", self.flow.scope()),
            ])
            .send()
            .await?;
        let tokens = parse_token_response(response).await?;
        let (microsoft_access, rotated_refresh) = tokens.into_secrets();
        let mut session = self.exchange_for_minecraft(&microsoft_access).await?;
        session.token_saved = rotated_refresh
            .as_deref()
            .is_some_and(|rotated| store_refresh_token(&session.profile.name, rotated).is_ok());
        Ok(session)
    }

    async fn poll_device_token(
        &self,
        device: &DeviceCodeResponse,
        mut cancel: Option<tokio::sync::watch::Receiver<bool>>,
    ) -> Result<(Zeroizing<String>, Option<Zeroizing<String>>), AuthError> {
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(device.expires_in.max(1));
        let mut interval = device.interval.clamp(1, 60);
        let mut failures = 0_u32;
        loop {
            if cancel.as_ref().is_some_and(|receiver| *receiver.borrow()) {
                return Err(AuthError::Cancelled);
            }
            if std::time::Instant::now() >= deadline {
                return Err(AuthError::Expired);
            }
            let request = self
                .client
                .post(self.flow.token_url())
                .form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("client_id", self.client_id.as_str()),
                    ("device_code", device.device_code.as_str()),
                    ("scope", self.flow.scope()),
                ])
                .send();
            let response = if let Some(receiver) = cancel.as_mut() {
                tokio::select! {
                    result = request => result,
                    changed = receiver.changed() => {
                        if changed.is_err() || *receiver.borrow() {
                            return Err(AuthError::Cancelled);
                        }
                        continue;
                    }
                }
            } else {
                request.await
            };
            let response = match response {
                Ok(response) => {
                    failures = 0;
                    response
                }
                // A dropped connection while the player is typing the code is
                // usually the network, not the attempt: retry on the same
                // interval instead of throwing the code away.
                Err(error) if failures + 1 < MAX_POLL_FAILURES => {
                    failures += 1;
                    if error.is_timeout() || error.is_connect() {
                        interval = interval.saturating_add(1).min(60);
                    }
                    let delay = tokio::time::sleep(std::time::Duration::from_secs(interval));
                    if let Some(receiver) = cancel.as_mut() {
                        tokio::select! {
                            _ = delay => {}
                            changed = receiver.changed() => {
                                if changed.is_err() || *receiver.borrow() {
                                    return Err(AuthError::Cancelled);
                                }
                            }
                        }
                    } else {
                        delay.await;
                    }
                    continue;
                }
                Err(error) => return Err(AuthError::Http(error)),
            };

            if response.status().is_success() {
                let tokens = response.json::<TokenResponse>().await?;
                return Ok(tokens.into_secrets());
            }

            let status = response.status();
            let body = response.text().await?;
            if let Ok(error) = serde_json::from_str::<TokenErrorResponse>(&body) {
                match error.error.as_str() {
                    "authorization_pending" => {}
                    "slow_down" => interval = interval.saturating_add(5).min(60),
                    "expired_token" => return Err(AuthError::Expired),
                    "authorization_declined" => {
                        return Err(AuthError::Rejected("authorization declined".to_owned()));
                    }
                    "bad_verification_code" => {
                        return Err(AuthError::Rejected("invalid device code".to_owned()));
                    }
                    other => return Err(AuthError::Rejected(other.to_owned())),
                }
            } else if !status.is_success() {
                return Err(AuthError::Rejected(format!(
                    "token endpoint returned {status}"
                )));
            }

            let delay = tokio::time::sleep(std::time::Duration::from_secs(interval));
            if let Some(receiver) = cancel.as_mut() {
                tokio::select! {
                    _ = delay => {}
                    changed = receiver.changed() => {
                        if changed.is_err() || *receiver.borrow() {
                            return Err(AuthError::Cancelled);
                        }
                    }
                }
            } else {
                delay.await;
            }
        }
    }

    async fn exchange_for_minecraft(
        &self,
        microsoft_access: &str,
    ) -> Result<AuthSession, AuthError> {
        let xbox = self.xbox_live(microsoft_access).await?;
        let xsts = self.xsts(&xbox.token).await?;
        let minecraft_token = self.minecraft_login(&xsts.token, &xsts.user_hash).await?;
        let profile = self.minecraft_profile(&minecraft_token).await?;
        Ok(AuthSession {
            profile,
            token_saved: true,
            minecraft_token: Zeroizing::new(minecraft_token),
        })
    }

    async fn xbox_live(&self, microsoft_access: &str) -> Result<XboxToken, AuthError> {
        // The accepted ticket form follows the issuing client, so both are
        // offered in the order most likely to succeed. Only an outright 401 is
        // retried: any other refusal would fail the same way twice.
        let mut refused = None;
        for ticket in self.flow.rps_tickets(microsoft_access) {
            match self.authenticate_rps(&ticket).await? {
                Ok(token) => return Ok(XboxToken { token }),
                Err(failure) if failure.status == reqwest::StatusCode::UNAUTHORIZED => {
                    refused = Some(failure.message);
                }
                Err(failure) => return Err(AuthError::Rejected(failure.message)),
            }
        }
        Err(AuthError::Rejected(refused.unwrap_or_else(|| {
            "Xbox Live rejected the Microsoft token".to_owned()
        })))
    }

    /// One Xbox user-authentication attempt with a single ticket encoding.
    ///
    /// The outer error is a transport failure; the inner one is Xbox's own
    /// refusal, which the caller may retry with the other encoding. Neither
    /// the ticket nor the token is ever placed in a message.
    async fn authenticate_rps(
        &self,
        rps_ticket: &str,
    ) -> Result<Result<Zeroizing<String>, XboxRefusal>, AuthError> {
        let body = serde_json::json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": rps_ticket
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT"
        });
        let response = self
            .client
            .post(XBOX_AUTH_URL)
            .header("x-xbl-contract-version", "1")
            .header(reqwest::header::ACCEPT, "application/json")
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let details = response.json::<Value>().await.unwrap_or_default();
            return Ok(Err(XboxRefusal {
                status,
                message: describe_xbox_refusal(status, &details),
            }));
        }
        let response = response.json::<XboxResponse>().await?;
        if response.token.is_empty() {
            return Err(AuthError::Rejected(
                "Xbox returned an empty token".to_owned(),
            ));
        }
        Ok(Ok(Zeroizing::new(response.token)))
    }

    async fn xsts(&self, xbox_token: &str) -> Result<XstsToken, AuthError> {
        let body = serde_json::json!({
            "Properties": { "SandboxId": "RETAIL", "UserTokens": [xbox_token] },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT"
        });
        let response = self
            .client
            .post(XSTS_URL)
            .header("x-xbl-contract-version", "1")
            .json(&body)
            .send()
            .await?;
        if !response.status().is_success() {
            let body = response.json::<Value>().await.unwrap_or_default();
            let code = body.get("XErr").and_then(Value::as_u64).unwrap_or(0);
            let message = match code {
                2_148_916_233 => {
                    "This Microsoft account has no Xbox profile. Sign in at xbox.com once, then retry."
                }
                2_148_916_235 => "Xbox Live is not available in this account's country or region.",
                2_148_916_238 => {
                    "This is a child account. Add it to a Microsoft family group before signing in."
                }
                2_148_916_240 => "Xbox Live is unavailable for this account.",
                _ if code != 0 => {
                    "Xbox authorization failed. Check the account's Xbox profile and region."
                }
                _ => "Xbox authorization was rejected.",
            };
            return Err(AuthError::Rejected(format!("{message} (XErr {code})")));
        }
        let response = response.json::<XboxResponse>().await?;
        let user_hash = response
            .display_claims
            .xui
            .first()
            .map(|claim| claim.uhs.clone())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AuthError::Rejected("XSTS response has no user hash".to_owned()))?;
        if response.token.is_empty() {
            return Err(AuthError::Rejected(
                "XSTS returned an empty token".to_owned(),
            ));
        }
        Ok(XstsToken {
            token: Zeroizing::new(response.token),
            user_hash,
        })
    }

    async fn minecraft_login(
        &self,
        xsts_token: &str,
        user_hash: &str,
    ) -> Result<String, AuthError> {
        let body = serde_json::json!({
            "identityToken": format!("XBL3.0 x={user_hash};{xsts_token}")
        });
        let response = self.client.post(MC_LOGIN_URL).json(&body).send().await?;
        let response = match response.error_for_status() {
            Ok(response) => response,
            Err(error) => {
                return Err(AuthError::Rejected(format!(
                    "Minecraft Services refused the Xbox token: {error}"
                )));
            }
        };
        let response = response.json::<MinecraftLoginResponse>().await?;
        if response.access_token.is_empty() {
            return Err(AuthError::Rejected(
                "Minecraft Services returned an empty access token".to_owned(),
            ));
        }
        Ok(response.access_token)
    }

    async fn minecraft_profile(&self, access_token: &str) -> Result<MinecraftProfile, AuthError> {
        let response = self
            .client
            .get(MC_PROFILE_URL)
            .bearer_auth(access_token)
            .send()
            .await?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(AuthError::Rejected(
                "This Microsoft account does not own Minecraft: Java Edition.".to_owned(),
            ));
        }
        let status = response.status();
        let profile = match response.error_for_status() {
            Ok(response) => response.json::<MinecraftProfile>().await?,
            Err(error) => {
                return Err(AuthError::Rejected(format!(
                    "The Minecraft profile could not be read ({status}): {error}"
                )));
            }
        };
        if profile.id.is_empty() || profile.name.is_empty() {
            return Err(AuthError::Rejected(
                "Minecraft profile response was incomplete".to_owned(),
            ));
        }
        Ok(profile)
    }
}

struct XboxToken {
    token: Zeroizing<String>,
}

/// Xbox's own refusal of an RPS ticket, kept apart from a transport failure so
/// the exchange can try the ticket's other encoding before giving up.
struct XboxRefusal {
    status: reqwest::StatusCode,
    message: String,
}

/// Turn an Xbox error body into a message a player can act on. The ticket and
/// the access token are never part of it.
fn describe_xbox_refusal(status: reqwest::StatusCode, details: &Value) -> String {
    let code = details.get("XErr").and_then(Value::as_u64);
    let detail = details
        .get("Message")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty());
    let diagnosis = match code {
        Some(2_148_916_233) => {
            "This Microsoft account has no Xbox profile yet; open xbox.com once, then retry."
        }
        Some(2_148_916_235) => "Xbox Live is not available in this account's country or region.",
        Some(2_148_916_238) => {
            "This is a child account; a Microsoft family adult has to approve Xbox Live."
        }
        Some(2_148_916_240) => "Xbox Live is unavailable for this account.",
        Some(_) => "Xbox refused the Microsoft token for this account.",
        None if status == reqwest::StatusCode::UNAUTHORIZED => {
            "Xbox did not accept the Microsoft sign-in token. Sign out at \
             account.microsoft.com, sign in again, and retry with a code from a \
             fresh attempt."
        }
        None => "Xbox refused the sign-in request.",
    };
    let mut message = format!("Xbox Live sign-in failed (HTTP {status})");
    if let Some(code) = code {
        message.push_str(&format!(", XErr {code}"));
    }
    message.push_str(&format!(": {diagnosis}"));
    if let Some(detail) = detail {
        message.push_str(&format!(" ({detail})"));
    }
    message
}

struct XstsToken {
    token: Zeroizing<String>,
    user_hash: String,
}

async fn parse_token_response(response: reqwest::Response) -> Result<TokenResponse, AuthError> {
    if response.status().is_success() {
        return Ok(response.json::<TokenResponse>().await?);
    }
    Err(rejected_token_error(response).await)
}

async fn rejected_token_error(response: reqwest::Response) -> AuthError {
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if let Ok(error) = serde_json::from_str::<TokenErrorResponse>(&body) {
        let detail = error
            .error_description
            .filter(|description| !description.trim().is_empty())
            .unwrap_or(error.error);
        return AuthError::Rejected(format!("{status}: {detail}"));
    }
    AuthError::Rejected(format!("Microsoft endpoint returned {status}"))
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
fn store_refresh_token(user: &str, refresh: &str) -> Result<(), AuthError> {
    let entry = keyring::Entry::new(kaelis_core::brand::KEYRING_SERVICE, user)
        .map_err(|error| AuthError::Keyring(error.to_string()))?;
    entry
        .set_password(refresh)
        .map_err(|error| AuthError::Keyring(error.to_string()))
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn store_refresh_token(_user: &str, _refresh: &str) -> Result<(), AuthError> {
    Err(AuthError::Keyring(
        "no native credential-store backend is configured for this platform".to_owned(),
    ))
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
pub fn load_refresh_token(user: &str) -> Result<String, AuthError> {
    let entry = keyring::Entry::new(kaelis_core::brand::KEYRING_SERVICE, user)
        .map_err(|error| AuthError::Keyring(error.to_string()))?;
    entry
        .get_password()
        .map_err(|error| AuthError::Keyring(error.to_string()))
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn load_refresh_token(_user: &str) -> Result<String, AuthError> {
    Err(AuthError::Keyring(
        "no native credential-store backend is configured for this platform".to_owned(),
    ))
}

#[cfg(any(windows, target_os = "macos", target_os = "linux"))]
pub fn delete_refresh_token(user: &str) -> Result<(), AuthError> {
    let entry = keyring::Entry::new(kaelis_core::brand::KEYRING_SERVICE, user)
        .map_err(|error| AuthError::Keyring(error.to_string()))?;
    entry
        .delete_credential()
        .map_err(|error| AuthError::Keyring(error.to_string()))
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
pub fn delete_refresh_token(_user: &str) -> Result<(), AuthError> {
    Err(AuthError::Keyring(
        "no native credential-store backend is configured for this platform".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        ENTRA_DEVICE_CODE_URL, ENTRA_SCOPE, ENTRA_TOKEN_URL, Flow, LIVE_DEVICE_CODE_URL,
        LIVE_SCOPE, LIVE_TOKEN_URL, MicrosoftAuth, PUBLIC_CLIENT_ID,
    };

    #[test]
    fn the_bundled_client_needs_no_registration() {
        // Building the client is the whole setup: no ID, no tenant, no secret.
        MicrosoftAuth::new().expect("public client builds");
        assert_eq!(PUBLIC_CLIENT_ID, "00000000402b5328");
        assert!(LIVE_DEVICE_CODE_URL.starts_with("https://login.live.com/"));
        assert!(LIVE_TOKEN_URL.starts_with("https://login.live.com/"));
        assert!(LIVE_SCOPE.contains("MBI_SSL"));
    }

    #[test]
    fn each_flow_sends_the_ticket_form_its_client_issues() {
        // MBI_SSL tickets are accepted verbatim; v2.0 tokens need `d=` first.
        assert_eq!(
            Flow::Live.rps_tickets("token"),
            ["token".to_owned(), "d=token".to_owned()]
        );
        assert_eq!(
            Flow::Entra.rps_tickets("token"),
            ["d=token".to_owned(), "token".to_owned()]
        );
    }

    #[test]
    fn the_bundled_client_uses_the_live_front_door() {
        let auth = MicrosoftAuth::public_client().expect("public client builds");
        assert!(auth.uses_public_client());
        assert_eq!(Flow::Live.device_code_url(), LIVE_DEVICE_CODE_URL);
        assert_eq!(Flow::Live.token_url(), LIVE_TOKEN_URL);
        assert_eq!(Flow::Live.scope(), LIVE_SCOPE);
        assert_eq!(Flow::Entra.device_code_url(), ENTRA_DEVICE_CODE_URL);
        assert_eq!(Flow::Entra.token_url(), ENTRA_TOKEN_URL);
        assert_eq!(Flow::Entra.scope(), ENTRA_SCOPE);
    }

    #[test]
    fn a_registered_client_switches_to_the_v2_flow() {
        let auth = MicrosoftAuth::with_client_id("11111111-2222-3333-4444-555555555555")
            .expect("registered client builds");
        assert!(!auth.uses_public_client());
        assert_eq!(auth.flow, Flow::Entra);
    }

    #[test]
    fn a_refused_ticket_names_the_account_problem() {
        let details = serde_json::json!({ "XErr": 2_148_916_233u64, "Message": "no profile" });
        let message = super::describe_xbox_refusal(reqwest::StatusCode::UNAUTHORIZED, &details);
        assert!(message.contains("XErr 2148916233"), "{message}");
        assert!(message.contains("xbox.com"), "{message}");
        assert!(!message.contains("d="), "{message}");

        let bare = super::describe_xbox_refusal(
            reqwest::StatusCode::UNAUTHORIZED,
            &serde_json::Value::Null,
        );
        assert!(bare.contains("did not accept the Microsoft sign-in token"));
    }

    #[test]
    fn device_code_json_parses_without_optional_fields() {
        let device: super::DeviceCodeResponse = serde_json::from_str(
            r#"{"user_code":"ABCD1234","device_code":"secret","verification_uri":"https://www.microsoft.com/link","expires_in":900}"#,
        )
        .expect("live endpoint shape parses");
        assert_eq!(device.interval, 5);
        assert!(device.verification_uri_complete.is_none());
    }

    #[test]
    #[ignore = "touches the OS credential store"]
    fn refresh_tokens_round_trip_through_the_credential_store() {
        let user = "kaelis-keyring-probe";
        super::store_refresh_token(user, "probe").expect("store token");
        assert_eq!(
            super::load_refresh_token(user).expect("load token"),
            "probe"
        );
        super::delete_refresh_token(user).expect("delete token");
    }

    #[tokio::test]
    #[ignore = "network: asks Microsoft for a real device code"]
    async fn the_bundled_client_gets_a_real_code() {
        let auth = MicrosoftAuth::new().expect("public client builds");
        let device = auth.request_device_code().await.expect("device code");
        // A real code is short, uppercase, and comes with a page to type it on.
        assert!(
            device.user_code.len() >= 6 && device.user_code.len() <= 12,
            "{}",
            device.user_code
        );
        assert!(device.verification_uri.starts_with("https://"));
        assert!(device.expires_in >= 60);
        assert!(!device.device_code.is_empty());
    }
}
