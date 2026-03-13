use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

use {anyhow::Result, async_trait::async_trait, secrecy::ExposeSecret, url::Url};
use tracing::warn;

use moltis_agents::tool_registry::AgentTool;
use moltis_config::schema::WebReadConfig;

const JINA_READER_URL: &str = "https://r.jina.ai/";
const METASO_READER_URL: &str = "https://metaso.cn/api/v1/reader";

/// Web page full-text fetcher with 4-level auto-fallback.
pub struct WebReadTool {
    jina_key: Option<String>,
    metaso_key: Option<String>,
    jina_enabled: bool,
    metaso_enabled: bool,
    crawl4ai_endpoint: String,
    crawl4ai_token: String,
    crawl4ai_timeout_secs: u64,
    pinchtab_endpoint: String,
    pinchtab_token: String,
    pinchtab_timeout_secs: u64,
    min_chars: usize,
    ssrf_allowlist: Vec<ipnet::IpNet>,
    cache: Mutex<HashMap<String, CacheEntry>>,
    cache_ttl: Duration,
}

/// Cached fetch result with expiry.
struct CacheEntry {
    value: serde_json::Value,
    expires_at: Instant,
}

impl WebReadTool {
    pub fn from_config_with_env_overrides(
        config: &WebReadConfig,
        env_overrides: &HashMap<String, String>,
    ) -> Option<Self> {
        if !config.enabled {
            return None;
        }
        let jina_enabled = config.jina.enabled;
        let metaso_enabled = config.metaso.enabled;

        let jina_key = first_enabled_key(&config.jina)
            .map(|s| s.to_owned())
            .or_else(|| env_value_with_overrides(env_overrides, "JINA_API_KEY"));
        let metaso_key = first_enabled_key(&config.metaso)
            .map(|s| s.to_owned())
            .or_else(|| env_value_with_overrides(env_overrides, "METASO_API_KEY"));

        let ssrf_allowlist = config
            .ssrf_allowlist
            .iter()
            .filter_map(|s| match s.parse::<ipnet::IpNet>() {
                Ok(net) => Some(net),
                Err(e) => {
                    warn!("ignoring invalid ssrf_allowlist entry \"{s}\": {e}");
                    None
                }
            })
            .collect();

        let tool = Self {
            jina_key,
            metaso_key,
            jina_enabled,
            metaso_enabled,
            crawl4ai_endpoint: config.crawl4ai.endpoint.clone(),
            crawl4ai_token: config.crawl4ai.api_token.expose_secret().to_string(),
            crawl4ai_timeout_secs: config.crawl4ai.timeout_seconds,
            pinchtab_endpoint: config.pinchtab.endpoint.clone(),
            pinchtab_token: config.pinchtab.token.expose_secret().to_string(),
            pinchtab_timeout_secs: config.pinchtab.timeout_seconds,
            min_chars: config.min_chars,
            ssrf_allowlist,
            cache: Mutex::new(HashMap::new()),
            cache_ttl: Duration::from_secs(config.cache_ttl_minutes * 60),
        };

        if jina_enabled && tool.jina_key.is_none() {
            warn!("web_read: jina enabled but no API key configured");
        }
        if metaso_enabled && tool.metaso_key.is_none() {
            warn!("web_read: metaso enabled but no API key configured");
        }
        if !tool.crawl4ai_endpoint.is_empty() && tool.crawl4ai_token.is_empty() {
            warn!("web_read: crawl4ai endpoint set but api_token is missing");
        }
        if !tool.pinchtab_endpoint.is_empty() && tool.pinchtab_token.trim().is_empty() {
            warn!("web_read: pinchtab endpoint set but token is missing");
        }
        if tool.pinchtab_endpoint.trim().is_empty() && !tool.pinchtab_token.trim().is_empty() {
            warn!("web_read: pinchtab token set but endpoint is missing");
        }
        if !tool.is_configured() {
            warn!("web_read enabled but no backends are configured");
        }

        Some(tool)
    }

    fn is_configured(&self) -> bool {
        self.jina_key.is_some()
            || self.metaso_key.is_some()
            || self.crawl4ai_configured()
            || self.pinchtab_configured()
    }

    fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.jina_enabled && self.jina_key.is_none() {
            warnings.push("jina enabled but no API key configured".to_string());
        }
        if self.metaso_enabled && self.metaso_key.is_none() {
            warnings.push("metaso enabled but no API key configured".to_string());
        }
        if !self.crawl4ai_endpoint.is_empty() && self.crawl4ai_token.is_empty() {
            warnings.push("crawl4ai endpoint set but api_token is missing".to_string());
        }
        if !self.pinchtab_endpoint.is_empty() && self.pinchtab_token.trim().is_empty() {
            warnings.push("pinchtab endpoint set but token is missing".to_string());
        }
        if self.pinchtab_endpoint.trim().is_empty() && !self.pinchtab_token.trim().is_empty() {
            warnings.push("pinchtab token set but endpoint is missing".to_string());
        }
        if warnings.is_empty() && !self.is_configured() {
            warnings.push(
                "no web_read backends configured (jina/metaso/crawl4ai/pinchtab)".to_string(),
            );
        }
        warnings
    }

    fn attach_warnings(&self, mut output: serde_json::Value) -> serde_json::Value {
        let warnings = self.warnings();
        if warnings.is_empty() {
            return output;
        }
        if let Some(obj) = output.as_object_mut() {
            if !obj.contains_key("warnings") {
                obj.insert("warnings".to_string(), serde_json::Value::from(warnings));
            }
            return output;
        }
        serde_json::json!({
            "data": output,
            "warnings": warnings,
        })
    }

    fn crawl4ai_configured(&self) -> bool {
        !self.crawl4ai_endpoint.is_empty() && !self.crawl4ai_token.is_empty()
    }

    fn pinchtab_configured(&self) -> bool {
        !self.pinchtab_endpoint.trim().is_empty() && !self.pinchtab_token.trim().is_empty()
    }

    fn cache_get(&self, key: &str) -> Option<serde_json::Value> {
        let cache = self.cache.lock().ok()?;
        let entry = cache.get(key)?;
        if Instant::now() < entry.expires_at {
            Some(entry.value.clone())
        } else {
            None
        }
    }

    fn cache_set(&self, key: String, value: serde_json::Value) {
        if self.cache_ttl.is_zero() {
            return;
        }
        if let Ok(mut cache) = self.cache.lock() {
            if cache.len() > 100 {
                let now = Instant::now();
                cache.retain(|_, e| e.expires_at > now);
            }
            cache.insert(key, CacheEntry {
                value,
                expires_at: Instant::now() + self.cache_ttl,
            });
        }
    }

    fn client_fast(&self, timeout: u64) -> Result<reqwest::Client> {
        Ok(reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout))
            .connect_timeout(Duration::from_secs(10))
            .user_agent("Moltis/1.0")
            .build()?)
    }

    async fn try_jina(&self, url: &str, with_links: bool) -> Result<String> {
        let key = self
            .jina_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Jina: no key configured"))?;
        let client = self.client_fast(20)?;
        let fetch_url = format!("{}{}", JINA_READER_URL, url);
        let resp = client
            .get(&fetch_url)
            .bearer_auth(key)
            .header("x-proxy", "auto")
            .header("x-return-format", "markdown")
            .header("x-retain-images", "none")
            .header("x-with-links-summary", if with_links { "true" } else { "false" })
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jina {status} — {}", truncate(&body, 200));
        }
        Ok(resp.text().await?)
    }

    async fn try_metaso(&self, url: &str) -> Result<String> {
        let key = self
            .metaso_key
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("Metaso: no key configured"))?;
        let client = self.client_fast(20)?;
        let body = serde_json::json!({ "url": url });
        let resp = client
            .post(METASO_READER_URL)
            .bearer_auth(key)
            .header("Accept", "text/plain")
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Metaso {status} — {}", truncate(&text, 200));
        }
        let raw = resp.text().await?;
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(content) = val["content"].as_str() {
                return Ok(content.to_string());
            }
        }
        Ok(raw)
    }

    async fn try_crawl4ai(&self, url: &str) -> Result<String> {
        if !self.crawl4ai_configured() {
            anyhow::bail!("Crawl4AI not configured");
        }
        let client_timeout = self.crawl4ai_timeout_secs.saturating_add(30);
        let client = self.client_fast(client_timeout)?;
        let body = serde_json::json!({
            "urls": [url],
            "crawler_run_config": {
                "word_count_threshold": 50,
                "excluded_tags": ["nav", "footer", "header", "aside"]
            }
        });
        let resp = client
            .post(format!("{}/crawl", self.crawl4ai_endpoint.trim_end_matches('/')))
            .bearer_auth(&self.crawl4ai_token)
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("Crawl4AI {} — {}", resp.status(), url);
        }
        let data: serde_json::Value = resp.json().await?;
        let result = &data["results"][0];
        if !result["success"].as_bool().unwrap_or(true) {
            let err = result["error_message"].as_str().unwrap_or("unknown error");
            anyhow::bail!("Crawl4AI page error: {err}");
        }
        let md_field = &result["markdown"];
        let md = md_field
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| md_field["fit_markdown"].as_str().filter(|s| !s.is_empty()))
            .or_else(|| md_field["raw_markdown"].as_str().filter(|s| !s.is_empty()))
            .unwrap_or("");
        Ok(md.to_string())
    }

    async fn try_pinchtab(&self, url: &str) -> Result<String> {
        if !self.pinchtab_configured() {
            anyhow::bail!("Pinchtab not configured (token required)");
        }
        let client = self.client_fast(self.pinchtab_timeout_secs)?;
        let body = serde_json::json!({ "url": url, "format": "markdown" });
        let resp = client
            .post(format!("{}/api/fetch", self.pinchtab_endpoint.trim_end_matches('/')))
            .header("Authorization", format!("Bearer {}", self.pinchtab_token))
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Pinchtab {status} — {}", truncate(&text, 200));
        }
        Ok(resp.text().await?)
    }

    async fn fetch(&self, url: &str, with_links: bool) -> Result<serde_json::Value> {
        if !self.is_configured() {
            anyhow::bail!(
                "No web_read backends configured. Set tools.web.read (jina/metaso/crawl4ai/pinchtab) \
                 or env JINA_API_KEY / METASO_API_KEY."
            );
        }

        let parsed = Url::parse(url)?;
        ssrf_check(&parsed, &self.ssrf_allowlist).await?;

        let cache_key = format!("{url}:with_links={with_links}");
        if let Some(val) = self.cache_get(&cache_key) {
            return Ok(val);
        }

        let mut errors = Vec::new();

        match self.try_jina(url, with_links).await {
            Ok(content) => {
                if content.len() >= self.min_chars {
                    let result = fmt_result("jina", url, &content);
                    self.cache_set(cache_key, result.clone());
                    return Ok(result);
                }
            }
            Err(e) => errors.push(format!("jina: {e}")),
        }

        match self.try_metaso(url).await {
            Ok(content) => {
                if content.len() >= self.min_chars {
                    let result = fmt_result("metaso", url, &content);
                    self.cache_set(cache_key, result.clone());
                    return Ok(result);
                }
            }
            Err(e) => errors.push(format!("metaso: {e}")),
        }

        match self.try_crawl4ai(url).await {
            Ok(content) => {
                if content.len() >= self.min_chars {
                    let result = fmt_result("crawl4ai", url, &content);
                    self.cache_set(cache_key, result.clone());
                    return Ok(result);
                }
            }
            Err(e) => errors.push(format!("crawl4ai: {e}")),
        }

        match self.try_pinchtab(url).await {
            Ok(content) => {
                if content.len() >= self.min_chars {
                    let result = fmt_result("pinchtab", url, &content);
                    self.cache_set(cache_key, result.clone());
                    return Ok(result);
                }
            }
            Err(e) => errors.push(format!("pinchtab: {e}")),
        }

        anyhow::bail!("all backends failed: {}", errors.join("; "))
    }
}

#[async_trait]
impl AgentTool for WebReadTool {
    fn name(&self) -> &str {
        "web_read"
    }

    fn description(&self) -> &str {
        "Fetch web page full-text via 4-level auto-fallback chain (Jina → Metaso → Crawl4AI → Pinchtab). \
         Requires provider API keys (tools.web.read or env JINA_API_KEY / METASO_API_KEY)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "required": ["url"],
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL of the web page to fetch. Required."
                },
                "with_links": {
                    "type": "boolean",
                    "description": "Include navigation/sidebar link list in output (Jina backend only).",
                    "default": false
                }
            }
        })
    }

    async fn execute(&self, params: serde_json::Value) -> Result<serde_json::Value> {
        let url = params
            .get("url")
            .and_then(|v| v.as_str())
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("missing 'url' parameter"))?;

        let with_links = params.get("with_links").and_then(|v| v.as_bool()).unwrap_or(false);

        let result = self.fetch(url, with_links).await?;
        Ok(self.attach_warnings(result))
    }
}

fn fmt_result(backend: &str, url: &str, content: &str) -> serde_json::Value {
    serde_json::json!({
        "backend": backend,
        "url": url,
        "length": content.len(),
        "content": content,
    })
}

fn first_enabled_key(provider: &moltis_config::schema::ApiKeyProviderConfig) -> Option<&str> {
    if !provider.enabled {
        return None;
    }
    provider
        .accounts
        .iter()
        .find(|a| a.enabled && !a.api_key.expose_secret().trim().is_empty())
        .map(|a| a.api_key.expose_secret().as_str())
}

fn truncate(s: &str, max: usize) -> &str {
    let end = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
    &s[..end]
}

fn env_value_with_overrides(
    env_overrides: &HashMap<String, String>,
    key: &str,
) -> Option<String> {
    std::env::var(key).ok().or_else(|| env_overrides.get(key).cloned())
}

fn is_ssrf_allowed(ip: &IpAddr, allowlist: &[ipnet::IpNet]) -> bool {
    allowlist.iter().any(|net| net.contains(ip))
}

async fn ssrf_check(url: &Url, allowlist: &[ipnet::IpNet]) -> Result<()> {
    let host = url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("URL has no host"))?;

    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_private_ip(&ip) && !is_ssrf_allowed(&ip, allowlist) {
            anyhow::bail!("SSRF blocked: {host}");
        }
        return Ok(());
    }

    let port = if url.scheme() == "http" { 80 } else { 443 };
    let addrs = tokio::net::lookup_host((host, port)).await?;
    for addr in addrs {
        if is_private_ip(&addr.ip()) && !is_ssrf_allowed(&addr.ip(), allowlist) {
            anyhow::bail!("SSRF blocked: {host} -> {}", addr.ip());
        }
    }
    Ok(())
}

fn is_private_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_web_read() {
        let mut cfg = WebReadConfig::default();
        cfg.enabled = true;
        let tool = WebReadTool::from_config_with_env_overrides(&cfg, &HashMap::new()).unwrap();
        assert_eq!(tool.name(), "web_read");
    }

    #[test]
    fn schema_requires_url() {
        let mut cfg = WebReadConfig::default();
        cfg.enabled = true;
        let tool = WebReadTool::from_config_with_env_overrides(&cfg, &HashMap::new()).unwrap();
        let schema = tool.parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "url"));
    }

    #[test]
    fn warnings_include_missing_keys() {
        let mut cfg = WebReadConfig::default();
        cfg.enabled = true;
        cfg.jina.enabled = true;
        cfg.metaso.enabled = true;
        let tool = WebReadTool::from_config_with_env_overrides(&cfg, &HashMap::new()).unwrap();
        let warnings = tool.warnings();
        assert!(warnings.iter().any(|w| w.contains("jina")));
        assert!(warnings.iter().any(|w| w.contains("metaso")));
    }

    #[test]
    fn warnings_include_pinchtab_endpoint_mismatch() {
        let mut cfg = WebReadConfig::default();
        cfg.enabled = true;
        cfg.pinchtab.endpoint = String::new();
        cfg.pinchtab.token = secrecy::Secret::new("token".to_string());
        let tool = WebReadTool::from_config_with_env_overrides(&cfg, &HashMap::new()).unwrap();
        let warnings = tool.warnings();
        assert!(warnings.iter().any(|w| w.contains("pinchtab token set but endpoint is missing")));
    }

    #[test]
    fn env_override_provides_missing_reader_key() {
        let mut cfg = WebReadConfig::default();
        cfg.enabled = true;
        cfg.jina.enabled = true;

        let tool = WebReadTool::from_config_with_env_overrides(
            &cfg,
            &HashMap::from([("JINA_API_KEY".to_string(), "env-key".to_string())]),
        )
        .unwrap();

        assert!(tool.is_configured());
        assert!(tool.warnings().iter().all(|w| !w.contains("jina enabled")));
    }
}
