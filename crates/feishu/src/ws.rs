use std::{collections::HashMap, sync::Arc, time::{Duration, Instant}};

use {
    anyhow::{Context, Result},
    futures::{SinkExt, StreamExt},
    secrecy::ExposeSecret,
    serde::Deserialize,
    tokio::time::MissedTickBehavior,
    tracing::{debug, info, warn},
};

use {
    moltis_channels::{
        ChannelEvent, ChannelEventSink,
        gating::{DmPolicy, GroupPolicy, MentionMode, is_allowed},
        message_log::{MessageLog, MessageLogEntry},
        plugin::{ChannelAttachment, ChannelMessageKind, ChannelMessageMeta, ChannelReplyTarget, ChannelType},
    },
};

use crate::{
    auth::{fetch_bot_open_id, get_access_token},
    state::AccountState,
    ws_frame::{FeishuFrame, FeishuHeader, decode_frame, encode_frame},
};

const LEGACY_WS_ENDPOINT: &str = "wss://open.feishu.cn/open-apis/event/v1/ws";
const DEFAULT_PING_INTERVAL_SECS: u64 = 120;
const WS_BOOTSTRAP_PATH: &str = "/callback/ws/endpoint";
const FRAME_METHOD_CONTROL: i32 = 0;
const FRAME_METHOD_DATA: i32 = 1;

#[derive(Clone)]
struct WsRuntimeConfig {
    connect_url: String,
    service_id: i32,
    ping_interval: Duration,
}

#[derive(Debug, Deserialize)]
struct WsBootstrapResponse {
    code: i64,
    #[serde(default)]
    msg: String,
    data: Option<WsBootstrapData>,
}

#[derive(Debug, Deserialize)]
struct WsBootstrapData {
    #[serde(rename = "URL")]
    url: String,
    #[serde(rename = "ClientConfig")]
    client_config: Option<WsClientConfig>,
}

#[derive(Debug, Default, Deserialize)]
struct WsClientConfig {
    #[serde(rename = "PingInterval", default)]
    ping_interval_secs: u64,
}

#[derive(Default)]
struct EventChunkCache {
    parts: HashMap<String, EventChunk>,
}

struct EventChunk {
    chunks: Vec<Option<Vec<u8>>>,
    updated_at: Instant,
}

impl EventChunkCache {
    fn insert(&mut self, message_id: &str, sum: usize, seq: usize, payload: Vec<u8>) -> Option<Vec<u8>> {
        self.prune();

        if message_id.is_empty() || sum <= 1 {
            return Some(payload);
        }
        if seq >= sum {
            return None;
        }

        let entry = self.parts.entry(message_id.to_string()).or_insert_with(|| EventChunk {
            chunks: vec![None; sum],
            updated_at: Instant::now(),
        });

        if entry.chunks.len() != sum {
            entry.chunks = vec![None; sum];
        }

        entry.updated_at = Instant::now();
        entry.chunks[seq] = Some(payload);

        if entry.chunks.iter().all(Option::is_some) {
            let merged = entry
                .chunks
                .iter_mut()
                .filter_map(Option::take)
                .fold(Vec::new(), |mut acc, part| {
                    acc.extend_from_slice(&part);
                    acc
                });
            self.parts.remove(message_id);
            return Some(merged);
        }

        None
    }

    fn prune(&mut self) {
        let ttl = Duration::from_secs(10);
        self.parts.retain(|_, part| part.updated_at.elapsed() <= ttl);
    }
}

pub async fn run_ws(
    account_id: String,
    mut state: AccountState,
    message_log: Option<Arc<dyn MessageLog>>,
    event_sink: Option<Arc<dyn ChannelEventSink>>,
) -> Result<()> {
    let mut reconnect_delay_secs = 1_u64;

    loop {
        if state.cancel.is_cancelled() {
            state
                .ws_connected
                .store(false, std::sync::atomic::Ordering::Relaxed);
            info!(account_id, "feishu ws cancelled");
            break;
        }

        let runtime = match resolve_ws_runtime_config(&state).await {
            Ok(runtime) => runtime,
            Err(e) => {
                warn!(account_id, error = %e, "feishu ws bootstrap failed");
                tokio::select! {
                    _ = state.cancel.cancelled() => {
                        state
                            .ws_connected
                            .store(false, std::sync::atomic::Ordering::Relaxed);
                        info!(account_id, "feishu ws cancelled");
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_secs(reconnect_delay_secs)) => {}
                }
                reconnect_delay_secs = (reconnect_delay_secs * 2).min(30);
                continue;
            },
        };

        let connect = tokio_tungstenite::connect_async(runtime.connect_url.clone()).await;
        let (mut ws, _) = match connect {
            Ok(ok) => ok,
            Err(e) => {
                warn!(account_id, error = %e, "feishu ws connect failed");
                tokio::select! {
                    _ = state.cancel.cancelled() => {
                        state
                            .ws_connected
                            .store(false, std::sync::atomic::Ordering::Relaxed);
                        info!(account_id, "feishu ws cancelled");
                        break;
                    }
                    _ = tokio::time::sleep(Duration::from_secs(reconnect_delay_secs)) => {}
                }
                reconnect_delay_secs = (reconnect_delay_secs * 2).min(30);
                continue;
            },
        };
        reconnect_delay_secs = 1;
        state
            .ws_connected
            .store(true, std::sync::atomic::Ordering::Relaxed);

        if let Some(token) = get_access_token(&state.http, &state.config, &state.token_cache)
            .await
            .ok()
        {
            state.bot_open_id = fetch_bot_open_id(&state.http, &state.config, &token)
                .await
                .ok()
                .flatten();
        }
        info!(
            account_id,
            service_id = runtime.service_id,
            ping_secs = runtime.ping_interval.as_secs(),
            "feishu ws connected"
        );

        let mut reconnect = true;
        let mut ping_tick = tokio::time::interval(runtime.ping_interval);
        ping_tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let _ = ping_tick.tick().await;

        let mut event_chunks = EventChunkCache::default();

        loop {
            tokio::select! {
                _ = state.cancel.cancelled() => {
                    state
                        .ws_connected
                        .store(false, std::sync::atomic::Ordering::Relaxed);
                    info!(account_id, "feishu ws cancelled");
                    reconnect = false;
                    break;
                }
                _ = ping_tick.tick() => {
                    if let Err(e) = send_ping_frame(&mut ws, runtime.service_id).await {
                        warn!(account_id, error = %e, "feishu ws ping failed");
                        break;
                    }
                }
                msg = ws.next() => {
                    let Some(msg) = msg else { break };
                    match msg {
                        Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                            if let Err(e) = handle_ws_text(&mut ws, &account_id, &state, &text, &message_log, &event_sink).await {
                                warn!(account_id, error = %e, "feishu ws message handling failed");
                            }
                        }
                        Ok(tokio_tungstenite::tungstenite::Message::Binary(bin)) => {
                            if let Err(e) = handle_ws_binary(
                                &mut ws,
                                &account_id,
                                &state,
                                &bin,
                                &message_log,
                                &event_sink,
                                &mut event_chunks,
                            )
                            .await
                            {
                                warn!(account_id, error = %e, "feishu ws binary handling failed");
                            }
                        }
                        Ok(tokio_tungstenite::tungstenite::Message::Ping(payload)) => {
                            let _ = ws.send(tokio_tungstenite::tungstenite::Message::Pong(payload)).await;
                        }
                        Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => {
                            break;
                        }
                        Err(e) => {
                            warn!(account_id, error = %e, "feishu ws stream error");
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        if !reconnect {
            break;
        }
        state
            .ws_connected
            .store(false, std::sync::atomic::Ordering::Relaxed);
        warn!(account_id, "feishu ws disconnected, reconnecting");
        tokio::select! {
            _ = state.cancel.cancelled() => {
                state
                    .ws_connected
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                info!(account_id, "feishu ws cancelled");
                break;
            }
            _ = tokio::time::sleep(Duration::from_secs(reconnect_delay_secs)) => {}
        }
        reconnect_delay_secs = (reconnect_delay_secs * 2).min(30);
    }
    Ok(())
}

async fn resolve_ws_runtime_config(state: &AccountState) -> Result<WsRuntimeConfig> {
    let endpoint = state.config.ws_endpoint.trim();
    if !endpoint.is_empty() && endpoint != LEGACY_WS_ENDPOINT {
        let url = url::Url::parse(endpoint).context("invalid feishu ws_endpoint")?;
        let service_id = parse_service_id(url.as_ref());
        return Ok(WsRuntimeConfig {
            connect_url: endpoint.to_string(),
            service_id,
            ping_interval: Duration::from_secs(DEFAULT_PING_INTERVAL_SECS),
        });
    }

    let bootstrap_url = format!(
        "{}{}",
        state.config.base_url.trim_end_matches('/'),
        WS_BOOTSTRAP_PATH
    );

    let body = serde_json::json!({
        "AppID": state.config.app_id.expose_secret(),
        "AppSecret": state.config.app_secret.expose_secret(),
    });

    let resp = state
        .http
        .post(&bootstrap_url)
        .header("locale", "zh")
        .json(&body)
        .send()
        .await
        .with_context(|| format!("bootstrap request failed: {bootstrap_url}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        anyhow::bail!("bootstrap HTTP {status}: {text}");
    }

    let parsed: WsBootstrapResponse = resp.json().await.context("invalid bootstrap response")?;
    if parsed.code != 0 {
        anyhow::bail!("bootstrap code {}: {}", parsed.code, parsed.msg);
    }

    let data = parsed
        .data
        .ok_or_else(|| anyhow::anyhow!("bootstrap response missing data"))?;
    let connect_url = data.url.trim().to_string();
    if connect_url.is_empty() {
        anyhow::bail!("bootstrap response returned empty URL");
    }

    let parsed_url = url::Url::parse(&connect_url).context("invalid bootstrap URL")?;
    let service_id = parse_service_id(parsed_url.as_ref());
    let ping_interval_secs = data
        .client_config
        .as_ref()
        .map(|c| c.ping_interval_secs)
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_PING_INTERVAL_SECS);

    Ok(WsRuntimeConfig {
        connect_url,
        service_id,
        ping_interval: Duration::from_secs(ping_interval_secs),
    })
}

fn parse_service_id(connect_url: &str) -> i32 {
    url::Url::parse(connect_url)
        .ok()
        .and_then(|url| {
            url.query_pairs()
                .find_map(|(k, v)| if k == "service_id" { Some(v.into_owned()) } else { None })
        })
        .and_then(|v| v.parse::<i32>().ok())
        .unwrap_or_default()
}

async fn send_ping_frame(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    service_id: i32,
) -> Result<()> {
    if service_id <= 0 {
        return Ok(());
    }

    let frame = FeishuFrame {
        seq_id: 0,
        log_id: 0,
        service: service_id,
        method: FRAME_METHOD_CONTROL,
        headers: vec![FeishuHeader {
            key: "type".to_string(),
            value: "ping".to_string(),
        }],
        payload_encoding: String::new(),
        payload_type: String::new(),
        payload: Vec::new(),
        log_id_new: String::new(),
    };

    ws.send(tokio_tungstenite::tungstenite::Message::Binary(
        encode_frame(&frame).into(),
    ))
    .await?;
    Ok(())
}

async fn handle_ws_binary(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    account_id: &str,
    state: &AccountState,
    bin: &[u8],
    message_log: &Option<Arc<dyn MessageLog>>,
    event_sink: &Option<Arc<dyn ChannelEventSink>>,
    chunks: &mut EventChunkCache,
) -> Result<()> {
    if let Ok(text) = std::str::from_utf8(bin) {
        if text.trim_start().starts_with('{') {
            return handle_ws_text(ws, account_id, state, text, message_log, event_sink).await;
        }
    }

    let frame = decode_frame(bin).context("decode frame")?;

    match frame.method {
        FRAME_METHOD_CONTROL => {
            let msg_type = header_value(&frame.headers, "type").unwrap_or_default();
            if msg_type == "pong" {
                debug!(account_id, "feishu ws received pong frame");
            }
        }
        FRAME_METHOD_DATA => {
            let msg_type = header_value(&frame.headers, "type").unwrap_or_default();
            if msg_type != "event" {
                return Ok(());
            }

            let message_id = header_value(&frame.headers, "message_id").unwrap_or_default();
            let sum = header_value(&frame.headers, "sum")
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|v| *v > 0)
                .unwrap_or(1);
            let seq = header_value(&frame.headers, "seq")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);

            let merged = chunks.insert(message_id, sum, seq, frame.payload.clone());
            let Some(payload) = merged else {
                return Ok(());
            };

            let code = match handle_event_payload(account_id, state, &payload, message_log, event_sink).await {
                Ok(()) => 200,
                Err(e) => {
                    warn!(account_id, error = %e, "failed to process feishu event payload");
                    500
                }
            };

            send_event_ack(ws, &frame, code).await?;
        }
        _ => {}
    }

    Ok(())
}

fn header_value<'a>(headers: &'a [FeishuHeader], key: &str) -> Option<&'a str> {
    headers
        .iter()
        .find_map(|h| if h.key == key { Some(h.value.as_str()) } else { None })
}

async fn send_event_ack(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    frame: &FeishuFrame,
    code: u16,
) -> Result<()> {
    let mut headers = frame.headers.clone();
    headers.push(FeishuHeader {
        key: "biz_rt".to_string(),
        value: "0".to_string(),
    });

    let payload = serde_json::json!({ "code": code }).to_string().into_bytes();
    let ack = FeishuFrame {
        seq_id: frame.seq_id,
        log_id: frame.log_id,
        service: frame.service,
        method: frame.method,
        headers,
        payload_encoding: frame.payload_encoding.clone(),
        payload_type: frame.payload_type.clone(),
        payload,
        log_id_new: frame.log_id_new.clone(),
    };

    ws.send(tokio_tungstenite::tungstenite::Message::Binary(
        encode_frame(&ack).into(),
    ))
    .await?;
    Ok(())
}

async fn handle_event_payload(
    account_id: &str,
    state: &AccountState,
    payload: &[u8],
    message_log: &Option<Arc<dyn MessageLog>>,
    event_sink: &Option<Arc<dyn ChannelEventSink>>,
) -> Result<()> {
    let raw = std::str::from_utf8(payload).context("event payload is not UTF-8")?;
    let value: serde_json::Value = serde_json::from_str(raw).context("invalid event JSON")?;
    handle_event_value(account_id, state, value, message_log, event_sink).await
}

async fn handle_ws_text(
    ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    account_id: &str,
    state: &AccountState,
    text: &str,
    message_log: &Option<Arc<dyn MessageLog>>,
    event_sink: &Option<Arc<dyn ChannelEventSink>>,
) -> Result<()> {
    let value: serde_json::Value = serde_json::from_str(text).unwrap_or_default();

    if value.get("type").and_then(|v| v.as_str()) == Some("ping") {
        let _ = ws
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::json!({"type": "pong"}).to_string().into(),
            ))
            .await;
        return Ok(());
    }

    if value.get("type").and_then(|v| v.as_str()) == Some("event_callback") {
        if let Some(uuid) = value.get("uuid").and_then(|v| v.as_str()) {
            let ack = serde_json::json!({
                "type": "event_callback",
                "uuid": uuid,
                "status": 0,
            });
            let _ = ws
                .send(tokio_tungstenite::tungstenite::Message::Text(ack.to_string().into()))
                .await;
        }
    }

    handle_event_value(account_id, state, value, message_log, event_sink).await
}

async fn handle_event_value(
    account_id: &str,
    state: &AccountState,
    value: serde_json::Value,
    message_log: &Option<Arc<dyn MessageLog>>,
    event_sink: &Option<Arc<dyn ChannelEventSink>>,
) -> Result<()> {
    let header = value.get("header").cloned().unwrap_or_default();
    let event_type = header.get("event_type").and_then(|v| v.as_str()).unwrap_or("");
    if event_type != "im.message.receive_v1" {
        return Ok(());
    }

    let event = value.get("event").cloned().unwrap_or_default();
    let message = event.get("message").cloned().unwrap_or_default();
    let chat_id = message.get("chat_id").and_then(|v| v.as_str()).unwrap_or("");
    let message_id = message
        .get("message_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let chat_type = message
        .get("chat_type")
        .and_then(|v| v.as_str())
        .unwrap_or("p2p");
    let message_type = message
        .get("message_type")
        .and_then(|v| v.as_str())
        .unwrap_or("text");
    let content_raw = message
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let content_json: serde_json::Value = serde_json::from_str(content_raw).unwrap_or_default();

    let sender = event.get("sender").cloned().unwrap_or_default();
    let sender_id = sender.get("sender_id").cloned().unwrap_or_default();
    let peer_id = sender_id
        .get("open_id")
        .and_then(|v| v.as_str())
        .or_else(|| sender_id.get("user_id").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string();

    let is_group = chat_type != "p2p";
    let policy_allowed = if is_group {
        match state.config.group_policy {
            GroupPolicy::Open => true,
            GroupPolicy::Allowlist => is_allowed(chat_id, &state.config.group_allowlist),
            GroupPolicy::Disabled => false,
        }
    } else {
        match state.config.dm_policy {
            DmPolicy::Open => true,
            DmPolicy::Allowlist => is_allowed(&peer_id, &state.config.allowlist),
            DmPolicy::Disabled => false,
        }
    };

    let mentions = event
        .get("mentions")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mention_allowed = if is_group {
        match state.config.mention_mode {
            MentionMode::Always => true,
            MentionMode::Mention => mentions_target_bot(&mentions, state.bot_open_id.as_deref()),
            MentionMode::None => false,
        }
    } else {
        true
    };

    let access_granted = policy_allowed && mention_allowed;

    if let Some(log) = message_log {
        let entry = MessageLogEntry {
            id: 0,
            account_id: account_id.to_string(),
            channel_type: "feishu".into(),
            peer_id: peer_id.clone(),
            username: None,
            sender_name: None,
            chat_id: chat_id.to_string(),
            chat_type: chat_type.to_string(),
            body: content_json
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            access_granted,
            created_at: unix_now(),
        };
        let _ = log.log(entry).await;
    }

    if let Some(sink) = event_sink {
        sink.emit(ChannelEvent::InboundMessage {
            channel_type: ChannelType::Feishu,
            account_id: account_id.to_string(),
            peer_id: peer_id.clone(),
            username: None,
            sender_name: None,
            message_count: None,
            access_granted,
        })
        .await;
    }

    if !access_granted {
        return Ok(());
    }

    let text = content_json
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let normalized_text = normalize_command_text(text);
    if let Some(cmd) = normalized_text.strip_prefix('/').map(str::trim)
        && !cmd.is_empty()
        && let Some(sink) = event_sink
    {
        let cmd_reply_to = ChannelReplyTarget {
            channel_type: ChannelType::Feishu,
            account_id: account_id.to_string(),
            chat_id: chat_id.to_string(),
            message_id: message_id.clone(),
        };
        let response = match sink.dispatch_command(cmd, cmd_reply_to).await {
            Ok(resp) => resp,
            Err(e) => format!("Error: {e}"),
        };
        if let Err(e) = send_text_message(state, chat_id, &response).await {
            warn!(account_id, error = %e, "failed to send feishu command response");
        }
        return Ok(());
    }

    let reply_to = ChannelReplyTarget {
        channel_type: ChannelType::Feishu,
        account_id: account_id.to_string(),
        chat_id: chat_id.to_string(),
        message_id: message_id.clone(),
    };

    let meta = ChannelMessageMeta {
        channel_type: ChannelType::Feishu,
        sender_name: None,
        username: None,
        message_kind: Some(map_message_kind(message_type)),
        model: state.config.model.clone(),
        audio_filename: None,
    };

    if message_type == "audio" {
        if let Err(e) = send_text_message(
            state,
            chat_id,
            "Voice messages are not supported in this fork. Please send text or upload a file instead.",
        )
        .await
        {
            warn!(account_id, error = %e, "failed to send voice unsupported hint");
        }
        return Ok(());
    }

    let mut attachments = Vec::new();
    if message_type == "image" {
        if let Some(key) = content_json.get("image_key").and_then(|v| v.as_str()) {
            if let Ok(att) = download_message_resource(
                state,
                message_id.as_deref(),
                key,
                "image",
                None,
            )
            .await
            {
                attachments.push(att);
            }
        }
    }
    if message_type == "file" {
        if let Some(key) = content_json.get("file_key").and_then(|v| v.as_str()) {
            let file_name = content_json
                .get("file_name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|v| !v.is_empty());
            if let Ok(att) = download_message_resource(
                state,
                message_id.as_deref(),
                key,
                "file",
                file_name,
            )
            .await
            {
                attachments.push(att);
            }
        }
    }

    if let Some(sink) = event_sink {
        if attachments.is_empty() {
            sink.dispatch_to_chat(text, reply_to, meta).await;
        } else {
            sink.dispatch_to_chat_with_attachments(text, attachments, reply_to, meta)
                .await;
        }
    }

    Ok(())
}

async fn download_message_resource(
    state: &AccountState,
    message_id: Option<&str>,
    file_key: &str,
    kind: &str,
    preferred_name: Option<&str>,
) -> Result<ChannelAttachment> {
    let message_id = message_id.ok_or_else(|| anyhow::anyhow!("missing message_id"))?;
    let token = get_access_token(&state.http, &state.config, &state.token_cache).await?;
    let url = format!(
        "{}/open-apis/im/v1/messages/{message_id}/resources/{file_key}",
        state.config.base_url.trim_end_matches('/')
    );
    // Some Feishu audio events expose a key that can only be fetched with type=file.
    let kinds: &[&str] = if kind == "audio" {
        &["audio", "file"]
    } else {
        &[kind]
    };
    let mut last_error: Option<anyhow::Error> = None;

    for resource_type in kinds {
        let resp = state
            .http
            .get(&url)
            .bearer_auth(token.expose_secret())
            .query(&[("type", *resource_type)])
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            last_error = Some(anyhow::anyhow!(
                "download resource HTTP {status} (type={resource_type}): {body}"
            ));
            continue;
        }
        let media_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_string();
        let header_name = resp
            .headers()
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|v| v.to_str().ok())
            .and_then(content_disposition_filename);
        let data = resp.bytes().await?.to_vec();
        let original_name = preferred_name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .or(header_name);
        return Ok(ChannelAttachment {
            media_type,
            original_name,
            data,
        });
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("download resource failed")))
}

fn content_disposition_filename(value: &str) -> Option<String> {
    for part in value.split(';').map(str::trim) {
        if let Some(raw) = part.strip_prefix("filename*=") {
            let cleaned = raw.trim().trim_matches('"');
            if let Some((_, encoded)) = cleaned.split_once("''") {
                if let Ok(decoded) = urlencoding::decode(encoded) {
                    let name = decoded.trim();
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
            if !cleaned.is_empty() {
                return Some(cleaned.to_string());
            }
        }
        if let Some(raw) = part.strip_prefix("filename=") {
            let cleaned = raw.trim().trim_matches('"');
            if !cleaned.is_empty() {
                return Some(cleaned.to_string());
            }
        }
    }
    None
}

fn map_message_kind(message_type: &str) -> ChannelMessageKind {
    match message_type {
        "text" => ChannelMessageKind::Text,
        "image" => ChannelMessageKind::Photo,
        "file" => ChannelMessageKind::Document,
        "audio" => ChannelMessageKind::Audio,
        "video" => ChannelMessageKind::Video,
        _ => ChannelMessageKind::Other,
    }
}

fn mentions_target_bot(mentions: &[serde_json::Value], bot_open_id: Option<&str>) -> bool {
    if mentions.is_empty() {
        return false;
    }
    let Some(bot_open_id) = bot_open_id else {
        return false;
    };
    mentions.iter().any(|m| {
        let id = &m["id"];
        id.get("open_id").and_then(|v| v.as_str()) == Some(bot_open_id)
            || id.get("user_id").and_then(|v| v.as_str()) == Some(bot_open_id)
    })
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn normalize_command_text(text: &str) -> &str {
    let mut s = text.trim_start();
    loop {
        let before = s;
        s = strip_leading_at_tag(s);
        s = strip_leading_mention_token(s);
        s = s.trim_start();
        if s == before {
            break;
        }
    }
    s
}

fn strip_leading_at_tag(text: &str) -> &str {
    if !text.starts_with("<at") {
        return text;
    }
    if let Some(end) = text.find("</at>") {
        return &text[end + "</at>".len()..];
    }
    text
}

fn strip_leading_mention_token(text: &str) -> &str {
    let Some(stripped) = text.strip_prefix('@') else {
        return text;
    };
    let mut split_at = 0;
    for (idx, ch) in stripped.char_indices() {
        if ch.is_whitespace() {
            split_at = idx;
            break;
        }
    }
    if split_at == 0 {
        return "";
    }
    &stripped[split_at..]
}

async fn send_text_message(state: &AccountState, chat_id: &str, text: &str) -> Result<()> {
    let token = get_access_token(&state.http, &state.config, &state.token_cache).await?;
    let url = format!(
        "{}/open-apis/im/v1/messages?receive_id_type=chat_id",
        state.config.base_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "receive_id": chat_id,
        "msg_type": "text",
        "content": serde_json::json!({ "text": text }).to_string(),
    });
    let resp = state
        .http
        .post(url)
        .bearer_auth(token.expose_secret())
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("send command response HTTP {status}: {body}");
    }
    Ok(())
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_service_id_works() {
        let url = "wss://open.feishu.cn/ws?device_id=d&service_id=123";
        assert_eq!(parse_service_id(url), 123);
    }

    #[test]
    fn parse_service_id_missing_defaults_zero() {
        assert_eq!(parse_service_id("wss://open.feishu.cn/ws"), 0);
    }

    #[test]
    fn chunk_cache_merges_all_parts() {
        let mut cache = EventChunkCache::default();
        assert!(cache.insert("mid", 2, 0, b"hel".to_vec()).is_none());
        let merged = cache.insert("mid", 2, 1, b"lo".to_vec()).unwrap();
        assert_eq!(merged, b"hello");
    }

    #[test]
    fn chunk_cache_rejects_invalid_seq() {
        let mut cache = EventChunkCache::default();
        assert!(cache.insert("mid", 2, 2, b"oops".to_vec()).is_none());
    }

    #[test]
    fn normalize_command_text_handles_mentions() {
        assert_eq!(normalize_command_text("@_user_1 /agent"), "/agent");
        assert_eq!(
            normalize_command_text("<at user_id=\"ou_x\">Bot</at> /agent 2"),
            "/agent 2"
        );
        assert_eq!(normalize_command_text("   /model"), "/model");
    }

    #[test]
    fn content_disposition_filename_parses_filename_star() {
        let value = "attachment; filename*=UTF-8''%E6%B5%8B%E8%AF%95.docx";
        assert_eq!(
            content_disposition_filename(value).as_deref(),
            Some("测试.docx")
        );
    }

    #[test]
    fn content_disposition_filename_parses_plain_filename() {
        let value = "attachment; filename=\"report-final.docx\"";
        assert_eq!(
            content_disposition_filename(value).as_deref(),
            Some("report-final.docx")
        );
    }

}
