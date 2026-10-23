//! Microsoft device authorization → Xbox Live → XSTS → Minecraft Services.
//! Microsoft refresh credentials stay in the OS keychain; live access tokens
//! are held in zeroizing RAM wrappers and are never serializable.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use zeroize::{Zeroize, Zeroizing};

mod offline;
pub use offline::{
    AvatarError, OfflineProfile, crafatar_head_url, fetch_crafatar_head, fetch_head_for_username,
    lookup_premium_uuid, offline_profile, valid_username,
};

const MS_DEVICE_CODE_URL: &str =
    "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const MS_AUTHORIZE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize";
const MS_TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const MS_SCOPE: &str = "XboxLive.signin offline_access openid profile";

/// The public Microsoft account client every Xbox Live relying party may use,
/// the same one the Xbox app and the console family sign in with. It needs no
/// Azure registration, so a fresh install can show a code immediately instead
/// of first asking the player to create an app registration. Tokens from it are
/// still exchanged through Xbox Live → XSTS → Minecraft Services, so an
/// account that does not own Java Edition is refused exactly as before.
const PUBLIC_CLIENT_ID: &str = "00000000402b5328";
const LIVE_DEVICE_CODE_URL: &str = "https://login.live.com/oauth20_connect.srf";
const LIVE_TOKEN_URL: &str = "https://login.live.com/oauth20_token.srf";
const LIVE_SCOPE: &str = "service::user.auth.xboxlive.com::MBI_SSL";
/// A poll that never answers must not spin forever: three failed polls in a
/// row end the attempt instead of hammering the endpoint.
const MAX_POLL_FAILURES: u32 = 3;
const BROWSER_LOGIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const CALLBACK_PATH: &str = "/";
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
    #[error("browser sign-in timed out; start again")]
    TimedOut,
    #[error("browser sign-in listener failed: {0}")]
    LocalIo(#[from] std::io::Error),
    #[error(
        "Microsoft client ID is not configured. Add an Azure public client ID in Settings > Integrations."
    )]
    MissingClientId,
}

/// Which Microsoft identity front door an attempt uses.
///
/// [`Flow::Live`] is the public Xbox Live client: no registration, a code and a
/// short page, which is why it is the default. [`Flow::Entra`] is the client ID
/// a self-hosting user registers, with the documented v2.0 device-code flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Live,
    Entra,
}

impl Flow {
    fn device_code_url(self) -> &'static str {
        match self {
            Self::Live => LIVE_DEVICE_CODE_URL,
            Self::Entra => MS_DEVICE_CODE_URL,
        }
    }

    fn token_url(self) -> &'static str {
        match self {
            Self::Live => LIVE_TOKEN_URL,
            Self::Entra => MS_TOKEN_URL,
        }
    }

    fn scope(self) -> &'static str {
        match self {
            Self::Live => LIVE_SCOPE,
            Self::Entra => "XboxLive.signin offline_access",
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
    minecraft_token: Zeroizing<String>,
}

impl AuthSession {
    pub fn access_token(&self) -> &str {
        self.minecraft_token.as_str()
    }
}

/// A prepared OAuth 2.0 authorization-code + PKCE flow. Binding the loopback
/// listener happens before the browser opens, so the redirect URI is stable
/// and the local callback can never race the browser.
pub struct BrowserLogin {
    authorize_url: String,
    redirect_uri: String,
    state: String,
    verifier: Zeroizing<String>,
    listener: tokio::net::TcpListener,
}

impl BrowserLogin {
    pub fn authorize_url(&self) -> &str {
        &self.authorize_url
    }

    pub fn redirect_uri(&self) -> &str {
        &self.redirect_uri
    }

    /// Wait for the browser to hit the loopback redirect, exchange the code,
    /// and verify the Minecraft profile. The refresh token is written to the
    /// OS credential store only after the profile is confirmed.
    pub async fn finish(
        self,
        auth: &MicrosoftAuth,
        mut cancel: tokio::sync::watch::Receiver<bool>,
    ) -> Result<AuthSession, AuthError> {
        let deadline = tokio::time::Instant::now() + BROWSER_LOGIN_TIMEOUT;
        let code = loop {
            let accepted = tokio::select! {
                changed = cancel.changed() => {
                    if changed.is_err() || *cancel.borrow() {
                        return Err(AuthError::Cancelled);
                    }
                    continue;
                }
                accepted = tokio::time::timeout_at(deadline, self.listener.accept()) => accepted,
            };
            let (mut socket, _) = match accepted {
                Ok(Ok(connection)) => connection,
                Ok(Err(error)) => return Err(AuthError::LocalIo(error)),
                Err(_) => return Err(AuthError::TimedOut),
            };
            let mut request = Vec::with_capacity(4096);
            let read = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                let mut buffer = [0_u8; 4096];
                loop {
                    let count = socket.read(&mut buffer).await?;
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&buffer[..count]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n")
                        || request.len() >= 16 * 1024
                    {
                        break;
                    }
                }
                Ok::<(), std::io::Error>(())
            })
            .await;
            match read {
                Ok(Ok(())) => {}
                Ok(Err(error)) => return Err(AuthError::LocalIo(error)),
                Err(_) => return Err(AuthError::TimedOut),
            }
            let request = String::from_utf8_lossy(&request);
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or_default();
            let (path, query) = target.split_once('?').unwrap_or((target, ""));
            if path != CALLBACK_PATH {
                let _ = respond_to_browser(
                    &mut socket,
                    404,
                    "Not found",
                    "This local address only accepts the Kaelis sign-in callback.",
                )
                .await;
                continue;
            }
            let params = url::form_urlencoded::parse(query.as_bytes()).collect::<Vec<_>>();
            let value = |name: &str| {
                params
                    .iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.to_string())
            };
            if let Some(error) = value("error") {
                let description = value("error_description").unwrap_or_default();
                let detail = if description.trim().is_empty() {
                    error
                } else {
                    format!("{error}: {description}")
                };
                let _ = respond_to_browser(
                    &mut socket,
                    400,
                    "Sign-in was not completed",
                    "Kaelis could not finish Microsoft sign-in. Return to the app and try again.",
                )
                .await;
                return Err(AuthError::Rejected(detail));
            }
            let Some(returned_state) = value("state") else {
                let _ = respond_to_browser(
                    &mut socket,
                    400,
                    "Missing sign-in state",
                    "Return to Kaelis and start the browser sign-in again.",
                )
                .await;
                return Err(AuthError::Rejected(
                    "browser callback did not include an OAuth state value".to_owned(),
                ));
            };
            if returned_state != self.state {
                let _ = respond_to_browser(
                    &mut socket,
                    400,
                    "Sign-in state did not match",
                    "The callback was stale or from another session. Start again in Kaelis.",
                )
                .await;
                return Err(AuthError::Rejected(
                    "browser callback state did not match the active sign-in".to_owned(),
                ));
            }
            let Some(code) = value("code") else {
                let _ = respond_to_browser(
                    &mut socket,
                    400,
                    "Missing authorization code",
                    "Return to Kaelis and start the browser sign-in again.",
                )
                .await;
                return Err(AuthError::Rejected(
                    "browser callback did not include an authorization code".to_owned(),
                ));
            };
            let _ = respond_to_browser(
                &mut socket,
                200,
                "Kaelis is connected",
                "Microsoft sign-in finished. You can close this tab and return to Kaelis.",
            )
            .await;
            break Zeroizing::new(code);
        };

        let response = auth
            .client
            .post(MS_TOKEN_URL)
            .form(&[
                ("client_id", auth.client_id.as_str()),
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("code_verifier", self.verifier.as_str()),
                ("scope", MS_SCOPE),
            ])
            .send()
            .await?;
        let tokens = parse_token_response(response).await?;
        let (microsoft_access, refresh) = tokens.into_secrets();
        let session = auth.exchange_for_minecraft(&microsoft_access).await?;
        if let Some(refresh) = refresh.as_deref() {
            store_refresh_token(&session.profile.name, refresh)?;
        }
        Ok(session)
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
    /// Build from `KAELIS_MS_CLIENT_ID` when it is set, otherwise from the
    /// bundled public client so sign-in works on a fresh install with no setup
    /// at all.
    pub fn new() -> Result<Self, AuthError> {
        match std::env::var("KAELIS_MS_CLIENT_ID")
            .ok()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty())
        {
            Some(client_id) => Self::with_client_id(client_id),
            None => Self::public_client(),
        }
    }

    /// The bundled public client: no Azure registration, code entry on a
    /// Microsoft page, and the same Xbox → Minecraft token chain afterwards.
    pub fn public_client() -> Result<Self, AuthError> {
        Self::build(PUBLIC_CLIENT_ID.to_owned(), Flow::Live)
    }

    /// Build with a client id supplied by the UI settings instead of relying
    /// on the process environment.
    pub fn with_client_id(client_id: impl Into<String>) -> Result<Self, AuthError> {
        let client_id = client_id.into();
        let client_id = client_id.trim();
        if client_id.is_empty() {
            return Err(AuthError::MissingClientId);
        }
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

    /// Whether this attempt uses the bundled client, which the settings UI
    /// shows as "works out of the box" instead of a required field.
    pub fn uses_public_client(&self) -> bool {
        self.flow == Flow::Live
    }

    /// The authorization-code + PKCE flow is only meaningful for a registered
    /// v2.0 application; the public Xbox client has no such redirect.
    pub fn supports_browser_login(&self) -> bool {
        self.flow == Flow::Entra
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
            // v2.0 endpoint infers it from the URL instead.
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
    /// token for a Minecraft session. The refresh token is saved to Credential
    /// Manager only after the Minecraft profile has been verified.
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
        let session = self.exchange_for_minecraft(&microsoft_access).await?;
        if let Some(refresh) = refresh.as_deref() {
            store_refresh_token(&session.profile.name, refresh)?;
        }
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
        let session = self.exchange_for_minecraft(&microsoft_access).await?;
        if let Some(rotated) = rotated_refresh.as_deref() {
            store_refresh_token(&session.profile.name, rotated)?;
        }
        Ok(session)
    }

    /// Start an authorization-code + PKCE flow with a loopback redirect.
    /// Device-code authorization can be disabled by a tenant policy; this
    /// path is the browser fallback and uses the same Xbox → XSTS → Minecraft
    /// token chain. The listener binds before the URL is returned so the
    /// redirect port is already live when the browser comes back.
    pub async fn begin_browser_login(&self) -> Result<BrowserLogin, AuthError> {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let redirect_uri = format!("http://localhost:{port}");
        let mut verifier_bytes = [0_u8; 64];
        rand::thread_rng().fill_bytes(&mut verifier_bytes);
        let verifier = Zeroizing::new(URL_SAFE_NO_PAD.encode(verifier_bytes));
        let challenge = pkce_challenge(&verifier);
        let mut state_bytes = [0_u8; 24];
        rand::thread_rng().fill_bytes(&mut state_bytes);
        let state = URL_SAFE_NO_PAD.encode(state_bytes);
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("client_id", &self.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("response_mode", "query")
            .append_pair("scope", MS_SCOPE)
            .append_pair("state", &state)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("prompt", "select_account")
            .finish();
        Ok(BrowserLogin {
            authorize_url: format!("{MS_AUTHORIZE_URL}?{query}"),
            redirect_uri,
            state,
            verifier,
            listener,
        })
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
            minecraft_token: Zeroizing::new(minecraft_token),
        })
    }

    async fn xbox_live(&self, microsoft_access: &str) -> Result<XboxToken, AuthError> {
        let body = serde_json::json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": format!("d={microsoft_access}")
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT"
        });
        let response = self
            .client
            .post(XBOX_AUTH_URL)
            .header("x-xbl-contract-version", "1")
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<XboxResponse>()
            .await?;
        if response.token.is_empty() {
            return Err(AuthError::Rejected(
                "Xbox returned an empty token".to_owned(),
            ));
        }
        Ok(XboxToken {
            token: Zeroizing::new(response.token),
        })
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
        let response = self
            .client
            .post(MC_LOGIN_URL)
            .json(&body)
            .send()
            .await?
            .error_for_status()?
            .json::<MinecraftLoginResponse>()
            .await?;
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
        let profile = response
            .error_for_status()?
            .json::<MinecraftProfile>()
            .await?;
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

async fn respond_to_browser(
    socket: &mut tokio::net::TcpStream,
    status: u16,
    title: &str,
    message: &str,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title} · Kaelis</title><style>\
         :root{{color-scheme:dark}}\
         body{{margin:0;min-height:100vh;display:grid;place-items:center;\
         background:radial-gradient(700px 460px at 20% 0%,#12314d,transparent 70%),\
         radial-gradient(600px 420px at 90% 100%,#26205a,transparent 72%),#060a12;\
         color:#edf4fc;font:16px/1.65 'Segoe UI',system-ui,sans-serif}}\
         main{{width:min(520px,calc(100% - 40px));padding:38px;border:1px solid \
         rgba(122,158,198,.22);border-radius:26px;background:rgba(12,20,33,.84);\
         box-shadow:0 30px 80px rgba(0,0,0,.45)}}\
         .mark{{width:54px;height:54px;border-radius:16px;display:grid;\
         place-items:center;background:linear-gradient(145deg,#142a40,#09111d);\
         border:1px solid #2f6388;color:#7fd4ff;font-weight:700;font-size:26px;\
         margin-bottom:22px}}\
         h1{{font-size:26px;letter-spacing:-.03em;margin:0 0 10px}}\
         p{{margin:0;color:#91a7c0}} small{{display:block;margin-top:22px;color:#60748c;font-size:11px}}\
         </style></head><body><main><div class=\"mark\">K</div>\
         <h1>{title}</h1><p>{message}</p>\
         <small>Kaelis · a quiet Minecraft workspace</small></main></body></html>"
    );
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\
         Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'\r\n\r\n",
        body.len()
    );
    socket.write_all(head.as_bytes()).await?;
    socket.write_all(body.as_bytes()).await?;
    socket.shutdown().await
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

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
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
    use super::{Flow, MicrosoftAuth, PUBLIC_CLIENT_ID, pkce_challenge};

    #[test]
    fn the_bundled_client_needs_no_registration() {
        let auth = MicrosoftAuth::public_client().expect("public client builds");
        assert!(auth.uses_public_client());
        // The Live front door has no PKCE redirect, so the GUI must not offer
        // a browser button that would land on unauthorized_client.
        assert!(!auth.supports_browser_login());
        assert_eq!(auth.flow, Flow::Live);
        assert_eq!(auth.client_id, PUBLIC_CLIENT_ID);
        assert_eq!(auth.flow.device_code_url(), super::LIVE_DEVICE_CODE_URL);
    }

    #[test]
    fn a_registered_application_uses_the_documented_flow() {
        let auth = MicrosoftAuth::with_client_id("11111111-2222-3333-4444-555555555555")
            .expect("client id builds");
        assert!(!auth.uses_public_client());
        assert!(auth.supports_browser_login());
        assert_eq!(auth.flow, Flow::Entra);
        assert_eq!(auth.flow.scope(), "XboxLive.signin offline_access");
        assert_eq!(auth.flow.device_code_url(), super::MS_DEVICE_CODE_URL);
    }

    #[tokio::test]
    #[ignore = "network: asks Microsoft for a real device code"]
    async fn the_bundled_client_gets_a_real_code() {
        let auth = MicrosoftAuth::public_client().expect("public client builds");
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

    #[test]
    fn the_public_client_id_selects_the_live_flow() {
        let auth = MicrosoftAuth::with_client_id(PUBLIC_CLIENT_ID).expect("client id builds");
        assert!(auth.uses_public_client());
        assert_eq!(auth.flow.token_url(), super::LIVE_TOKEN_URL);
    }

    #[test]
    fn pkce_challenge_matches_rfc_7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[tokio::test]
    async fn browser_login_binds_loopback_and_builds_authorize_url() {
        let auth = MicrosoftAuth::with_client_id("00000000-0000-0000-0000-000000000000")
            .expect("test client should build");
        let flow = auth
            .begin_browser_login()
            .await
            .expect("listener should bind");
        assert!(flow.redirect_uri().starts_with("http://localhost:"));
        assert!(!flow.redirect_uri().contains("/kaelis-auth"));
        let url = flow.authorize_url();
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("state="));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost"));
        assert!(url.contains("XboxLive.signin"));
    }
}
