use std::time::{Duration, Instant};

use {anyhow::Result, secrecy::{ExposeSecret, Secret}};

use crate::config::FeishuAccountConfig;

#[derive(Clone)]
pub struct CachedAccessToken {
    pub token: Secret<String>,
    pub expires_at: Instant,
}

pub async fn get_access_token(
    http: &reqwest::Client,
    config: &FeishuAccountConfig,
    cache: &tokio::sync::Mutex<Option<CachedAccessToken>>,
) -> Result<Secret<String>> {
    if let Ok(mut guard) = cache.try_lock() {
        if let Some(cached) = guard.as_ref() {
            if Instant::now() < cached.expires_at {
                return Ok(cached.token.clone());
            }
        }
        let token = fetch_access_token(http, config).await?;
        *guard = Some(token.clone());
        return Ok(token.token);
    }

    // Fallback to waiting for lock if try_lock fails.
    let mut guard = cache.lock().await;
    if let Some(cached) = guard.as_ref() {
        if Instant::now() < cached.expires_at {
            return Ok(cached.token.clone());
        }
    }
    let token = fetch_access_token(http, config).await?;
    *guard = Some(token.clone());
    Ok(token.token)
}

async fn fetch_access_token(
    http: &reqwest::Client,
    config: &FeishuAccountConfig,
) -> Result<CachedAccessToken> {
    let url = format!(
        "{}/open-apis/auth/v3/tenant_access_token/internal",
        config.base_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "app_id": config.app_id.expose_secret(),
        "app_secret": config.app_secret.expose_secret(),
    });
    let resp = http.post(url).json(&body).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("feishu token HTTP {status}: {text}");
    }
    let data: serde_json::Value = resp.json().await?;
    let token = data
        .get("tenant_access_token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing tenant_access_token"))?;
    let expire = data
        .get("expire")
        .and_then(|v| v.as_u64())
        .unwrap_or(3600);
    let expires_at = Instant::now() + Duration::from_secs(expire.saturating_sub(60));
    Ok(CachedAccessToken {
        token: Secret::new(token.to_string()),
        expires_at,
    })
}

pub async fn fetch_bot_open_id(
    http: &reqwest::Client,
    config: &FeishuAccountConfig,
    token: &Secret<String>,
) -> Result<Option<String>> {
    let url = format!(
        "{}/open-apis/bot/v3/info",
        config.base_url.trim_end_matches('/')
    );
    let resp = http
        .get(url)
        .bearer_auth(token.expose_secret())
        .send()
        .await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let data: serde_json::Value = resp.json().await.unwrap_or_default();
    let open_id = data
        .get("bot")
        .and_then(|v| v.get("open_id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Ok(open_id)
}
