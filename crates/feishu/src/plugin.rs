use std::{
    collections::HashMap,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use {
    async_trait::async_trait,
    secrecy::ExposeSecret,
    tracing::{info, warn},
};

use moltis_channels::{
    ChannelConfigView, ChannelEventSink, Error as ChannelError, Result as ChannelResult,
    message_log::MessageLog,
    plugin::{
        ChannelHealthSnapshot, ChannelOutbound, ChannelPlugin, ChannelStatus, ChannelStreamOutbound,
    },
};

use crate::{
    auth::{fetch_bot_open_id, get_access_token},
    config::FeishuAccountConfig,
    outbound::FeishuOutbound,
    state::{AccountState, AccountStateMap},
    ws::run_ws,
};

/// Cache TTL for probe results (30 seconds).
const PROBE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Feishu channel plugin.
pub struct FeishuPlugin {
    accounts: AccountStateMap,
    outbound: FeishuOutbound,
    message_log: Option<Arc<dyn MessageLog>>,
    event_sink: Option<Arc<dyn ChannelEventSink>>,
    probe_cache: RwLock<HashMap<String, (ChannelHealthSnapshot, Instant)>>,
}

impl FeishuPlugin {
    pub fn new() -> Self {
        let accounts: AccountStateMap = Arc::new(RwLock::new(HashMap::new()));
        let outbound = FeishuOutbound {
            accounts: Arc::clone(&accounts),
        };
        Self {
            accounts,
            outbound,
            message_log: None,
            event_sink: None,
            probe_cache: RwLock::new(HashMap::new()),
        }
    }

    pub fn with_message_log(mut self, log: Arc<dyn MessageLog>) -> Self {
        self.message_log = Some(log);
        self
    }

    pub fn with_event_sink(mut self, sink: Arc<dyn ChannelEventSink>) -> Self {
        self.event_sink = Some(sink);
        self
    }

    pub fn shared_outbound(&self) -> Arc<dyn ChannelOutbound> {
        Arc::new(FeishuOutbound {
            accounts: Arc::clone(&self.accounts),
        })
    }

    pub fn shared_stream_outbound(&self) -> Arc<dyn ChannelStreamOutbound> {
        Arc::new(FeishuOutbound {
            accounts: Arc::clone(&self.accounts),
        })
    }

    pub fn account_ids(&self) -> Vec<String> {
        let accounts = self.accounts.read().unwrap_or_else(|e| e.into_inner());
        accounts.keys().cloned().collect()
    }

    pub fn has_account(&self, account_id: &str) -> bool {
        let accounts = self.accounts.read().unwrap_or_else(|e| e.into_inner());
        accounts.contains_key(account_id)
    }

    pub fn account_config(&self, account_id: &str) -> Option<serde_json::Value> {
        let accounts = self.accounts.read().unwrap_or_else(|e| e.into_inner());
        accounts
            .get(account_id)
            .and_then(|s| serde_json::to_value(&s.config).ok())
    }

    pub fn update_account_config(
        &self,
        account_id: &str,
        config: serde_json::Value,
    ) -> ChannelResult<()> {
        let parsed: FeishuAccountConfig = serde_json::from_value(config)?;
        let mut accounts = self.accounts.write().unwrap_or_else(|e| e.into_inner());
        if let Some(state) = accounts.get_mut(account_id) {
            state.config = parsed;
            Ok(())
        } else {
            Err(ChannelError::unknown_account(account_id))
        }
    }
}

impl Default for FeishuPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ChannelPlugin for FeishuPlugin {
    fn id(&self) -> &str {
        "feishu"
    }

    fn name(&self) -> &str {
        "Feishu"
    }

    async fn start_account(
        &mut self,
        account_id: &str,
        config: serde_json::Value,
    ) -> ChannelResult<()> {
        let cfg: FeishuAccountConfig = serde_json::from_value(config)?;
        if cfg.app_id.expose_secret().is_empty() || cfg.app_secret.expose_secret().is_empty() {
            return Err(ChannelError::invalid_input(
                "feishu app_id and app_secret are required",
            ));
        }

        info!(account_id, "starting feishu account");

        let http = reqwest::Client::new();
        let token_cache = Arc::new(tokio::sync::Mutex::new(None));
        let cancel = tokio_util::sync::CancellationToken::new();
        let mut state = AccountState {
            config: cfg.clone(),
            cancel: cancel.clone(),
            http: http.clone(),
            token_cache: Arc::clone(&token_cache),
            bot_open_id: None,
            ws_connected: Arc::new(AtomicBool::new(false)),
        };

        if let Ok(token) = get_access_token(&http, &cfg, &token_cache).await {
            state.bot_open_id = fetch_bot_open_id(&http, &cfg, &token).await.ok().flatten();
        }

        let message_log = self.message_log.clone();
        let event_sink = self.event_sink.clone();
        let account_id_owned = account_id.to_string();
        let state_for_store = state.clone();
        tokio::spawn(async move {
            if let Err(e) = run_ws(account_id_owned, state, message_log, event_sink).await {
                warn!(error = %e, "feishu ws loop stopped");
            }
        });

        let mut accounts = self.accounts.write().unwrap_or_else(|e| e.into_inner());
        accounts.insert(account_id.to_string(), state_for_store);

        Ok(())
    }

    async fn stop_account(&mut self, account_id: &str) -> ChannelResult<()> {
        let cancel = {
            let accounts = self.accounts.read().unwrap_or_else(|e| e.into_inner());
            accounts.get(account_id).map(|s| s.cancel.clone())
        };

        if let Some(cancel) = cancel {
            info!(account_id, "stopping feishu account");
            cancel.cancel();
            let mut accounts = self.accounts.write().unwrap_or_else(|e| e.into_inner());
            accounts.remove(account_id);
        } else {
            warn!(account_id, "feishu account not found");
        }

        Ok(())
    }

    fn outbound(&self) -> Option<&dyn ChannelOutbound> {
        Some(&self.outbound)
    }

    fn status(&self) -> Option<&dyn ChannelStatus> {
        Some(self)
    }

    fn has_account(&self, account_id: &str) -> bool {
        FeishuPlugin::has_account(self, account_id)
    }

    fn account_ids(&self) -> Vec<String> {
        FeishuPlugin::account_ids(self)
    }

    fn account_config(&self, account_id: &str) -> Option<Box<dyn ChannelConfigView>> {
        let accounts = self.accounts.read().unwrap_or_else(|e| e.into_inner());
        accounts
            .get(account_id)
            .map(|state| Box::new(state.config.clone()) as Box<dyn ChannelConfigView>)
    }

    fn update_account_config(
        &self,
        account_id: &str,
        config: serde_json::Value,
    ) -> ChannelResult<()> {
        FeishuPlugin::update_account_config(self, account_id, config)
    }

    fn shared_outbound(&self) -> Arc<dyn ChannelOutbound> {
        FeishuPlugin::shared_outbound(self)
    }

    fn shared_stream_outbound(&self) -> Arc<dyn ChannelStreamOutbound> {
        FeishuPlugin::shared_stream_outbound(self)
    }

    fn account_config_json(&self, account_id: &str) -> Option<serde_json::Value> {
        FeishuPlugin::account_config(self, account_id)
    }
}

#[async_trait]
impl ChannelStatus for FeishuPlugin {
    async fn probe(&self, account_id: &str) -> ChannelResult<ChannelHealthSnapshot> {
        if let Some((snap, ts)) = self
            .probe_cache
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(account_id)
            .cloned()
        {
            if ts.elapsed() < PROBE_CACHE_TTL {
                return Ok(snap);
            }
        }

        let state = {
            let accounts = self.accounts.read().unwrap_or_else(|e| e.into_inner());
            accounts
                .get(account_id)
                .cloned()
                .ok_or_else(|| ChannelError::unknown_account(account_id))?
        };

        let token_ok = get_access_token(&state.http, &state.config, &state.token_cache)
            .await
            .is_ok();
        let connected = token_ok && state.ws_connected.load(Ordering::Relaxed);
        let snap = ChannelHealthSnapshot {
            connected,
            account_id: account_id.to_string(),
            details: Some(if connected {
                "ok".to_string()
            } else if !token_ok {
                "auth failed".to_string()
            } else {
                "ws disconnected".to_string()
            }),
        };
        if let Ok(mut cache) = self.probe_cache.write() {
            cache.insert(account_id.to_string(), (snap.clone(), Instant::now()));
        }
        Ok(snap)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn test_account_state(config: FeishuAccountConfig) -> AccountState {
        AccountState {
            config,
            cancel: tokio_util::sync::CancellationToken::new(),
            http: reqwest::Client::new(),
            token_cache: Arc::new(tokio::sync::Mutex::new(None)),
            bot_open_id: None,
            ws_connected: Arc::new(AtomicBool::new(false)),
        }
    }

    #[tokio::test]
    async fn start_account_requires_credentials() {
        let mut plugin = FeishuPlugin::new();
        let err = plugin
            .start_account("bot", serde_json::json!({}))
            .await
            .unwrap_err();

        assert!(err.to_string().contains("app_id and app_secret"));
        assert!(!plugin.has_account("bot"));
    }

    #[test]
    fn update_account_config_replaces_live_config() {
        let plugin = FeishuPlugin::new();
        {
            let mut accounts = plugin.accounts.write().unwrap();
            accounts.insert(
                "bot".to_string(),
                test_account_state(FeishuAccountConfig {
                    app_id: secrecy::Secret::new("old-app".to_string()),
                    app_secret: secrecy::Secret::new("old-secret".to_string()),
                    allow_agent_switch: false,
                    session_auto_archive_days: 30,
                    ..Default::default()
                }),
            );
        }

        plugin
            .update_account_config(
                "bot",
                serde_json::json!({
                    "app_id": "new-app",
                    "app_secret": "new-secret",
                    "allow_agent_switch": true,
                    "session_auto_archive_days": 7
                }),
            )
            .unwrap();

        let cfg = plugin.account_config("bot").unwrap();
        assert_eq!(cfg["app_id"], "new-app");
        assert_eq!(cfg["allow_agent_switch"], true);
        assert_eq!(cfg["session_auto_archive_days"], 7);
    }

    #[tokio::test]
    async fn stop_account_removes_live_account() {
        let mut plugin = FeishuPlugin::new();
        {
            let mut accounts = plugin.accounts.write().unwrap();
            accounts.insert(
                "bot".to_string(),
                test_account_state(FeishuAccountConfig {
                    app_id: secrecy::Secret::new("app".to_string()),
                    app_secret: secrecy::Secret::new("secret".to_string()),
                    ..Default::default()
                }),
            );
        }

        plugin.stop_account("bot").await.unwrap();

        assert!(!plugin.has_account("bot"));
    }
}
