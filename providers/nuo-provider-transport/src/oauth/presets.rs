//! Official OAuth 2.0 client presets and credentials for subscription providers.
//!
//! Pinned and maintained in `nuo-provider-adapters` (not `nuo-wire`), keeping core
//! domain contracts 100% free of vendor-specific secrets, client IDs, and endpoints.

use nuo_model_codec::LoginMethod;
use nuo_model_codec::provider_auth::{
    ClientAuthMethod, DeviceFlowMode, OAuthConfig, PkceMode, PortMode, TokenRequestFormat,
};
use std::borrow::Cow;

pub const GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_ID: &str = concat!(
    "1071006060591-",
    "tmhssin2h21lcre235vtolojh4g403ep",
    ".apps.googleusercontent.com"
);

pub const GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_SECRET: &str =
    concat!("GOCSPX-", "K58FWR486LdLJ1mLB8sXC4z6qDAf");

/// Google Antigravity standalone CLI (`agy`) OAuth client ID.
///
/// Extracted from the shipped `agy` binary and confirmed live against
/// `oauth2.googleapis.com/token`: this id pairs with
/// [`GOOGLE_ANTIGRAVITY_CLI_CLIENT_SECRET`] (the endpoint answers
/// `invalid_grant` for a dummy refresh token rather than `invalid_client`).
pub const GOOGLE_ANTIGRAVITY_CLI_CLIENT_ID: &str = concat!(
    "884354919052-",
    "36trc1jjb3tguiac32ov6cod268c5blh",
    ".apps.googleusercontent.com"
);

pub const GOOGLE_ANTIGRAVITY_CLI_CLIENT_SECRET: &str =
    concat!("GOCSPX-", "9YQWpF7RWDC0QTdj-YxKMwR0ZtsX");

pub const XAI_CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const CHATGPT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const COPILOT_CLIENT_ID: &str = "Ov23li8tweQw6odWQebz";
pub const QODER_CLIENT_ID: &str = "e883ade2-e6e3-4d6d-adf7-f92ceff5fdcb";
pub const QODER_CN_CLIENT_ID: &str = "e93fe488-5778-4c35-a6fc-0f54ed7b3139";
pub const OPENCODE_CLIENT_ID: &str = "opencode-cli";
pub const OPENCODE_CONSOLE_URL: &str = "https://opencode.ai/console";

/// Google Antigravity (Cloud Code Companion) preset.
pub fn google_antigravity_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("google-antigravity"),
        client_id: Cow::Borrowed(GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_ID),
        client_secret: Some(Cow::Borrowed(GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_SECRET)),
        client_auth_method: ClientAuthMethod::RequestBody,
        authorize_url: Cow::Borrowed("https://accounts.google.com/o/oauth2/v2/auth"),
        token_url: Cow::Borrowed("https://oauth2.googleapis.com/token"),
        device_authorization_url: Cow::Borrowed("https://oauth2.googleapis.com/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed(
            "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs https://www.googleapis.com/auth/aicode openid",
        ),
        extra_authorize_params: vec![
            (Cow::Borrowed("access_type"), Cow::Borrowed("offline")),
            (Cow::Borrowed("prompt"), Cow::Borrowed("consent")),
            (
                Cow::Borrowed("include_granted_scopes"),
                Cow::Borrowed("true"),
            ),
        ],
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: Some(Cow::Borrowed(
            nuo_model_codec::client_identity::ANTIGRAVITY_USER_AGENT,
        )),
        browser_login: true,
        default_login_method: LoginMethod::Browser,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 51121,
        port_mode: PortMode::PreferredOrDynamic(51121),
        oauth_path: Cow::Borrowed("/oauth-callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlowMode::disabled(),
        device_token_url: Cow::Borrowed("https://oauth2.googleapis.com/token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// Google Antigravity Standalone CLI preset.
pub fn google_antigravity_cli_preset() -> OAuthConfig {
    let mut cfg = google_antigravity_preset();
    cfg.provider_id = Cow::Borrowed("antigravity-cli");
    cfg.client_id = Cow::Borrowed(GOOGLE_ANTIGRAVITY_CLI_CLIENT_ID);
    cfg.client_secret = Some(Cow::Borrowed(GOOGLE_ANTIGRAVITY_CLI_CLIENT_SECRET));
    cfg
}

/// xAI SuperGrok preset.
pub fn xai_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("xai"),
        client_id: Cow::Borrowed(XAI_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://auth.x.ai/oauth2/authorize"),
        token_url: Cow::Borrowed("https://auth.x.ai/oauth2/token"),
        device_authorization_url: Cow::Borrowed("https://auth.x.ai/oauth2/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed("openid profile email offline_access grok-cli:access api:access"),
        extra_authorize_params: vec![
            (Cow::Borrowed("plan"), Cow::Borrowed("generic")),
            (Cow::Borrowed("referrer"), Cow::Borrowed("nuo")),
        ],
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        browser_login: true,
        default_login_method: LoginMethod::Device,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 56121,
        port_mode: PortMode::Fixed(56121),
        oauth_path: Cow::Borrowed("/callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: true,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlowMode::rfc8628(),
        device_token_url: Cow::Borrowed("https://auth.x.ai/oauth2/token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// OpenAI / ChatGPT Subscription preset.
pub fn chatgpt_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("chatgpt"),
        client_id: Cow::Borrowed(CHATGPT_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://auth.openai.com/oauth/authorize"),
        token_url: Cow::Borrowed("https://auth.openai.com/oauth/token"),
        device_authorization_url: Cow::Borrowed(
            "https://auth.openai.com/api/accounts/deviceauth/usercode",
        ),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed(
            "openid profile email offline_access api.connectors.read api.connectors.invoke",
        ),
        extra_authorize_params: vec![
            (
                Cow::Borrowed("id_token_add_organizations"),
                Cow::Borrowed("true"),
            ),
            (
                Cow::Borrowed("codex_cli_simplified_flow"),
                Cow::Borrowed("true"),
            ),
            (Cow::Borrowed("originator"), Cow::Borrowed("codex_cli_rs")),
        ],
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        browser_login: true,
        default_login_method: LoginMethod::Browser,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 1455,
        port_mode: PortMode::Fixed(1455),
        oauth_path: Cow::Borrowed("/auth/callback"),
        redirect_host: Cow::Borrowed("localhost"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlowMode::custom("chatgpt"),
        device_token_url: Cow::Borrowed("https://auth.openai.com/api/accounts/deviceauth/token"),
        device_redirect_uri: Cow::Borrowed("https://auth.openai.com/deviceauth/callback"),
    }
}

/// GitHub Copilot subscription preset.
pub fn copilot_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("copilot"),
        client_id: Cow::Borrowed(COPILOT_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://github.com/login/oauth/authorize"),
        token_url: Cow::Borrowed("https://github.com/login/oauth/access_token"),
        device_authorization_url: Cow::Borrowed("https://github.com/login/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed("read:user"),
        extra_authorize_params: Vec::new(),
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        browser_login: false,
        default_login_method: LoginMethod::Device,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 42195,
        port_mode: PortMode::Fixed(42195),
        oauth_path: Cow::Borrowed("/callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::FormUrlEncoded,
        device_flow: DeviceFlowMode::rfc8628(),
        device_token_url: Cow::Borrowed("https://github.com/login/oauth/access_token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// Alibaba Qoder international preset.
pub fn qoder_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("qoder"),
        client_id: Cow::Borrowed(QODER_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed("https://qoder.com/device/selectAccounts"),
        token_url: Cow::Borrowed("https://openapi.qoder.sh/api/v1/deviceToken/poll"),
        device_authorization_url: Cow::Borrowed("https://qoder.com/device/selectAccounts"),
        grant_type_device: Cow::Borrowed("qoder_device_flow"),
        scope: Cow::Borrowed("openid"),
        extra_authorize_params: Vec::new(),
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        browser_login: false,
        default_login_method: LoginMethod::Device,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 0,
        port_mode: PortMode::Dynamic,
        oauth_path: Cow::Borrowed("/callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: true,
        pkce_mode: PkceMode::S256,
        token_format: TokenRequestFormat::Json,
        device_flow: DeviceFlowMode::custom("qoder"),
        device_token_url: Cow::Borrowed("https://openapi.qoder.sh/api/v1/deviceToken/poll"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// Alibaba Qoder CN region preset.
pub fn qoder_cn_preset() -> OAuthConfig {
    let mut cfg = qoder_preset();
    cfg.provider_id = Cow::Borrowed("qoder-cn");
    cfg.client_id = Cow::Borrowed(QODER_CN_CLIENT_ID);
    cfg.authorize_url = Cow::Borrowed("https://qoder.com.cn/device/selectAccounts");
    cfg.token_url = Cow::Borrowed("https://openapi.qoder.com.cn/api/v1/deviceToken/poll");
    cfg.device_authorization_url = Cow::Borrowed("https://qoder.com.cn/device/selectAccounts");
    cfg.device_token_url =
        Cow::Borrowed("https://openapi.qoder.com.cn/api/v1/deviceToken/poll");
    cfg
}

/// OpenCode Console account preset (the OpenCode Go / Zen subscription).
///
/// OpenCode uses a JSON device-authorization grant: the `/auth/device/code`
/// endpoint takes `{client_id}` and the `/auth/device/token` endpoint both
/// polls for the initial tokens and rotates them on refresh. There is no
/// loopback browser callback and no scope.
pub fn opencode_preset() -> OAuthConfig {
    OAuthConfig {
        provider_id: Cow::Borrowed("opencode"),
        client_id: Cow::Borrowed(OPENCODE_CLIENT_ID),
        client_secret: None,
        client_auth_method: ClientAuthMethod::None,
        authorize_url: Cow::Borrowed(OPENCODE_CONSOLE_URL),
        token_url: Cow::Borrowed("https://opencode.ai/console/auth/device/token"),
        device_authorization_url: Cow::Borrowed("https://opencode.ai/console/auth/device/code"),
        grant_type_device: Cow::Borrowed("urn:ietf:params:oauth:grant-type:device_code"),
        scope: Cow::Borrowed(""),
        extra_authorize_params: Vec::new(),
        extra_token_params: Vec::new(),
        extra_refresh_params: Vec::new(),
        extra_headers: Vec::new(),
        user_agent: None,
        browser_login: false,
        default_login_method: LoginMethod::Device,
        oauth_host: Cow::Borrowed("127.0.0.1"),
        oauth_port: 0,
        port_mode: PortMode::Dynamic,
        oauth_path: Cow::Borrowed("/callback"),
        redirect_host: Cow::Borrowed("127.0.0.1"),
        custom_redirect_uri: None,
        send_nonce: false,
        pkce_mode: PkceMode::Disabled,
        token_format: TokenRequestFormat::Json,
        device_flow: DeviceFlowMode::custom("opencode"),
        device_token_url: Cow::Borrowed("https://opencode.ai/console/auth/device/token"),
        device_redirect_uri: Cow::Borrowed(""),
    }
}

/// Lookup OAuth preset configuration by stable provider id.
pub fn config_by_provider_id(provider_id: &str) -> Option<OAuthConfig> {
    match provider_id {
        "google-antigravity" | "antigravity" => Some(google_antigravity_preset()),
        "antigravity-cli" => Some(google_antigravity_cli_preset()),
        "xai" => Some(xai_preset()),
        "chatgpt" | "openai-subscription" | "chatgpt-plan" => Some(chatgpt_preset()),
        "copilot" | "github-copilot" => Some(copilot_preset()),
        "qoder" => Some(qoder_preset()),
        "qoder-cn" => Some(qoder_cn_preset()),
        "opencode" | "opencode-go" | "opencode-plan" => Some(opencode_preset()),
        _ => None,
    }
}

pub fn is_qoder(config: &OAuthConfig) -> bool {
    config.provider_id == "qoder"
        || config.provider_id == "qoder-cn"
        || config.token_url.contains("openapi.qoder.sh")
        || config.token_url.contains("openapi.qoder.com.cn")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bundled Antigravity presets must ship *matched* client-id/secret
    /// pairs. Google rejects a crossed pair with `invalid_client` at the token
    /// endpoint, and nuo's own google-antigravity channel would then fail every
    /// token exchange. Verified live against `oauth2.googleapis.com/token`
    /// (ADR-0289).
    #[test]
    fn antigravity_presets_ship_matched_credential_pairs() {
        let main = google_antigravity_preset();
        assert_eq!(main.client_id, GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_ID);
        assert_eq!(
            main.client_secret.as_deref(),
            Some(GOOGLE_ANTIGRAVITY_CLOUD_CODE_CLIENT_SECRET)
        );
        assert!(main.client_id.starts_with("1071006060591-"));
        assert_eq!(
            main.client_secret.as_deref(),
            Some("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")
        );

        let cli = google_antigravity_cli_preset();
        assert_eq!(cli.client_id, GOOGLE_ANTIGRAVITY_CLI_CLIENT_ID);
        assert_eq!(
            cli.client_secret.as_deref(),
            Some(GOOGLE_ANTIGRAVITY_CLI_CLIENT_SECRET)
        );
        assert!(cli.client_id.starts_with("884354919052-"));
        assert_eq!(
            cli.client_secret.as_deref(),
            Some("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")
        );

        // The two Antigravity surfaces must never share a secret: each Google
        // client id is paired one-to-one with its own secret.
        assert_ne!(main.client_secret, cli.client_secret);
        assert_ne!(main.client_id, cli.client_id);
    }

    /// The preset carries the CLI-brand User-Agent mirroring the `agy` wire
    /// identity, not the rejected legacy `antigravity/<version>` form.
    #[test]
    fn antigravity_preset_advertises_cli_brand_user_agent() {
        let cfg = google_antigravity_preset();
        let ua = cfg.user_agent.expect("antigravity preset declares a User-Agent");
        assert!(ua.starts_with("antigravity/cli/"));
        assert_eq!(ua, nuo_model_codec::client_identity::ANTIGRAVITY_USER_AGENT);
    }

    /// The consent scope must include `aicode`: every stored Antigravity token
    /// set in the wild carries it, and it is the scope that authorises the
    /// Cloud Code inference surface.
    #[test]
    fn antigravity_scope_requests_aicode() {
        let cfg = google_antigravity_preset();
        assert!(cfg.scope.contains("https://www.googleapis.com/auth/aicode"));
        assert!(cfg.scope.contains("https://www.googleapis.com/auth/cloud-platform"));
        assert!(cfg.scope.contains("https://www.googleapis.com/auth/cclog"));
        assert!(cfg.scope.contains("https://www.googleapis.com/auth/experimentsandconfigs"));
    }

    #[test]
    fn opencode_preset_is_device_only_json() {
        let cfg = config_by_provider_id("opencode").expect("opencode preset");
        assert_eq!(cfg.client_id, OPENCODE_CLIENT_ID);
        assert!(!cfg.browser_login);
        assert_eq!(
            cfg.effective_default_login_method(),
            Some(LoginMethod::Device)
        );
        assert!(cfg.supports_login_method(LoginMethod::Device));
        assert!(!cfg.supports_login_method(LoginMethod::Browser));
        assert_eq!(cfg.device_flow, DeviceFlowMode::custom("opencode"));
        assert_eq!(cfg.token_format, TokenRequestFormat::Json);
        assert_eq!(cfg.pkce_mode, PkceMode::Disabled);
        assert_eq!(
            cfg.device_authorization_url,
            "https://opencode.ai/console/auth/device/code"
        );
        assert_eq!(
            cfg.token_url,
            "https://opencode.ai/console/auth/device/token"
        );
    }

    #[test]
    fn opencode_go_alias_resolves_the_same_preset() {
        assert_eq!(
            config_by_provider_id("opencode-go").map(|c| c.provider_id),
            Some(Cow::Borrowed("opencode"))
        );
    }
}
