use {
    moltis_channels::{
        ChannelConfigView,
        gating::{DmPolicy, GroupPolicy, MentionMode},
    },
    secrecy::{ExposeSecret, Secret},
    serde::{Deserialize, Serialize},
};

/// Feishu bot account config.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FeishuAccountConfig {
    /// App ID from Feishu developer console.
    #[serde(serialize_with = "serialize_secret")]
    pub app_id: Secret<String>,
    /// App Secret from Feishu developer console.
    #[serde(serialize_with = "serialize_secret")]
    pub app_secret: Secret<String>,
    /// Base API URL (default: https://open.feishu.cn).
    pub base_url: String,
    /// Optional direct WebSocket endpoint override.
    /// Leave empty to auto-negotiate with `{base_url}/callback/ws/endpoint`.
    pub ws_endpoint: String,
    /// DM access policy.
    pub dm_policy: DmPolicy,
    /// Group access policy.
    pub group_policy: GroupPolicy,
    /// Mention activation mode for groups.
    pub mention_mode: MentionMode,
    /// User allowlist for DMs.
    pub allowlist: Vec<String>,
    /// Group/chat ID allowlist.
    pub group_allowlist: Vec<String>,
    /// Default model ID for this bot's sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Provider name associated with `model`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    /// Default agent ID for this bot's sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Allow /agent switching for this bot (default: false).
    pub allow_agent_switch: bool,
    /// Auto-archive stale non-active sessions after N days (0 to disable).
    pub session_auto_archive_days: u64,
}

impl std::fmt::Debug for FeishuAccountConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeishuAccountConfig")
            .field("app_id", &"[REDACTED]")
            .field("app_secret", &"[REDACTED]")
            .field("dm_policy", &self.dm_policy)
            .field("group_policy", &self.group_policy)
            .field("session_auto_archive_days", &self.session_auto_archive_days)
            .finish_non_exhaustive()
    }
}

fn serialize_secret<S: serde::Serializer>(
    secret: &Secret<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(secret.expose_secret())
}

impl ChannelConfigView for FeishuAccountConfig {
    fn allowlist(&self) -> &[String] {
        &self.allowlist
    }

    fn group_allowlist(&self) -> &[String] {
        &self.group_allowlist
    }

    fn dm_policy(&self) -> DmPolicy {
        self.dm_policy.clone()
    }

    fn group_policy(&self) -> GroupPolicy {
        self.group_policy.clone()
    }

    fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    fn model_provider(&self) -> Option<&str> {
        self.model_provider.as_deref()
    }
}

impl Default for FeishuAccountConfig {
    fn default() -> Self {
        Self {
            app_id: Secret::new(String::new()),
            app_secret: Secret::new(String::new()),
            base_url: "https://open.feishu.cn".into(),
            ws_endpoint: String::new(),
            dm_policy: DmPolicy::default(),
            group_policy: GroupPolicy::default(),
            mention_mode: MentionMode::default(),
            allowlist: Vec::new(),
            group_allowlist: Vec::new(),
            model: None,
            model_provider: None,
            agent_id: None,
            allow_agent_switch: false,
            session_auto_archive_days: 30,
        }
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let cfg = FeishuAccountConfig::default();
        assert_eq!(cfg.dm_policy, DmPolicy::Allowlist);
        assert_eq!(cfg.group_policy, GroupPolicy::Open);
        assert_eq!(cfg.mention_mode, MentionMode::Mention);
        assert_eq!(cfg.session_auto_archive_days, 30);
    }

    #[test]
    fn serialize_roundtrip() {
        let cfg = FeishuAccountConfig {
            app_id: Secret::new("app".into()),
            app_secret: Secret::new("secret".into()),
            dm_policy: DmPolicy::Disabled,
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let cfg2: FeishuAccountConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg2.app_id.expose_secret(), "app");
        assert_eq!(cfg2.dm_policy, DmPolicy::Disabled);
    }
}
