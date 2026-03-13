use std::{
    collections::HashMap,
    sync::{atomic::{AtomicUsize, Ordering}, Arc},
    time::Duration,
};

use {anyhow::Result, async_trait::async_trait, secrecy::ExposeSecret, tracing::warn};

use moltis_agents::tool_registry::AgentTool;
use moltis_config::schema::WebCnSearchConfig;

/// API endpoints.
const METASO_SEARCH_URL: &str = "https://metaso.cn/api/v1/search";
const METASO_CHAT_URL: &str = "https://metaso.cn/api/v1/chat/completions";
const BOCHA_SEARCH_URL: &str = "https://api.bocha.cn/v1/web-search";
const ANSPIRE_SEARCH_URL: &str = "https://plugin.anspire.cn/api/ntsearch/search";
const ANSPIRE_PROSEARCH_URL: &str = "https://plugin.anspire.cn/api/ntsearch/prosearch";
const JINA_SEARCH_URL: &str = "https://s.jina.ai/";

/// Domain trust tiers (edu_hss profile).
const HIGH_TRUST: &[&str] = &[
    "gov.cn", "moe.gov.cn",
    "people.com.cn", "news.cn", "xinhuanet.com",
    "gmw.cn",
    "qstheory.cn",
    "jyb.cn",
    "edu.cn", "cnki.net", "cssn.cn", "cass.cn",
];
const MEDIUM_TRUST: &[&str] = &[
    "thepaper.cn", "caixin.com", "guancha.cn",
    "chinanews.com.cn",
    "jyb.com.cn",
];
const LOW_TRUST: &[&str] = &[
    "baijiahao.baidu.com", "sohu.com", "toutiao.com", "csdn.net",
    "mp.weixin.qq.com",
];

/// Thread-safe round-robin key selector.
struct AccountPool {
    keys: Vec<(String, String)>, // (name, api_key)
    counter: AtomicUsize,
}

impl AccountPool {
    fn new(entries: &[moltis_config::schema::ApiKeyEntry]) -> Self {
        let keys = entries
            .iter()
            .filter(|e| e.enabled && !e.api_key.expose_secret().trim().is_empty())
            .map(|e| (e.name.clone(), e.api_key.expose_secret().to_string()))
            .collect();
        Self { keys, counter: AtomicUsize::new(0) }
    }

    fn from_env(name: &str, api_key: &str) -> Self {
        let mut keys = Vec::new();
        if !api_key.trim().is_empty() {
            keys.push((name.to_string(), api_key.to_string()));
        }
        Self { keys, counter: AtomicUsize::new(0) }
    }

    fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    fn len(&self) -> usize {
        self.keys.len()
    }

    fn next_key(&self) -> Option<&str> {
        if self.keys.is_empty() {
            return None;
        }
        let idx = self.counter.fetch_add(1, Ordering::Relaxed) % self.keys.len();
        Some(&self.keys[idx].1)
    }

    fn key_at(&self, offset: usize) -> Option<&str> {
        if self.keys.is_empty() {
            return None;
        }
        Some(&self.keys[offset % self.keys.len()].1)
    }
}

#[derive(Debug, Clone)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
    source: String,
    time: String,
}

/// Chinese web search and Q&A tool.
pub struct WebCnSearchTool {
    metaso: Arc<AccountPool>,
    bocha: Arc<AccountPool>,
    anspire: Arc<AccountPool>,
    jina: Arc<AccountPool>,
    metaso_enabled: bool,
    bocha_enabled: bool,
    anspire_enabled: bool,
    jina_enabled: bool,
    timeout_secs: u64,
}

impl WebCnSearchTool {
    pub fn from_config_with_env_overrides(
        config: &WebCnSearchConfig,
        env_overrides: &HashMap<String, String>,
    ) -> Option<Self> {
        if !config.enabled {
            return None;
        }

        let metaso_enabled = config.metaso.enabled;
        let bocha_enabled = config.bocha.enabled;
        let anspire_enabled = config.anspire.enabled;
        let jina_enabled = config.jina.enabled;

        let metaso = if metaso_enabled {
            Arc::new(AccountPool::new(&config.metaso.accounts))
        } else {
            Arc::new(AccountPool::new(&[]))
        };
        let bocha = if bocha_enabled {
            Arc::new(AccountPool::new(&config.bocha.accounts))
        } else {
            Arc::new(AccountPool::new(&[]))
        };
        let anspire = if anspire_enabled {
            Arc::new(AccountPool::new(&config.anspire.accounts))
        } else {
            Arc::new(AccountPool::new(&[]))
        };
        let jina = if jina_enabled {
            Arc::new(AccountPool::new(&config.jina.accounts))
        } else {
            Arc::new(AccountPool::new(&[]))
        };

        let metaso = Self::merge_env_key(
            metaso,
            env_overrides,
            "METASO_API_KEY",
            "metaso",
        );
        let bocha = Self::merge_env_key(
            bocha,
            env_overrides,
            "BOCHA_API_KEY",
            "bocha",
        );
        let anspire = Self::merge_env_key(
            anspire,
            env_overrides,
            "ANSPIRE_API_KEY",
            "anspire",
        );
        let jina = Self::merge_env_key(
            jina,
            env_overrides,
            "JINA_API_KEY",
            "jina",
        );

        if metaso_enabled && metaso.is_empty() {
            warn!("web_cn_search: metaso enabled but no API key configured");
        }
        if bocha_enabled && bocha.is_empty() {
            warn!("web_cn_search: bocha enabled but no API key configured");
        }
        if anspire_enabled && anspire.is_empty() {
            warn!("web_cn_search: anspire enabled but no API key configured");
        }
        if jina_enabled && jina.is_empty() {
            warn!("web_cn_search: jina enabled but no API key configured");
        }
        if metaso.is_empty() && bocha.is_empty() && anspire.is_empty() && jina.is_empty() {
            warn!("web_cn_search enabled but no providers are configured");
        }

        Some(Self {
            metaso,
            bocha,
            anspire,
            jina,
            metaso_enabled,
            bocha_enabled,
            anspire_enabled,
            jina_enabled,
            timeout_secs: config.timeout_seconds,
        })
    }

    fn merge_env_key(
        pool: Arc<AccountPool>,
        env_overrides: &HashMap<String, String>,
        env_key: &str,
        name: &str,
    ) -> Arc<AccountPool> {
        if !pool.is_empty() {
            return pool;
        }
        if let Some(value) = env_value_with_overrides(env_overrides, env_key) {
            return Arc::new(AccountPool::from_env(name, &value));
        }
        pool
    }

    fn has_any_accounts(&self) -> bool {
        !self.metaso.is_empty()
            || !self.bocha.is_empty()
            || !self.anspire.is_empty()
            || !self.jina.is_empty()
    }

    fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        if self.metaso_enabled && self.metaso.is_empty() {
            warnings.push("metaso enabled but no API key configured".to_string());
        }
        if self.bocha_enabled && self.bocha.is_empty() {
            warnings.push("bocha enabled but no API key configured".to_string());
        }
        if self.anspire_enabled && self.anspire.is_empty() {
            warnings.push("anspire enabled but no API key configured".to_string());
        }
        if self.jina_enabled && self.jina.is_empty() {
            warnings.push("jina enabled but no API key configured".to_string());
        }
        if warnings.is_empty() && !self.has_any_accounts() {
            warnings.push(
                "no cn search accounts configured (tools.web.cn_search or env METASO/BOCHA/ANSPIRE/JINA API keys)"
                    .to_string(),
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

    fn client(&self) -> Result<reqwest::Client> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::ACCEPT_ENCODING,
            reqwest::header::HeaderValue::from_static("identity"),
        );
        Ok(reqwest::Client::builder()
            .timeout(Duration::from_secs(self.timeout_secs))
            .connect_timeout(Duration::from_secs(10))
            .user_agent("Moltis/1.0")
            .default_headers(headers)
            .build()?)
    }

    async fn decode_json_response(
        provider: &str,
        resp: reqwest::Response,
    ) -> Result<serde_json::Value> {
        let body = resp
            .bytes()
            .await
            .map_err(|e| anyhow::anyhow!("{provider} response decode failed: {e}"))?;
        serde_json::from_slice(&body).map_err(|e| {
            let mut preview = String::from_utf8_lossy(&body).into_owned();
            if preview.chars().count() > 240 {
                preview = preview.chars().take(240).collect::<String>() + "...";
            }
            anyhow::anyhow!("{provider} invalid JSON response: {e}; body={preview}")
        })
    }

    fn score_domain(url: &str) -> f32 {
        let domain = url
            .split("//")
            .nth(1)
            .unwrap_or(url)
            .split('/')
            .next()
            .unwrap_or("")
            .to_lowercase();

        if HIGH_TRUST.iter().any(|d| domain.ends_with(d)) {
            2.0
        } else if MEDIUM_TRUST.iter().any(|d| domain.ends_with(d)) {
            1.0
        } else if LOW_TRUST.iter().any(|d| domain.ends_with(d)) {
            -0.3
        } else {
            0.0
        }
    }

    fn score_recency(time: &str) -> f32 {
        if time.contains("hour") || time.contains("小时") {
            0.8
        } else if time.contains("day") || time.contains("天") {
            0.4
        } else if time.contains("month") || time.contains("月") {
            0.1
        } else {
            0.0
        }
    }

    fn score_title(query: &str, title: &str) -> f32 {
        let q = query.to_lowercase();
        let t = title.to_lowercase();
        q.split_whitespace().filter(|w| t.contains(w)).count() as f32 * 0.4
    }

    fn rerank(results: Vec<SearchResult>, query: &str, limit: usize) -> Vec<SearchResult> {
        let mut scored: Vec<(f32, SearchResult)> = results
            .into_iter()
            .map(|r| {
                let score = Self::score_domain(&r.url)
                    + Self::score_recency(&r.time)
                    + Self::score_title(query, &r.title);
                (score, r)
            })
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(limit).map(|(_, r)| r).collect()
    }

    fn results_to_json(results: &[SearchResult]) -> Vec<serde_json::Value> {
        results
            .iter()
            .map(|r| {
                serde_json::json!({
                    "title": r.title,
                    "url": r.url,
                    "snippet": r.snippet,
                    "source": r.source,
                    "time": r.time,
                })
            })
            .collect()
    }

    async fn exec_search(&self, c: &reqwest::Client, a: &serde_json::Value) -> Result<serde_json::Value> {
        let query = require_query(a)?;
        let limit = usize_arg(a, "limit", 5).clamp(1, 50);
        let site = opt_str(a, "site");
        let freshness = opt_str(a, "freshness");
        let per = ((limit * 2) as u32).max(5).min(10);

        let (results, errors) = self
            .multi_search(c, &query, per, site.as_deref(), freshness.as_deref())
            .await;
        let ranked = Self::rerank(results, &query, limit);

        if ranked.is_empty() {
            let base = format!("No results for: {query}");
            return Ok(serde_json::json!({
                "query": query,
                "count": 0,
                "results": [],
                "errors": errors,
                "note": base,
            }));
        }

        Ok(serde_json::json!({
            "query": query,
            "count": ranked.len(),
            "results": Self::results_to_json(&ranked),
            "errors": errors,
        }))
    }

    async fn exec_metaso_search(
        &self,
        c: &reqwest::Client,
        a: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let query = require_query(a)?;
        let size = u32_arg(a, "limit", 5).clamp(1, 100);
        let scope = a.get("scope").and_then(|v| v.as_str()).unwrap_or("webpage");
        let include_summary = bool_arg(a, "include_summary", true);
        let include_raw = bool_arg(a, "include_raw_content", false);
        let concise_snippet = bool_arg(a, "concise_snippet", false);
        let resp = self
            .metaso_search(c, &query, size, scope, include_summary, include_raw, concise_snippet)
            .await?;
        Ok(resp)
    }

    async fn exec_metaso_chat(
        &self,
        c: &reqwest::Client,
        a: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let query = require_query(a)?;
        let messages = a
            .get("messages")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_else(|| vec![serde_json::json!({"role": "user", "content": query})]);
        let model = a.get("model").and_then(|v| v.as_str()).unwrap_or("fast");
        let chat_format = a
            .get("chat_format")
            .and_then(|v| v.as_str())
            .unwrap_or("chat_completions");
        let resp = self.metaso_chat(c, &messages, model, chat_format).await?;
        Ok(resp)
    }

    async fn exec_bocha_search(
        &self,
        c: &reqwest::Client,
        a: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let query = require_query(a)?;
        let freshness = a.get("freshness").and_then(|v| v.as_str()).unwrap_or("noLimit");
        let summary = bool_arg(a, "bocha_summary", true);
        let include_domains = opt_str(a, "include_domains");
        let exclude_domains = opt_str(a, "exclude_domains");
        let resp = self
            .bocha_search(c, &query, freshness, summary, include_domains.as_deref(), exclude_domains.as_deref())
            .await?;
        Ok(resp)
    }

    async fn exec_anspire_search(
        &self,
        c: &reqwest::Client,
        a: &serde_json::Value,
        pro: bool,
    ) -> Result<serde_json::Value> {
        let query = require_query(a)?;
        let top_k = u32_arg(a, "limit", 5).clamp(1, 50);
        let site = opt_str(a, "site");
        let from_time = opt_str(a, "from_time");
        let to_time = opt_str(a, "to_time");
        let resp = self
            .anspire_search(
                c,
                &query,
                top_k,
                site.as_deref(),
                from_time.as_deref(),
                to_time.as_deref(),
                pro,
            )
            .await?;
        Ok(resp)
    }

    async fn exec_jina_search(
        &self,
        c: &reqwest::Client,
        a: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let query = require_query(a)?;
        let count = u32_arg(a, "limit", 5).clamp(1, 20);
        let with_content = bool_arg(a, "with_content", false);
        let resp = self.jina_search(c, &query, count, with_content, None).await?;
        Ok(resp)
    }

    async fn exec_jina_site_search(
        &self,
        c: &reqwest::Client,
        a: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let query = require_query(a)?;
        let site = opt_str(a, "site").ok_or_else(|| anyhow::anyhow!("missing 'site'"))?;
        let count = u32_arg(a, "limit", 5).clamp(1, 20);
        let with_content = bool_arg(a, "with_content", false);
        let resp = self
            .jina_search(c, &query, count, with_content, Some(&site))
            .await?;
        Ok(resp)
    }

    async fn metaso_search(
        &self,
        client: &reqwest::Client,
        query: &str,
        size: u32,
        scope: &str,
        include_summary: bool,
        include_raw_content: bool,
        concise_snippet: bool,
    ) -> Result<serde_json::Value> {
        let pool = &self.metaso;
        if pool.is_empty() {
            anyhow::bail!("Metaso: no accounts configured");
        }
        let body = serde_json::json!({
            "q": query,
            "size": size.clamp(1, 100),
            "scope": scope,
            "includeSummary": include_summary,
            "includeRawContent": include_raw_content,
            "conciseSnippet": concise_snippet,
        });

        let mut err = None;
        for i in 0..pool.len().max(1) {
            let key = pool.key_at(i).unwrap_or("");
            let resp = client
                .post(METASO_SEARCH_URL)
                .bearer_auth(key)
                .json(&body)
                .send()
                .await?;
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                err = Some("Metaso rate-limited".to_string());
                continue;
            }
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("Metaso {status} — {text}");
            }
            return Self::decode_json_response("Metaso", resp).await;
        }
        anyhow::bail!(err.unwrap_or_else(|| "Metaso: failed".into()));
    }

    async fn metaso_chat(
        &self,
        client: &reqwest::Client,
        messages: &[serde_json::Value],
        model: &str,
        chat_format: &str,
    ) -> Result<serde_json::Value> {
        let pool = &self.metaso;
        if pool.is_empty() {
            anyhow::bail!("Metaso: no accounts configured");
        }
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "chat_format": chat_format,
        });

        let mut err = None;
        for i in 0..pool.len().max(1) {
            let key = pool.key_at(i).unwrap_or("");
            let resp = client
                .post(METASO_CHAT_URL)
                .bearer_auth(key)
                .json(&body)
                .send()
                .await?;
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                err = Some("Metaso rate-limited".to_string());
                continue;
            }
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("Metaso {status} — {text}");
            }
            return Self::decode_json_response("Metaso", resp).await;
        }
        anyhow::bail!(err.unwrap_or_else(|| "Metaso: failed".into()));
    }

    async fn bocha_search(
        &self,
        client: &reqwest::Client,
        query: &str,
        freshness: &str,
        summary: bool,
        include_domains: Option<&str>,
        exclude_domains: Option<&str>,
    ) -> Result<serde_json::Value> {
        let pool = &self.bocha;
        if pool.is_empty() {
            anyhow::bail!("Bocha: no accounts configured");
        }
        let body = serde_json::json!({
            "query": query,
            "freshness": freshness,
            "summary": summary,
            "include_domains": include_domains,
            "exclude_domains": exclude_domains,
        });

        let mut err = None;
        for i in 0..pool.len().max(1) {
            let key = pool.key_at(i).unwrap_or("");
            let resp = client
                .post(BOCHA_SEARCH_URL)
                .bearer_auth(key)
                .json(&body)
                .send()
                .await?;
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                err = Some("Bocha rate-limited".to_string());
                continue;
            }
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("Bocha {status} — {text}");
            }
            return Self::decode_json_response("Bocha", resp).await;
        }
        anyhow::bail!(err.unwrap_or_else(|| "Bocha: failed".into()));
    }

    async fn anspire_search(
        &self,
        client: &reqwest::Client,
        query: &str,
        top_k: u32,
        site: Option<&str>,
        from_time: Option<&str>,
        to_time: Option<&str>,
        pro: bool,
    ) -> Result<serde_json::Value> {
        let pool = &self.anspire;
        if pool.is_empty() {
            anyhow::bail!("Anspire: no accounts configured");
        }
        let url = if pro { ANSPIRE_PROSEARCH_URL } else { ANSPIRE_SEARCH_URL };
        let mut params: Vec<(&str, String)> = vec![
            ("query", query.to_string()),
            ("top_k", top_k.clamp(1, 50).to_string()),
        ];
        if let Some(value) = site.filter(|v| !v.trim().is_empty()) {
            params.push(("Insite", value.to_string()));
        }
        if let Some(value) = from_time.filter(|v| !v.trim().is_empty()) {
            params.push(("FromTime", value.to_string()));
        }
        if let Some(value) = to_time.filter(|v| !v.trim().is_empty()) {
            params.push(("ToTime", value.to_string()));
        }

        let mut err = None;
        for i in 0..pool.len().max(1) {
            let key = pool.key_at(i).unwrap_or("");
            let resp = client
                .get(url)
                .bearer_auth(key)
                .header(reqwest::header::ACCEPT, "*/*")
                .header(reqwest::header::CONNECTION, "keep-alive")
                .query(&params)
                .send()
                .await?;
            if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                err = Some("Anspire rate-limited".to_string());
                continue;
            }
            if !resp.status().is_success() {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                anyhow::bail!("Anspire {status} — {text}");
            }
            return Self::decode_json_response("Anspire", resp).await;
        }
        anyhow::bail!(err.unwrap_or_else(|| "Anspire: failed".into()));
    }

    async fn jina_search(
        &self,
        client: &reqwest::Client,
        query: &str,
        count: u32,
        with_content: bool,
        site: Option<&str>,
    ) -> Result<serde_json::Value> {
        let pool = &self.jina;
        if pool.is_empty() {
            anyhow::bail!("Jina: no accounts configured");
        }
        let key = pool.next_key().unwrap_or("");
        let count_str = count.clamp(1, 20).to_string();
        let mut req = client
            .get(JINA_SEARCH_URL)
            .bearer_auth(key)
            .header(reqwest::header::ACCEPT, "application/json")
            .query(&[
                ("q", query),
                ("gl", "CN"),
                ("hl", "zh-cn"),
                ("num", count_str.as_str()),
            ]);
        if !with_content {
            req = req.header("x-respond-with", "no-content");
        }
        if let Some(site) = site {
            req = req.header("x-site", site);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Jina {status} — {text}");
        }
        Self::decode_json_response("Jina", resp).await
    }

    async fn multi_search(
        &self,
        c: &reqwest::Client,
        query: &str,
        per: u32,
        site: Option<&str>,
        freshness: Option<&str>,
    ) -> (Vec<SearchResult>, Vec<String>) {
        let mut errors = Vec::new();
        let bocha_freshness = freshness.unwrap_or("noLimit");

        let metaso = async {
            if self.metaso.is_empty() {
                return Ok(None);
            }
            self.metaso_search(c, query, per, "webpage", true, false, true)
                .await
                .map(Some)
        };
        let bocha = async {
            if self.bocha.is_empty() {
                return Ok(None);
            }
            self.bocha_search(c, query, bocha_freshness, true, site, None)
                .await
                .map(Some)
        };
        let anspire = async {
            if self.anspire.is_empty() {
                return Ok(None);
            }
            self.anspire_search(c, query, per, site, None, None, false)
                .await
                .map(Some)
        };
        let jina = async {
            if self.jina.is_empty() {
                return Ok(None);
            }
            self.jina_search(c, query, per, false, site).await.map(Some)
        };

        let (metaso, bocha, anspire, jina) = tokio::join!(metaso, bocha, anspire, jina);
        let mut results = Vec::new();
        for value in [metaso, bocha, anspire, jina] {
            match value {
                Ok(Some(val)) => results.extend(parse_results(&val)),
                Ok(None) => {}
                Err(e) => errors.push(e.to_string()),
            }
        }
        (results, errors)
    }
}

#[async_trait]
impl AgentTool for WebCnSearchTool {
    fn name(&self) -> &str {
        "web_cn_search"
    }

    fn description(&self) -> &str {
        "Chinese web search and AI Q&A via Metaso, Bocha, Anspire, and Jina. \
         Requires provider API keys (tools.web.cn_search or env METASO_API_KEY / \
         BOCHA_API_KEY / ANSPIRE_API_KEY / JINA_API_KEY). \
         For full-text fetch, use web_read."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "Which capability to invoke. Default: 'search'.",
                    "enum": [
                        "search", "metaso_search", "metaso_chat",
                        "bocha_search", "anspire_search", "anspire_prosearch",
                        "jina_search", "jina_site_search"
                    ],
                    "default": "search"
                },
                "query": {
                    "type": "string",
                    "description": "Search query or question."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results to return (1-50, default 5).",
                    "default": 5
                },
                "site": {
                    "type": "string",
                    "description": "Restrict results to this domain (e.g. 'jyj.beijing.gov.cn')."
                },
                "scope": {
                    "type": "string",
                    "description": "Metaso search scope.",
                    "enum": ["webpage", "document", "paper", "image", "video", "podcast"],
                    "default": "webpage"
                },
                "include_summary": {
                    "type": "boolean",
                    "description": "Metaso: include snippet summaries. Default true.",
                    "default": true
                },
                "include_raw_content": {
                    "type": "boolean",
                    "description": "Metaso: include full raw page content. Default false.",
                    "default": false
                },
                "messages": {
                    "type": "array",
                    "description": "Chat messages for metaso_chat.",
                    "items": { "type": "object" }
                },
                "model": {
                    "type": "string",
                    "description": "Metaso chat model.",
                    "enum": ["fast", "fast_thinking", "ds-r1"],
                    "default": "fast"
                },
                "chat_format": {
                    "type": "string",
                    "description": "Metaso chat output format.",
                    "enum": ["chat_completions", "simple"],
                    "default": "chat_completions"
                },
                "freshness": {
                    "type": "string",
                    "description": "Bocha time filter: noLimit|oneDay|oneWeek|oneMonth|oneYear|YYYY-MM-DD..YYYY-MM-DD",
                    "default": "noLimit"
                },
                "bocha_summary": {
                    "type": "boolean",
                    "description": "Bocha: include AI summaries. Default true.",
                    "default": true
                },
                "include_domains": {
                    "type": "string",
                    "description": "Bocha domain allowlist, pipe-separated (e.g. 'gov.cn|edu.cn')."
                },
                "exclude_domains": {
                    "type": "string",
                    "description": "Bocha domain blocklist, pipe-separated."
                },
                "from_time": {
                    "type": "string",
                    "description": "Anspire start time filter (YYYY-MM-DD HH:MM:SS)."
                },
                "to_time": {
                    "type": "string",
                    "description": "Anspire end time filter (YYYY-MM-DD HH:MM:SS)."
                },
                "with_content": {
                    "type": "boolean",
                    "description": "Jina: include full page content in results. Default false.",
                    "default": false
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<serde_json::Value> {
        if !self.has_any_accounts() {
            anyhow::bail!(
                "No cn search accounts configured. Set tools.web.cn_search or env \
                 METASO_API_KEY / BOCHA_API_KEY / ANSPIRE_API_KEY / JINA_API_KEY."
            );
        }

        let client = self.client()?;
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("search");
        let output = match action {
            "search" => self.exec_search(&client, &args).await?,
            "metaso_search" => self.exec_metaso_search(&client, &args).await?,
            "metaso_chat" => self.exec_metaso_chat(&client, &args).await?,
            "bocha_search" => self.exec_bocha_search(&client, &args).await?,
            "anspire_search" => self.exec_anspire_search(&client, &args, false).await?,
            "anspire_prosearch" => self.exec_anspire_search(&client, &args, true).await?,
            "jina_search" => self.exec_jina_search(&client, &args).await?,
            "jina_site_search" => self.exec_jina_site_search(&client, &args).await?,
            other => anyhow::bail!("unknown action: {other}"),
        };
        Ok(self.attach_warnings(output))
    }
}

fn env_value_with_overrides(
    env_overrides: &HashMap<String, String>,
    key: &str,
) -> Option<String> {
    std::env::var(key).ok().or_else(|| env_overrides.get(key).cloned())
}

fn require_query(a: &serde_json::Value) -> Result<String> {
    let query = a
        .get("query")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if query.is_empty() {
        anyhow::bail!("missing 'query'");
    }
    Ok(query)
}

fn opt_str(a: &serde_json::Value, key: &str) -> Option<String> {
    a.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn usize_arg(a: &serde_json::Value, key: &str, default: usize) -> usize {
    a.get(key).and_then(|v| v.as_u64()).map(|v| v as usize).unwrap_or(default)
}

fn u32_arg(a: &serde_json::Value, key: &str, default: u32) -> u32 {
    a.get(key).and_then(|v| v.as_u64()).map(|v| v as u32).unwrap_or(default)
}

fn bool_arg(a: &serde_json::Value, key: &str, default: bool) -> bool {
    a.get(key).and_then(|v| v.as_bool()).unwrap_or(default)
}

fn parse_results(val: &serde_json::Value) -> Vec<SearchResult> {
    let mut results = Vec::new();
    if let Some(arr) = val.get("data").and_then(|v| v.as_array()) {
        for item in arr {
            if let (Some(title), Some(url)) = (
                item.get("title").and_then(|v| v.as_str()),
                item.get("url").and_then(|v| v.as_str()),
            ) {
                results.push(SearchResult {
                    title: title.to_string(),
                    url: url.to_string(),
                    snippet: item
                        .get("snippet")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("description").and_then(|v| v.as_str()))
                        .or_else(|| item.get("content").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .to_string(),
                    source: item
                        .get("source")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    time: item
                        .get("time")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("publishedTime").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .to_string(),
                });
            }
        }
    }

    if let Some(arr) = val.get("results").and_then(|v| v.as_array()) {
        for item in arr {
            if let (Some(title), Some(url)) = (
                item.get("title").and_then(|v| v.as_str()),
                item.get("url").and_then(|v| v.as_str()),
            ) {
                results.push(SearchResult {
                    title: title.to_string(),
                    url: url.to_string(),
                    snippet: item
                        .get("snippet")
                        .and_then(|v| v.as_str())
                        .or_else(|| item.get("desc").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .to_string(),
                    source: item
                        .get("source")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    time: item
                        .get("time")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                });
            }
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_web_cn_search() {
        let mut cfg = WebCnSearchConfig::default();
        cfg.enabled = true;
        let tool = WebCnSearchTool::from_config_with_env_overrides(&cfg, &HashMap::new()).unwrap();
        assert_eq!(tool.name(), "web_cn_search");
    }

    #[test]
    fn schema_has_action() {
        let mut cfg = WebCnSearchConfig::default();
        cfg.enabled = true;
        let tool = WebCnSearchTool::from_config_with_env_overrides(&cfg, &HashMap::new()).unwrap();
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
    }

    #[test]
    fn warnings_include_missing_keys() {
        let mut cfg = WebCnSearchConfig::default();
        cfg.enabled = true;
        cfg.metaso.enabled = true;
        cfg.bocha.enabled = true;
        cfg.anspire.enabled = true;
        cfg.jina.enabled = true;
        let tool = WebCnSearchTool::from_config_with_env_overrides(&cfg, &HashMap::new()).unwrap();
        let warnings = tool.warnings();
        assert!(warnings.iter().any(|w| w.contains("metaso")));
        assert!(warnings.iter().any(|w| w.contains("bocha")));
        assert!(warnings.iter().any(|w| w.contains("anspire")));
        assert!(warnings.iter().any(|w| w.contains("jina")));
    }

    #[test]
    fn env_override_provides_missing_provider_key() {
        let mut cfg = WebCnSearchConfig::default();
        cfg.enabled = true;
        cfg.metaso.enabled = true;

        let tool = WebCnSearchTool::from_config_with_env_overrides(
            &cfg,
            &HashMap::from([("METASO_API_KEY".to_string(), "env-key".to_string())]),
        )
        .unwrap();

        assert!(tool.has_any_accounts());
        assert!(tool.warnings().iter().all(|w| !w.contains("metaso")));
    }
}
