use std::sync::Arc;

use {
    async_trait::async_trait,
    moltis_tools::image_cache::ImageBuilder,
    tracing::{debug, error, info, warn},
};

use {
    moltis_channels::{
        ChannelAttachment, ChannelEvent, ChannelEventSink, ChannelMessageMeta, ChannelReplyTarget,
        ChannelType,
        Error as ChannelError, Result as ChannelResult,
    },
    moltis_sessions::metadata::SqliteSessionMetadata,
};

use crate::{
    broadcast::{BroadcastOpts, broadcast},
    state::GatewayState,
};

async fn channel_account_config(
    state: &GatewayState,
    reply_to: &ChannelReplyTarget,
) -> Option<serde_json::Value> {
    state
        .services
        .channel
        .status()
        .await
        .ok()
        .and_then(|v| v.get("channels").and_then(|c| c.as_array()).cloned())
        .and_then(|channels| {
            channels
                .into_iter()
                .find(|ch| {
                    ch.get("type").and_then(|v| v.as_str())
                        == Some(reply_to.channel_type.as_str())
                        && ch.get("account_id").and_then(|v| v.as_str())
                            == Some(reply_to.account_id.as_str())
                })
                .and_then(|ch| ch.get("config").cloned())
        })
}

async fn channel_config_string(
    state: &GatewayState,
    reply_to: &ChannelReplyTarget,
    key: &str,
) -> Option<String> {
    channel_account_config(state, reply_to)
        .await
        .and_then(|cfg| cfg.get(key).and_then(|v| v.as_str()).map(str::to_string))
        .filter(|value| !value.trim().is_empty())
}

async fn channel_config_bool(
    state: &GatewayState,
    reply_to: &ChannelReplyTarget,
    key: &str,
) -> Option<bool> {
    channel_account_config(state, reply_to)
        .await
        .and_then(|cfg| cfg.get(key).and_then(|v| v.as_bool()))
}

async fn channel_config_u64(
    state: &GatewayState,
    reply_to: &ChannelReplyTarget,
    key: &str,
) -> Option<u64> {
    channel_account_config(state, reply_to)
        .await
        .and_then(|cfg| {
            cfg.get(key).and_then(|v| {
                v.as_u64().or_else(|| {
                    v.as_str()
                        .and_then(|raw| raw.trim().parse::<u64>().ok())
                })
            })
        })
}

/// Default (deterministic) session key for a channel chat.
fn default_channel_session_key(target: &ChannelReplyTarget) -> String {
    format!(
        "{}:{}:{}",
        target.channel_type, target.account_id, target.chat_id
    )
}

/// Resolve the active session key for a channel chat.
/// Uses the forward mapping table if an override exists, otherwise falls back
/// to the deterministic key.
async fn resolve_channel_session(
    target: &ChannelReplyTarget,
    metadata: &SqliteSessionMetadata,
) -> String {
    if let Some(key) = metadata
        .get_active_session(
            target.channel_type.as_str(),
            &target.account_id,
            &target.chat_id,
        )
        .await
    {
        return key;
    }
    default_channel_session_key(target)
}

async fn resolve_inbound_channel_session(
    state: &GatewayState,
    target: &ChannelReplyTarget,
    metadata: &SqliteSessionMetadata,
) -> String {
    let session_key = resolve_channel_session(target, metadata).await;
    let _ = auto_archive_stale_channel_sessions(state, metadata, target, &session_key).await;
    session_key
}

fn slash_command_name(text: &str) -> Option<&str> {
    let rest = text.trim_start().strip_prefix('/')?;
    let cmd = rest.split_whitespace().next().unwrap_or("");
    if cmd.is_empty() {
        None
    } else {
        Some(cmd)
    }
}

fn is_channel_control_command_name(cmd: &str) -> bool {
    matches!(
        cmd,
        "new"
            | "clear"
            | "compact"
            | "context"
            | "model"
            | "sandbox"
            | "sessions"
            | "agent"
            | "handoff"
            | "help"
            | "sh"
    )
}

fn rewrite_for_shell_mode(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(cmd) = slash_command_name(trimmed)
        && is_channel_control_command_name(cmd)
    {
        return None;
    }

    Some(format!("/sh {trimmed}"))
}

fn is_image_media_type(media_type: &str) -> bool {
    media_type
        .trim()
        .to_ascii_lowercase()
        .starts_with("image/")
}

fn attachment_placeholder_text(has_images: bool, has_non_images: bool) -> &'static str {
    match (has_images, has_non_images) {
        (true, true) => "[Image + Attachment]",
        (true, false) => "[Image]",
        (false, true) => "[Attachment]",
        (false, false) => "",
    }
}

async fn persist_non_image_attachments(
    state: &GatewayState,
    session_key: &str,
    reply_to: &ChannelReplyTarget,
    attachments: &[ChannelAttachment],
) -> Vec<String> {
    let Some(store) = state.services.attachment_store.clone() else {
        return Vec::new();
    };
    let mut saved_paths = Vec::new();
    for attachment in attachments {
        if is_image_media_type(&attachment.media_type) {
            continue;
        }
        let saved = store
            .save_channel_attachment(crate::attachment_store::SaveChannelAttachment {
                session_key,
                channel_type: reply_to.channel_type.as_str(),
                account_id: &reply_to.account_id,
                chat_id: &reply_to.chat_id,
                message_id: reply_to.message_id.as_deref(),
                media_type: &attachment.media_type,
                original_name: attachment.original_name.as_deref(),
                data: &attachment.data,
            })
            .await;
        match saved {
            Ok(saved) => {
                saved_paths.push(format!(
                    "{} -> {} ({}, {} bytes)",
                    saved.original_name,
                    saved.absolute_path.display(),
                    saved.media_type,
                    saved.size_bytes
                ));
            }
            Err(error) => {
                warn!(
                    session_key,
                    media_type = %attachment.media_type,
                    error = %error,
                    "failed to persist non-image attachment"
                );
            }
        }
    }
    saved_paths
}

fn normalize_selector(input: &str) -> String {
    input.trim().to_ascii_lowercase()
}

fn resolve_agent_selector<'a>(
    agents: &'a [crate::agent_persona::AgentPersona],
    selector: &str,
) -> ChannelResult<&'a crate::agent_persona::AgentPersona> {
    let trimmed = selector.trim();
    if trimmed.is_empty() {
        return Err(ChannelError::invalid_input("missing agent id"));
    }

    let normalized = normalize_selector(trimmed);

    let mut by_id = agents.iter().filter(|agent| agent.id == normalized);
    if let Some(found) = by_id.next() {
        return Ok(found);
    }

    Err(ChannelError::invalid_input(format!(
        "unknown agent id: '{trimmed}'"
    )))
}

const HANDOFF_NAMESPACE: &str = "handoff";
const HANDOFF_PENDING_KEY: &str = "pending";
const DEFAULT_AUTO_ARCHIVE_DAYS: u64 = 30;

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum HandoffMode {
    SameSession,
    NewSession,
    ForkSession,
}

impl HandoffMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::SameSession => "same_session",
            Self::NewSession => "new_session",
            Self::ForkSession => "fork_session",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct HandoffPacket {
    version: u8,
    mode: HandoffMode,
    from_agent_id: String,
    to_agent_id: String,
    source_session_key: String,
    target_session_key: String,
    #[serde(default)]
    note: String,
    created_at_ms: u64,
}

fn parse_handoff_mode(value: &str) -> Option<HandoffMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "same_session" | "same" => Some(HandoffMode::SameSession),
        "new_session" | "new" => Some(HandoffMode::NewSession),
        "fork_session" | "fork" => Some(HandoffMode::ForkSession),
        _ => None,
    }
}

fn parse_handoff_args(args: &str) -> ChannelResult<(String, HandoffMode, String)> {
    let mut tokens = args.split_whitespace().peekable();
    let selector = tokens
        .next()
        .ok_or_else(|| ChannelError::invalid_input("usage: /handoff <agent_id> [note]"))?;

    let mut mode = HandoffMode::SameSession;
    let mut mode_explicit = false;
    let mut note_parts = Vec::new();

    while let Some(token) = tokens.next() {
        if let Some(value) = token.strip_prefix("--mode=") {
            let parsed = parse_handoff_mode(value).ok_or_else(|| {
                ChannelError::invalid_input("invalid mode; use same_session|new_session|fork_session")
            })?;
            mode = parsed;
            mode_explicit = true;
            continue;
        }
        if token == "--mode" {
            let value = tokens.next().ok_or_else(|| {
                ChannelError::invalid_input("missing mode value after --mode")
            })?;
            let parsed = parse_handoff_mode(value).ok_or_else(|| {
                ChannelError::invalid_input("invalid mode; use same_session|new_session|fork_session")
            })?;
            mode = parsed;
            mode_explicit = true;
            continue;
        }
        if token.starts_with("--") {
            return Err(ChannelError::invalid_input(format!(
                "unknown flag: {token}"
            )));
        }
        if !mode_explicit
            && let Some(parsed) = parse_handoff_mode(token)
        {
            mode = parsed;
            mode_explicit = true;
            continue;
        }
        note_parts.push(token);
    }

    Ok((
        selector.to_string(),
        mode,
        note_parts.join(" ").trim().to_string(),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionsCommand {
    List,
    Switch(usize),
    Archive(usize),
    Unarchive(usize),
}

fn parse_sessions_command_args(args: &str) -> ChannelResult<SessionsCommand> {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return Ok(SessionsCommand::List);
    }

    let mut parts = trimmed.split_whitespace();
    let head = parts.next().unwrap_or_default();
    match head {
        "archive" | "unarchive" => {
            let n_str = parts.next().ok_or_else(|| {
                ChannelError::invalid_input(format!("usage: /sessions {head} <number>"))
            })?;
            if parts.next().is_some() {
                return Err(ChannelError::invalid_input(format!(
                    "usage: /sessions {head} <number>"
                )));
            }
            let n = n_str.parse::<usize>().map_err(|_| {
                ChannelError::invalid_input(format!("usage: /sessions {head} <number>"))
            })?;
            if n == 0 {
                return Err(ChannelError::invalid_input("session number must be >= 1"));
            }
            if head == "archive" {
                Ok(SessionsCommand::Archive(n))
            } else {
                Ok(SessionsCommand::Unarchive(n))
            }
        }
        n_str => {
            if parts.next().is_some() {
                return Err(ChannelError::invalid_input(
                    "usage: /sessions [number]|archive <number>|unarchive <number>",
                ));
            }
            let n = n_str
                .parse::<usize>()
                .map_err(|_| ChannelError::invalid_input("usage: /sessions <number>"))?;
            if n == 0 {
                return Err(ChannelError::invalid_input("session number must be >= 1"));
            }
            Ok(SessionsCommand::Switch(n))
        }
    }
}

async fn current_agent_id_for_session(
    state: &GatewayState,
    session_metadata: &SqliteSessionMetadata,
    session_key: &str,
) -> String {
    if let Some(agent_id) = session_metadata
        .get(session_key)
        .await
        .and_then(|entry| entry.agent_id)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        return agent_id;
    }
    if let Some(ref store) = state.services.agent_persona_store {
        return store
            .default_id()
            .await
            .unwrap_or_else(|_| "main".to_string());
    }
    "main".to_string()
}

async fn auto_archive_stale_channel_sessions(
    state: &GatewayState,
    session_metadata: &SqliteSessionMetadata,
    reply_to: &ChannelReplyTarget,
    current_session_key: &str,
) -> usize {
    let auto_archive_days =
        channel_config_u64(state, reply_to, "session_auto_archive_days")
            .await
            .unwrap_or(DEFAULT_AUTO_ARCHIVE_DAYS);
    if auto_archive_days == 0 {
        return 0;
    }

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let cutoff_ms = now_ms.saturating_sub(auto_archive_days.saturating_mul(86_400_000));

    let sessions = session_metadata
        .list_channel_sessions(
            reply_to.channel_type.as_str(),
            &reply_to.account_id,
            &reply_to.chat_id,
        )
        .await;
    let mut archived_count = 0;
    for session in sessions {
        if session.key == current_session_key || session.archived || session.updated_at >= cutoff_ms
        {
            continue;
        }
        if session_metadata.set_archived(&session.key, true).await.is_ok() {
            archived_count += 1;
        }
    }
    archived_count
}

async fn maybe_apply_handoff_context(
    state: &GatewayState,
    session_key: &str,
    message_text: String,
) -> String {
    let Some(ref state_store) = state.services.session_state_store else {
        return message_text;
    };
    let Ok(raw) = state_store
        .get(session_key, HANDOFF_NAMESPACE, HANDOFF_PENDING_KEY)
        .await
    else {
        return message_text;
    };
    let Some(raw) = raw else {
        return message_text;
    };

    let packet: HandoffPacket = match serde_json::from_str(&raw) {
        Ok(packet) => packet,
        Err(_) => {
            let _ = state_store
                .delete(session_key, HANDOFF_NAMESPACE, HANDOFF_PENDING_KEY)
                .await;
            return message_text;
        },
    };

    let current_agent = if let Some(ref meta) = state.services.session_metadata {
        current_agent_id_for_session(state, meta, session_key).await
    } else {
        "main".to_string()
    };
    if packet.to_agent_id != current_agent {
        return message_text;
    }

    let _ = state_store
        .delete(session_key, HANDOFF_NAMESPACE, HANDOFF_PENDING_KEY)
        .await;

    let mut prefix = vec![
        "[Internal Handoff Context]".to_string(),
        format!("from_agent: {}", packet.from_agent_id),
        format!("to_agent: {}", packet.to_agent_id),
        format!("mode: {}", packet.mode.as_str()),
    ];
    if !packet.note.trim().is_empty() {
        prefix.push(format!("note: {}", packet.note.trim()));
    }
    prefix.push(
        "Take over seamlessly. Do not mention this internal handoff metadata unless the user asks."
            .to_string(),
    );

    format!(
        "{}\n\nUser message:\n{}",
        prefix.join("\n"),
        message_text
    )
}

/// Broadcasts channel events over the gateway WebSocket.
///
/// Uses a deferred `OnceCell` reference so the sink can be created before
/// `GatewayState` exists (same pattern as cron callbacks).
pub struct GatewayChannelEventSink {
    state: Arc<tokio::sync::OnceCell<Arc<GatewayState>>>,
}

impl GatewayChannelEventSink {
    pub fn new(state: Arc<tokio::sync::OnceCell<Arc<GatewayState>>>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl ChannelEventSink for GatewayChannelEventSink {
    async fn emit(&self, event: ChannelEvent) {
        if let Some(state) = self.state.get() {
            let mut payload = match serde_json::to_value(&event) {
                Ok(v) => v,
                Err(e) => {
                    warn!("failed to serialize channel event: {e}");
                    return;
                },
            };

            // Render QR data as an SVG so the frontend can display it directly.
            #[cfg(feature = "whatsapp")]
            if let ChannelEvent::PairingQrCode { ref qr_data, .. } = event
                && let Ok(code) = qrcode::QrCode::new(qr_data)
            {
                let svg = code
                    .render::<qrcode::render::svg::Color>()
                    .min_dimensions(200, 200)
                    .quiet_zone(true)
                    .build();
                if let serde_json::Value::Object(ref mut map) = payload {
                    map.insert("qr_svg".into(), serde_json::Value::String(svg));
                }
            }

            broadcast(state, "channel", payload, BroadcastOpts {
                drop_if_slow: true,
                ..Default::default()
            })
            .await;
        }
    }

    async fn dispatch_to_chat(
        &self,
        text: &str,
        reply_to: ChannelReplyTarget,
        meta: ChannelMessageMeta,
    ) {
        if let Some(state) = self.state.get() {
            let session_key = if let Some(ref sm) = state.services.session_metadata {
                resolve_inbound_channel_session(state, &reply_to, sm).await
            } else {
                default_channel_session_key(&reply_to)
            };
            let effective_text = if state.is_channel_command_mode_enabled(&session_key).await {
                rewrite_for_shell_mode(text).unwrap_or_else(|| text.to_string())
            } else {
                text.to_string()
            };
            let effective_text = if effective_text.starts_with("/sh ") {
                effective_text
            } else {
                maybe_apply_handoff_context(state, &session_key, effective_text).await
            };

            // Broadcast a "chat" event so the web UI shows the user message
            // in real-time (like typing from the UI).
            // Include messageIndex so the client can deduplicate against history.
            let msg_index = if let Some(ref store) = state.services.session_store {
                store.count(&session_key).await.unwrap_or(0)
            } else {
                0
            };
            let payload = serde_json::json!({
                "state": "channel_user",
                "text": text,
                "channel": &meta,
                "sessionKey": &session_key,
                "messageIndex": msg_index,
            });
            broadcast(state, "chat", payload, BroadcastOpts {
                drop_if_slow: true,
                ..Default::default()
            })
            .await;

            // Persist channel binding so web UI messages on this session
            // can be echoed back to the channel.
            if let Ok(binding_json) = serde_json::to_string(&reply_to)
                && let Some(ref session_meta) = state.services.session_metadata
            {
                // Ensure the session row exists and label it on first use.
                // `set_channel_binding` is an UPDATE, so the row must exist
                // before we can set the binding column.
                let entry = session_meta.get(&session_key).await;
                if entry.as_ref().is_none_or(|e| e.channel_binding.is_none()) {
                    let existing = session_meta
                        .list_channel_sessions(
                            reply_to.channel_type.as_str(),
                            &reply_to.account_id,
                            &reply_to.chat_id,
                        )
                        .await;
                    let n = existing.len() + 1;
                    let _ = session_meta
                        .upsert(
                            &session_key,
                            Some(format!("{} {n}", reply_to.channel_type.display_name())),
                        )
                        .await;
                }
                session_meta
                    .set_channel_binding(&session_key, Some(binding_json))
                    .await;
            if let Some(entry) = session_meta.get(&session_key).await
                && entry
                    .agent_id
                    .as_deref()
                    .map(str::trim)
                    .is_none_or(|value| value.is_empty())
            {
                let channel_agent = channel_config_string(state, &reply_to, "agent_id").await;
                let default_agent = if let Some(agent_id) = channel_agent {
                    agent_id
                } else if let Some(ref store) = state.services.agent_persona_store {
                    store
                        .default_id()
                        .await
                        .unwrap_or_else(|_| "main".to_string())
                } else {
                    "main".to_string()
                };
                let _ = session_meta
                    .set_agent_id(&session_key, Some(&default_agent))
                    .await;
            }
            }

            let chat = state.chat().await;
            let mut params = serde_json::json!({
                "text": effective_text,
                "channel": &meta,
                "_session_key": &session_key,
                // Defer reply-target registration until chat.send() actually
                // starts executing this message (after semaphore acquire).
                "_channel_reply_target": &reply_to,
            });
            // Thread saved voice audio filename so chat.rs persists the audio path.
            if let Some(ref audio_filename) = meta.audio_filename {
                params["_audio_filename"] = serde_json::json!(audio_filename);
            }

            // Forward the channel's default model to chat.send() if configured.
            // If no channel model is set, check if the session already has a model.
            // If neither exists, assign the first registered model so the session
            // behaves the same as the web UI (which always sends an explicit model).
            if let Some(ref model) = meta.model {
                params["model"] = serde_json::json!(model);

                // Notify the user which model was assigned from the channel config
                // on the first message of a new session (no model set yet).
                let session_has_model = if let Some(ref sm) = state.services.session_metadata {
                    sm.get(&session_key).await.and_then(|e| e.model).is_some()
                } else {
                    false
                };
                if !session_has_model {
                    // Persist channel model on the session.
                    let _ = state
                        .services
                        .session
                        .patch(serde_json::json!({
                            "key": &session_key,
                            "model": model,
                        }))
                        .await;

                    // Buffer model notification for the logbook instead of sending separately.
                    let display: String = if let Ok(models_val) = state.services.model.list().await
                        && let Some(models) = models_val.as_array()
                    {
                        models
                            .iter()
                            .find(|m| m.get("id").and_then(|v| v.as_str()) == Some(model))
                            .and_then(|m| m.get("displayName").and_then(|v| v.as_str()))
                            .unwrap_or(model)
                            .to_string()
                    } else {
                        model.clone()
                    };
                    let msg = format!("Using {display}. Use /model to change.");
                    state.push_channel_status_log(&session_key, msg).await;
                }
            } else {
                let session_has_model = if let Some(ref sm) = state.services.session_metadata {
                    sm.get(&session_key).await.and_then(|e| e.model).is_some()
                } else {
                    false
                };
                if !session_has_model
                    && let Ok(models_val) = state.services.model.list().await
                    && let Some(models) = models_val.as_array()
                    && let Some(first) = models.first()
                    && let Some(id) = first.get("id").and_then(|v| v.as_str())
                {
                    params["model"] = serde_json::json!(id);
                    let _ = state
                        .services
                        .session
                        .patch(serde_json::json!({
                            "key": &session_key,
                            "model": id,
                        }))
                        .await;

                    // Buffer model notification for the logbook.
                    let display = first
                        .get("displayName")
                        .and_then(|v| v.as_str())
                        .unwrap_or(id);
                    let msg = format!("Using {display}. Use /model to change.");
                    state.push_channel_status_log(&session_key, msg).await;
                }
            }

            // Send a repeating "typing" indicator every 4s until chat.send()
            // completes. Telegram's typing status expires after ~5s.
            let send_result = if let Some(outbound) = state.services.channel_outbound_arc() {
                let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();
                let account_id = reply_to.account_id.clone();
                let chat_id = reply_to.chat_id.clone();
                tokio::spawn(async move {
                    debug!(
                        account_id = account_id,
                        chat_id = chat_id,
                        "starting typing indicator loop"
                    );
                    loop {
                        if let Err(e) = outbound.send_typing(&account_id, &chat_id).await {
                            debug!(
                                account_id = account_id,
                                chat_id = chat_id,
                                "typing indicator failed: {e}"
                            );
                        } else {
                            debug!(
                                account_id = account_id,
                                chat_id = chat_id,
                                "typing indicator sent"
                            );
                        }
                        tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_secs(4)) => {
                                debug!(
                                    account_id = account_id,
                                    chat_id = chat_id,
                                    "typing loop: 4s elapsed, sending again"
                                );
                            },
                            _ = &mut done_rx => {
                                debug!(
                                    account_id = account_id,
                                    chat_id = chat_id,
                                    "typing loop: chat completed, stopping"
                                );
                                break;
                            },
                        }
                    }
                });
                let result = chat.send(params).await;
                let _ = done_tx.send(());
                result
            } else {
                chat.send(params).await
            };

            if let Err(e) = send_result {
                error!("channel dispatch_to_chat failed: {e}");
                // Send the error back to the originating channel so the user
                // knows something went wrong.
                if let Some(outbound) = state.services.channel_outbound_arc() {
                    let error_msg = format!("⚠️ {e}");
                    if let Err(send_err) = outbound
                        .send_text(
                            &reply_to.account_id,
                            &reply_to.chat_id,
                            &error_msg,
                            reply_to.message_id.as_deref(),
                        )
                        .await
                    {
                        warn!("failed to send error back to channel: {send_err}");
                    }
                }
            }
        } else {
            warn!("channel dispatch_to_chat: gateway not ready");
        }
    }

    async fn request_disable_account(&self, channel_type: &str, account_id: &str, reason: &str) {
        warn!(
            channel_type,
            account_id,
            reason,
            "stopping local polling: detected bot already running on another instance"
        );

        if let Some(state) = self.state.get() {
            // Note: We intentionally do NOT remove the channel from the database.
            // The channel config should remain persisted so other moltis instances
            // sharing the same database can still use it. The polling loop will
            // cancel itself after this call returns.

            // Broadcast an event so the UI can update.
            let channel_type: ChannelType = match channel_type.parse() {
                Ok(ct) => ct,
                Err(e) => {
                    warn!("request_disable_account: {e}");
                    return;
                },
            };
            let event = ChannelEvent::AccountDisabled {
                channel_type,
                account_id: account_id.to_string(),
                reason: reason.to_string(),
            };
            let payload = match serde_json::to_value(&event) {
                Ok(v) => v,
                Err(e) => {
                    warn!("failed to serialize AccountDisabled event: {e}");
                    return;
                },
            };
            broadcast(state, "channel", payload, BroadcastOpts {
                drop_if_slow: true,
                ..Default::default()
            })
            .await;
        } else {
            warn!("request_disable_account: gateway not ready");
        }
    }

    async fn request_sender_approval(
        &self,
        _channel_type: &str,
        account_id: &str,
        identifier: &str,
    ) {
        if let Some(state) = self.state.get() {
            let params = serde_json::json!({
                "account_id": account_id,
                "identifier": identifier,
            });
            match state.services.channel.sender_approve(params).await {
                Ok(_) => {
                    info!(account_id, identifier, "OTP self-approval: sender approved");
                },
                Err(e) => {
                    warn!(
                        account_id,
                        identifier,
                        error = %e,
                        "OTP self-approval: failed to approve sender"
                    );
                },
            }
        } else {
            warn!("request_sender_approval: gateway not ready");
        }
    }

    async fn save_channel_voice(
        &self,
        audio_data: &[u8],
        filename: &str,
        reply_to: &ChannelReplyTarget,
    ) -> Option<String> {
        let state = self.state.get()?;
        let session_key = if let Some(ref sm) = state.services.session_metadata {
            resolve_inbound_channel_session(state, reply_to, sm).await
        } else {
            default_channel_session_key(reply_to)
        };
        let store = state.services.session_store.as_ref()?;
        match store.save_media(&session_key, filename, audio_data).await {
            Ok(_) => {
                debug!(
                    session_key,
                    filename, "saved channel voice audio to session media"
                );
                Some(filename.to_string())
            },
            Err(e) => {
                warn!(session_key, filename, error = %e, "failed to save channel voice audio");
                None
            },
        }
    }

    async fn transcribe_voice(&self, audio_data: &[u8], format: &str) -> ChannelResult<String> {
        let state = self
            .state
            .get()
            .ok_or_else(|| ChannelError::unavailable("gateway not ready"))?;

        let result = state
            .services
            .stt
            .transcribe_bytes(
                bytes::Bytes::copy_from_slice(audio_data),
                format,
                None,
                None,
                None,
            )
            .await
            .map_err(|e| ChannelError::unavailable(format!("transcription failed: {e}")))?;

        let text = result
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ChannelError::invalid_input("transcription result missing text"))?;

        Ok(text.to_string())
    }

    async fn voice_stt_available(&self) -> bool {
        let Some(state) = self.state.get() else {
            return false;
        };

        match state.services.stt.status().await {
            Ok(status) => status
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            Err(_) => false,
        }
    }

    async fn update_location(
        &self,
        reply_to: &ChannelReplyTarget,
        latitude: f64,
        longitude: f64,
    ) -> bool {
        let Some(state) = self.state.get() else {
            warn!("update_location: gateway not ready");
            return false;
        };

        let session_key = if let Some(ref sm) = state.services.session_metadata {
            resolve_inbound_channel_session(state, reply_to, sm).await
        } else {
            default_channel_session_key(reply_to)
        };

        // Update in-memory cache.
        let geo = moltis_config::GeoLocation::now(latitude, longitude, None);
        state.inner.write().await.cached_location = Some(geo.clone());

        // Persist to USER.md (best-effort).
        let mut user = moltis_config::load_user().unwrap_or_default();
        user.location = Some(geo);
        if let Err(e) = moltis_config::save_user(&user) {
            warn!(error = %e, "failed to persist location to USER.md");
        }

        // Check for a pending tool-triggered location request.
        let pending_key = format!("channel_location:{session_key}");
        let pending = state
            .inner
            .write()
            .await
            .pending_invokes
            .remove(&pending_key);
        if let Some(invoke) = pending {
            let result = serde_json::json!({
                "location": {
                    "latitude": latitude,
                    "longitude": longitude,
                    "accuracy": 0.0,
                }
            });
            let _ = invoke.sender.send(result);
            info!(session_key, "resolved pending channel location request");
            return true;
        }

        false
    }

    async fn dispatch_to_chat_with_attachments(
        &self,
        text: &str,
        attachments: Vec<ChannelAttachment>,
        reply_to: ChannelReplyTarget,
        meta: ChannelMessageMeta,
    ) {
        if attachments.is_empty() {
            // No attachments, use the regular dispatch
            self.dispatch_to_chat(text, reply_to, meta).await;
            return;
        }

        let Some(state) = self.state.get() else {
            warn!("channel dispatch_to_chat_with_attachments: gateway not ready");
            return;
        };

        let session_key = if let Some(ref sm) = state.services.session_metadata {
            resolve_inbound_channel_session(state, &reply_to, sm).await
        } else {
            default_channel_session_key(&reply_to)
        };
        let saved_non_image_paths =
            persist_non_image_attachments(state, &session_key, &reply_to, &attachments).await;

        let mut non_image_types = Vec::new();
        let has_images = attachments.iter().any(|attachment| {
            let is_image = is_image_media_type(&attachment.media_type);
            if !is_image {
                non_image_types.push(attachment.media_type.clone());
            }
            is_image
        });

        // If there are no image attachments, downgrade to plain-text dispatch.
        // This avoids sending multimodal payloads (content arrays) to providers
        // that reject them when only non-image files are present.
        if !has_images {
            let mut fallback_text = text.trim().to_string();
            if !non_image_types.is_empty() {
                let kinds = non_image_types.join(", ");
                let notice = format!(
                    "[Attachment Notice] Non-image attachment MIME types: {kinds}. Ask the user for plain text content or a screenshot/image if visual parsing is needed."
                );
                if fallback_text.is_empty() {
                    fallback_text = notice;
                } else {
                    fallback_text = format!("{fallback_text}\n\n{notice}");
                }
            }
            if !saved_non_image_paths.is_empty() {
                let saved = saved_non_image_paths.join("\n- ");
                fallback_text = if fallback_text.is_empty() {
                    format!(
                        "[Attachment Saved]\n- {saved}\nUse these local paths directly when reading attachments."
                    )
                } else {
                    format!(
                        "{fallback_text}\n\n[Attachment Saved]\n- {saved}\nUse these local paths directly when reading attachments."
                    )
                };
            }
            self.dispatch_to_chat(&fallback_text, reply_to, meta).await;
            return;
        }
        let mut dispatch_text =
            maybe_apply_handoff_context(state, &session_key, text.to_string()).await;

        // Build multimodal content array (OpenAI format)
        let mut content_parts: Vec<serde_json::Value> = Vec::new();
        let mut image_count = 0usize;
        let mut skipped_types: Vec<String> = Vec::new();

        // Add image parts. Non-image attachments are represented as a text hint
        // to avoid sending invalid multimodal payloads to providers.
        for attachment in &attachments {
            if !is_image_media_type(&attachment.media_type) {
                skipped_types.push(attachment.media_type.clone());
                continue;
            }
            let base64_data = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                &attachment.data,
            );
            let data_uri = format!("data:{};base64,{}", attachment.media_type, base64_data);
            content_parts.push(serde_json::json!({
                "type": "image_url",
                "image_url": {
                    "url": data_uri,
                },
            }));
            image_count = image_count.saturating_add(1);
        }

        if !skipped_types.is_empty() {
            let kinds = skipped_types.join(", ");
            let mut notice = format!(
                "[Attachment Notice] Non-image attachment MIME types: {kinds}. Ask the user for plain text content or a screenshot/image if visual parsing is needed."
            );
            if !saved_non_image_paths.is_empty() {
                let saved = saved_non_image_paths.join("\n- ");
                notice.push_str(&format!(
                    "\n[Attachment Saved]\n- {saved}\nUse these local paths directly when reading attachments."
                ));
            }
            if dispatch_text.trim().is_empty() {
                dispatch_text = notice;
            } else {
                dispatch_text = format!("{dispatch_text}\n\n{notice}");
            }
        }

        // Add text part if not empty.
        if !dispatch_text.trim().is_empty() {
            content_parts.push(serde_json::json!({
                "type": "text",
                "text": dispatch_text,
            }));
        }

        if content_parts.is_empty() {
            // Defensive fallback: should not happen, but avoid sending an empty
            // multimodal content array downstream.
            content_parts.push(serde_json::json!({
                "type": "text",
                "text": "[Attachment]",
            }));
        }

        debug!(
            session_key = %session_key,
            text_len = text.len(),
            attachment_count = attachments.len(),
            image_count,
            skipped_attachment_count = skipped_types.len(),
            "dispatching multimodal message to chat"
        );

        // Broadcast a "chat" event so the web UI shows the user message
        let msg_index = if let Some(ref store) = state.services.session_store {
            store.count(&session_key).await.unwrap_or(0)
        } else {
            0
        };

        // For the broadcast, just show the text portion
        let placeholder = attachment_placeholder_text(image_count > 0, !skipped_types.is_empty());
        let payload = serde_json::json!({
            "state": "channel_user",
            "text": if text.is_empty() { placeholder } else { text },
            "channel": &meta,
            "sessionKey": &session_key,
            "messageIndex": msg_index,
            "hasAttachments": true,
        });
        broadcast(state, "chat", payload, BroadcastOpts {
            drop_if_slow: true,
            ..Default::default()
        })
        .await;

        // Persist channel binding (ensure session row exists first —
        // set_channel_binding is an UPDATE so the row must already be present).
        if let Ok(binding_json) = serde_json::to_string(&reply_to)
            && let Some(ref session_meta) = state.services.session_metadata
        {
            let entry = session_meta.get(&session_key).await;
            if entry.as_ref().is_none_or(|e| e.channel_binding.is_none()) {
                let existing = session_meta
                    .list_channel_sessions(
                        reply_to.channel_type.as_str(),
                        &reply_to.account_id,
                        &reply_to.chat_id,
                    )
                    .await;
                let n = existing.len() + 1;
                let _ = session_meta
                    .upsert(
                        &session_key,
                        Some(format!("{} {n}", reply_to.channel_type.display_name())),
                    )
                    .await;
            }
            session_meta
                .set_channel_binding(&session_key, Some(binding_json))
                .await;
            if let Some(entry) = session_meta.get(&session_key).await
                && entry
                    .agent_id
                    .as_deref()
                    .map(str::trim)
                    .is_none_or(|value| value.is_empty())
            {
                let channel_agent = channel_config_string(state, &reply_to, "agent_id").await;
                let default_agent = if let Some(agent_id) = channel_agent {
                    agent_id
                } else if let Some(ref store) = state.services.agent_persona_store {
                    store
                        .default_id()
                        .await
                        .unwrap_or_else(|_| "main".to_string())
                } else {
                    "main".to_string()
                };
                let _ = session_meta
                    .set_agent_id(&session_key, Some(&default_agent))
                    .await;
            }
        }

        let chat = state.chat().await;
        let mut params = serde_json::json!({
            "content": content_parts,
            "channel": &meta,
            "_session_key": &session_key,
            // Defer reply-target registration until chat.send() actually
            // starts executing this message (after semaphore acquire).
            "_channel_reply_target": &reply_to,
        });

        // Forward the channel's default model if configured
        if let Some(ref model) = meta.model {
            params["model"] = serde_json::json!(model);

            let session_has_model = if let Some(ref sm) = state.services.session_metadata {
                sm.get(&session_key).await.and_then(|e| e.model).is_some()
            } else {
                false
            };
            if !session_has_model {
                let _ = state
                    .services
                    .session
                    .patch(serde_json::json!({
                        "key": &session_key,
                        "model": model,
                    }))
                    .await;

                let display: String = if let Ok(models_val) = state.services.model.list().await
                    && let Some(models) = models_val.as_array()
                {
                    models
                        .iter()
                        .find(|m| m.get("id").and_then(|v| v.as_str()) == Some(model))
                        .and_then(|m| m.get("displayName").and_then(|v| v.as_str()))
                        .unwrap_or(model)
                        .to_string()
                } else {
                    model.clone()
                };
                let msg = format!("Using {display}. Use /model to change.");
                state.push_channel_status_log(&session_key, msg).await;
            }
        } else {
            let session_has_model = if let Some(ref sm) = state.services.session_metadata {
                sm.get(&session_key).await.and_then(|e| e.model).is_some()
            } else {
                false
            };
            if !session_has_model
                && let Ok(models_val) = state.services.model.list().await
                && let Some(models) = models_val.as_array()
                && let Some(first) = models.first()
                && let Some(id) = first.get("id").and_then(|v| v.as_str())
            {
                params["model"] = serde_json::json!(id);
                let _ = state
                    .services
                    .session
                    .patch(serde_json::json!({
                        "key": &session_key,
                        "model": id,
                    }))
                    .await;

                let display = first
                    .get("displayName")
                    .and_then(|v| v.as_str())
                    .unwrap_or(id);
                let msg = format!("Using {display}. Use /model to change.");
                state.push_channel_status_log(&session_key, msg).await;
            }
        }

        // Send typing indicator and dispatch to chat
        let send_result = if let Some(outbound) = state.services.channel_outbound_arc() {
            let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();
            let account_id = reply_to.account_id.clone();
            let chat_id = reply_to.chat_id.clone();
            tokio::spawn(async move {
                loop {
                    if let Err(e) = outbound.send_typing(&account_id, &chat_id).await {
                        debug!(account_id, chat_id, "typing indicator failed: {e}");
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(4)) => {},
                        _ = &mut done_rx => break,
                    }
                }
            });
            let result = chat.send(params).await;
            let _ = done_tx.send(());
            result
        } else {
            chat.send(params).await
        };

        if let Err(e) = send_result {
            error!("channel dispatch_to_chat_with_attachments failed: {e}");
            if let Some(outbound) = state.services.channel_outbound_arc() {
                let error_msg = format!("⚠️ {e}");
                if let Err(send_err) = outbound
                    .send_text(
                        &reply_to.account_id,
                        &reply_to.chat_id,
                        &error_msg,
                        reply_to.message_id.as_deref(),
                    )
                    .await
                {
                    warn!("failed to send error back to channel: {send_err}");
                }
            }
        }
    }

    async fn dispatch_command(
        &self,
        command: &str,
        reply_to: ChannelReplyTarget,
    ) -> ChannelResult<String> {
        let state = self
            .state
            .get()
            .ok_or_else(|| ChannelError::unavailable("gateway not ready"))?;
        let session_metadata = state
            .services
            .session_metadata
            .as_ref()
            .ok_or_else(|| ChannelError::unavailable("session metadata not available"))?;
        let session_key = resolve_channel_session(&reply_to, session_metadata).await;
        let chat = state.chat().await;

        // Extract the command name (first word) and args (rest).
        let cmd = command.split_whitespace().next().unwrap_or("");
        let args = command[cmd.len()..].trim();

        match cmd {
            "new" => {
                // Create a new session with a fresh UUID key.
                let new_key = format!("session:{}", uuid::Uuid::new_v4());
                let binding_json = serde_json::to_string(&reply_to)
                    .map_err(|e| ChannelError::external("serialize channel binding", e))?;

                // Sequential label: count existing sessions for this chat.
                let existing = session_metadata
                    .list_channel_sessions(
                        reply_to.channel_type.as_str(),
                        &reply_to.account_id,
                        &reply_to.chat_id,
                    )
                    .await;
                let n = existing.len() + 1;

                // Create the new session entry with channel binding.
                session_metadata
                    .upsert(
                        &new_key,
                        Some(format!("{} {n}", reply_to.channel_type.display_name())),
                    )
                    .await
                    .map_err(|e| ChannelError::external("create channel session", e))?;
                session_metadata
                    .set_channel_binding(&new_key, Some(binding_json.clone()))
                    .await;

                // Ensure the old session also has a channel binding (for listing).
                let old_entry = session_metadata.get(&session_key).await;
                if old_entry
                    .as_ref()
                    .and_then(|e| e.channel_binding.as_ref())
                    .is_none()
                {
                    session_metadata
                        .set_channel_binding(&session_key, Some(binding_json))
                        .await;
                }

                let inherited_agent = old_entry
                    .as_ref()
                    .and_then(|entry| entry.agent_id.as_deref())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                let channel_agent = channel_config_string(state, &reply_to, "agent_id").await;
                let target_agent = if let Some(agent_id) = inherited_agent {
                    agent_id
                } else if let Some(agent_id) = channel_agent {
                    agent_id
                } else if let Some(ref store) = state.services.agent_persona_store {
                    store
                        .default_id()
                        .await
                        .unwrap_or_else(|_| "main".to_string())
                } else {
                    "main".to_string()
                };
                let _ = session_metadata
                    .set_agent_id(&new_key, Some(&target_agent))
                    .await;

                // Update forward mapping.
                session_metadata
                    .set_active_session(
                        reply_to.channel_type.as_str(),
                        &reply_to.account_id,
                        &reply_to.chat_id,
                        &new_key,
                    )
                    .await;

                info!(
                    old_session = %session_key,
                    new_session = %new_key,
                    "channel /new: created new session"
                );

                // Assign a model to the new session: prefer the channel's
                // configured model, fall back to the first registered model.
                let channel_model: Option<String> =
                    state.services.channel.status().await.ok().and_then(|v| {
                        let channels = v.get("channels")?.as_array()?;
                        channels
                            .iter()
                            .find(|ch| {
                                ch.get("account_id").and_then(|v| v.as_str())
                                    == Some(&reply_to.account_id)
                            })
                            .and_then(|ch| {
                                ch.get("config")?.get("model")?.as_str().map(String::from)
                            })
                    });

                let models_val = state.services.model.list().await.ok();
                let models = models_val.as_ref().and_then(|v| v.as_array());

                let (model_id, model_display): (Option<String>, String) = if let Some(ref cm) =
                    channel_model
                {
                    let d = models
                        .and_then(|ms| {
                            ms.iter()
                                .find(|m| m.get("id").and_then(|v| v.as_str()) == Some(cm.as_str()))
                                .and_then(|m| m.get("displayName").and_then(|v| v.as_str()))
                        })
                        .unwrap_or(cm.as_str());
                    (Some(cm.clone()), d.to_string())
                } else if let Some(ms) = models
                    && let Some(first) = ms.first()
                    && let Some(id) = first.get("id").and_then(|v| v.as_str())
                {
                    let d = first
                        .get("displayName")
                        .and_then(|v| v.as_str())
                        .unwrap_or(id);
                    (Some(id.to_string()), d.to_string())
                } else {
                    (None, String::new())
                };

                if let Some(ref mid) = model_id {
                    let _ = state
                        .services
                        .session
                        .patch(serde_json::json!({
                            "key": &new_key,
                            "model": mid,
                        }))
                        .await;
                }

                // Notify web UI so the session list refreshes.
                broadcast(
                    state,
                    "session",
                    serde_json::json!({
                        "kind": "created",
                        "sessionKey": &new_key,
                    }),
                    BroadcastOpts {
                        drop_if_slow: true,
                        ..Default::default()
                    },
                )
                .await;

                if model_display.is_empty() {
                    Ok("New session started.".to_string())
                } else {
                    Ok(format!(
                        "New session started. Using *{model_display}*. Use /model to change."
                    ))
                }
            },
            "clear" => {
                let params = serde_json::json!({ "_session_key": &session_key });
                chat.clear(params)
                    .await
                    .map_err(ChannelError::unavailable)?;
                Ok("Session cleared.".to_string())
            },
            "compact" => {
                let params = serde_json::json!({ "_session_key": &session_key });
                chat.compact(params)
                    .await
                    .map_err(ChannelError::unavailable)?;
                Ok("Session compacted.".to_string())
            },
            "context" => {
                let params = serde_json::json!({ "_session_key": &session_key });
                let res = chat
                    .context(params)
                    .await
                    .map_err(ChannelError::unavailable)?;

                let session_info = res.get("session").cloned().unwrap_or_default();
                let msg_count = session_info
                    .get("messageCount")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let provider = session_info
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let model = session_info
                    .get("model")
                    .and_then(|v| v.as_str())
                    .unwrap_or("default");

                let tokens = res.get("tokenUsage").cloned().unwrap_or_default();
                let total = tokens.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                let context_window = tokens
                    .get("contextWindow")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                // Sandbox section
                let sandbox = res.get("sandbox").cloned().unwrap_or_default();
                let sandbox_enabled = sandbox
                    .get("enabled")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let sandbox_line = if sandbox_enabled {
                    let image = sandbox
                        .get("image")
                        .and_then(|v| v.as_str())
                        .unwrap_or("default");
                    format!("**Sandbox:** on · `{image}`")
                } else {
                    "**Sandbox:** off".to_string()
                };

                // Skills/plugins section
                let skills = res
                    .get("skills")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                let skills_line = if skills.is_empty() {
                    "**Plugins:** none".to_string()
                } else {
                    let names: Vec<_> = skills
                        .iter()
                        .filter_map(|s| s.get("name").and_then(|v| v.as_str()))
                        .collect();
                    format!("**Plugins:** {}", names.join(", "))
                };

                Ok(format!(
                    "**Session:** `{session_key}`\n**Messages:** {msg_count}\n**Provider:** {provider}\n**Model:** `{model}`\n{sandbox_line}\n{skills_line}\n**Tokens:** ~{total}/{context_window}"
                ))
            },
            "sessions" => {
                let auto_archived = auto_archive_stale_channel_sessions(
                    state,
                    session_metadata,
                    &reply_to,
                    &session_key,
                )
                .await;

                let sessions = session_metadata
                    .list_channel_sessions(
                        reply_to.channel_type.as_str(),
                        &reply_to.account_id,
                        &reply_to.chat_id,
                    )
                    .await;

                if sessions.is_empty() {
                    return Ok("No sessions found. Send a message to start one.".to_string());
                }

                match parse_sessions_command_args(args)? {
                    SessionsCommand::List => {
                        let mut lines = Vec::new();
                        for (i, s) in sessions.iter().enumerate() {
                            let label = s.label.as_deref().unwrap_or(&s.key);
                            let marker = if s.key == session_key {
                                " *"
                            } else {
                                ""
                            };
                            let archived = if s.archived {
                                " (archived)"
                            } else {
                                ""
                            };
                            lines.push(format!(
                                "{}. {} ({} msgs){}{}",
                                i + 1,
                                label,
                                s.message_count,
                                archived,
                                marker,
                            ));
                        }
                        if auto_archived > 0 {
                            lines.push(format!(
                                "\nAuto-archived {auto_archived} stale session(s)."
                            ));
                        }
                        lines.push("\nUse /sessions N to switch.".to_string());
                        lines.push("Use /sessions archive N to archive a session.".to_string());
                        lines.push("Use /sessions unarchive N to restore a session.".to_string());
                        Ok(lines.join("\n"))
                    }
                    SessionsCommand::Archive(n) => {
                        if n > sessions.len() {
                            return Err(ChannelError::invalid_input(format!(
                                "invalid session number. Use 1–{}.",
                                sessions.len()
                            )));
                        }
                        let target = &sessions[n - 1];
                        if target.key == session_key {
                            return Err(ChannelError::invalid_input(
                                "cannot archive the currently active session",
                            ));
                        }
                        if target.archived {
                            return Ok("Session is already archived.".to_string());
                        }
                        session_metadata
                            .set_archived(&target.key, true)
                            .await
                            .map_err(|e| ChannelError::external("archive session", e))?;
                        let label = target.label.as_deref().unwrap_or(&target.key);
                        Ok(format!("Archived: {label}"))
                    }
                    SessionsCommand::Unarchive(n) => {
                        if n > sessions.len() {
                            return Err(ChannelError::invalid_input(format!(
                                "invalid session number. Use 1–{}.",
                                sessions.len()
                            )));
                        }
                        let target = &sessions[n - 1];
                        if !target.archived {
                            return Ok("Session is already active.".to_string());
                        }
                        session_metadata
                            .set_archived(&target.key, false)
                            .await
                            .map_err(|e| ChannelError::external("unarchive session", e))?;
                        let label = target.label.as_deref().unwrap_or(&target.key);
                        Ok(format!("Unarchived: {label}"))
                    }
                    SessionsCommand::Switch(n) => {
                        if n > sessions.len() {
                            return Err(ChannelError::invalid_input(format!(
                                "invalid session number. Use 1–{}.",
                                sessions.len()
                            )));
                        }
                        let target_session = &sessions[n - 1];
                        if target_session.archived {
                            return Err(ChannelError::invalid_input(
                                "selected session is archived. Use /sessions unarchive N first.",
                            ));
                        }

                        // Update forward mapping.
                        session_metadata
                            .set_active_session(
                                reply_to.channel_type.as_str(),
                                &reply_to.account_id,
                                &reply_to.chat_id,
                                &target_session.key,
                            )
                            .await;

                        let label = target_session
                            .label
                            .as_deref()
                            .unwrap_or(&target_session.key);
                        info!(
                            session = %target_session.key,
                            "channel /sessions: switched session"
                        );

                        broadcast(
                            state,
                            "session",
                            serde_json::json!({
                                "kind": "switched",
                                "sessionKey": &target_session.key,
                            }),
                            BroadcastOpts {
                                drop_if_slow: true,
                                ..Default::default()
                            },
                        )
                        .await;

                        Ok(format!("Switched to: {label}"))
                    }
                }
            },
            "agent" => {
                if reply_to.channel_type == ChannelType::Feishu {
                    let allow_switch =
                        channel_config_bool(state, &reply_to, "allow_agent_switch")
                            .await
                            .unwrap_or(false);
                    if !allow_switch {
                        return Err(ChannelError::invalid_input(
                            "agent switching is disabled for this bot",
                        ));
                    }
                }
                let Some(ref store) = state.services.agent_persona_store else {
                    return Err(ChannelError::unavailable(
                        "agent personas are not available",
                    ));
                };
                let default_id = store
                    .default_id()
                    .await
                    .unwrap_or_else(|_| "main".to_string());
                let agents = store
                    .list()
                    .await
                    .map_err(|e| ChannelError::external("listing agents", e))?;
                let current_agent = session_metadata
                    .get(&session_key)
                    .await
                    .and_then(|entry| entry.agent_id)
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or(default_id.clone());

                if args.is_empty() {
                    let mut lines = Vec::new();
                    for (i, agent) in agents.iter().enumerate() {
                        let marker = if agent.id == current_agent {
                            " *"
                        } else {
                            ""
                        };
                        let default_badge = if agent.id == default_id {
                            " (default)"
                        } else {
                            ""
                        };
                        let emoji = agent.emoji.clone().unwrap_or_default();
                        let label = if emoji.is_empty() {
                            agent.name.clone()
                        } else {
                            format!("{emoji} {}", agent.name)
                        };
                        lines.push(format!(
                            "{}. {} [{}]{}{}",
                            i + 1,
                            label,
                            agent.id,
                            default_badge,
                            marker,
                        ));
                    }
                    lines.push("\nUse /agent <id> to switch.".to_string());
                    Ok(lines.join("\n"))
                } else {
                    let chosen = resolve_agent_selector(&agents, args)?;
                    session_metadata
                        .set_agent_id(&session_key, Some(&chosen.id))
                        .await
                        .map_err(|e| ChannelError::external("setting session agent", e))?;

                    broadcast(
                        state,
                        "session",
                        serde_json::json!({
                            "kind": "patched",
                            "sessionKey": &session_key,
                        }),
                        BroadcastOpts {
                            drop_if_slow: true,
                            ..Default::default()
                        },
                    )
                    .await;

                    let emoji = chosen.emoji.clone().unwrap_or_default();
                    if emoji.is_empty() {
                        Ok(format!("Agent switched to: {}", chosen.name))
                    } else {
                        Ok(format!("Agent switched to: {} {}", emoji, chosen.name))
                    }
                }
            },
            "handoff" => {
                if reply_to.channel_type == ChannelType::Feishu {
                    let allow_switch =
                        channel_config_bool(state, &reply_to, "allow_agent_switch")
                            .await
                            .unwrap_or(false);
                    if !allow_switch {
                        return Err(ChannelError::invalid_input(
                            "agent switching is disabled for this bot",
                        ));
                    }
                }

                let Some(ref store) = state.services.agent_persona_store else {
                    return Err(ChannelError::unavailable(
                        "agent personas are not available",
                    ));
                };

                let default_id = store
                    .default_id()
                    .await
                    .unwrap_or_else(|_| "main".to_string());
                let agents = store
                    .list()
                    .await
                    .map_err(|e| ChannelError::external("listing agents", e))?;
                let current_agent = current_agent_id_for_session(state, session_metadata, &session_key).await;

                if args.is_empty() {
                    let mut lines = Vec::new();
                    for (i, agent) in agents.iter().enumerate() {
                        let marker = if agent.id == current_agent {
                            " *"
                        } else {
                            ""
                        };
                        let default_badge = if agent.id == default_id {
                            " (default)"
                        } else {
                            ""
                        };
                        lines.push(format!(
                            "{}. {} [{}]{}{}",
                            i + 1,
                            agent.name,
                            agent.id,
                            default_badge,
                            marker,
                        ));
                    }
                    lines.push(
                        "\nUse /handoff <id> [--mode same_session|new_session|fork_session] [note]"
                            .to_string(),
                    );
                    return Ok(lines.join("\n"));
                }

                let (selector, mode, note) = parse_handoff_args(args)?;
                let chosen = resolve_agent_selector(&agents, &selector)?;
                let mut target_session_key = session_key.clone();
                let source_session_key = session_key.clone();

                match mode {
                    HandoffMode::SameSession => {
                        session_metadata
                            .set_agent_id(&session_key, Some(&chosen.id))
                            .await
                            .map_err(|e| ChannelError::external("setting session agent", e))?;

                        broadcast(
                            state,
                            "session",
                            serde_json::json!({
                                "kind": "patched",
                                "sessionKey": &session_key,
                            }),
                            BroadcastOpts {
                                drop_if_slow: true,
                                ..Default::default()
                            },
                        )
                        .await;
                    }
                    HandoffMode::NewSession => {
                        let new_key = format!("session:{}", uuid::Uuid::new_v4());
                        let binding_json = serde_json::to_string(&reply_to)
                            .map_err(|e| ChannelError::external("serialize channel binding", e))?;
                        let n = session_metadata
                            .list_channel_sessions(
                                reply_to.channel_type.as_str(),
                                &reply_to.account_id,
                                &reply_to.chat_id,
                            )
                            .await
                            .len()
                            + 1;
                        session_metadata
                            .upsert(
                                &new_key,
                                Some(format!("{} {n}", reply_to.channel_type.display_name())),
                            )
                            .await
                            .map_err(|e| ChannelError::external("create handoff session", e))?;
                        session_metadata
                            .set_channel_binding(&new_key, Some(binding_json))
                            .await;
                        if let Some(source_entry) = session_metadata.get(&session_key).await {
                            if source_entry.model.is_some() {
                                session_metadata.set_model(&new_key, source_entry.model).await;
                            }
                            if source_entry.project_id.is_some() {
                                session_metadata
                                    .set_project_id(&new_key, source_entry.project_id)
                                    .await;
                            }
                            if source_entry.mcp_disabled.is_some() {
                                session_metadata
                                    .set_mcp_disabled(&new_key, source_entry.mcp_disabled)
                                    .await;
                            }
                        }
                        session_metadata
                            .set_agent_id(&new_key, Some(&chosen.id))
                            .await
                            .map_err(|e| ChannelError::external("setting handoff agent", e))?;
                        session_metadata
                            .set_active_session(
                                reply_to.channel_type.as_str(),
                                &reply_to.account_id,
                                &reply_to.chat_id,
                                &new_key,
                            )
                            .await;

                        broadcast(
                            state,
                            "session",
                            serde_json::json!({
                                "kind": "created",
                                "sessionKey": &new_key,
                            }),
                            BroadcastOpts {
                                drop_if_slow: true,
                                ..Default::default()
                            },
                        )
                        .await;
                        target_session_key = new_key;
                    }
                    HandoffMode::ForkSession => {
                        let forked = state
                            .services
                            .session
                            .fork(serde_json::json!({
                                "key": &session_key,
                            }))
                            .await
                            .map_err(ChannelError::unavailable)?;
                        let new_key = forked
                            .get("sessionKey")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                ChannelError::unavailable(
                                    "session fork did not return sessionKey",
                                )
                            })?
                            .to_string();

                        if let Ok(binding_json) = serde_json::to_string(&reply_to) {
                            session_metadata
                                .set_channel_binding(&new_key, Some(binding_json))
                                .await;
                        }
                        session_metadata
                            .set_agent_id(&new_key, Some(&chosen.id))
                            .await
                            .map_err(|e| ChannelError::external("setting handoff agent", e))?;
                        session_metadata
                            .set_active_session(
                                reply_to.channel_type.as_str(),
                                &reply_to.account_id,
                                &reply_to.chat_id,
                                &new_key,
                            )
                            .await;

                        broadcast(
                            state,
                            "session",
                            serde_json::json!({
                                "kind": "created",
                                "sessionKey": &new_key,
                            }),
                            BroadcastOpts {
                                drop_if_slow: true,
                                ..Default::default()
                            },
                        )
                        .await;
                        target_session_key = new_key;
                    }
                }

                if let Some(ref state_store) = state.services.session_state_store {
                    let packet = HandoffPacket {
                        version: 1,
                        mode,
                        from_agent_id: current_agent.clone(),
                        to_agent_id: chosen.id.clone(),
                        source_session_key: source_session_key.clone(),
                        target_session_key: target_session_key.clone(),
                        note: note.clone(),
                        created_at_ms: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64,
                    };
                    if let Ok(payload) = serde_json::to_string(&packet) {
                        let _ = state_store
                            .set(&target_session_key, HANDOFF_NAMESPACE, HANDOFF_PENDING_KEY, &payload)
                            .await;
                    }
                }

                let note_suffix = if note.is_empty() {
                    String::new()
                } else {
                    format!("\nNote: {note}")
                };
                Ok(format!(
                    "Handoff to {} [{}] via {}. Session: {}{}",
                    chosen.name,
                    chosen.id,
                    mode.as_str(),
                    target_session_key,
                    note_suffix
                ))
            }
            "model" => {
                let models_val = state
                    .services
                    .model
                    .list()
                    .await
                    .map_err(ChannelError::unavailable)?;
                let models = models_val
                    .as_array()
                    .ok_or_else(|| ChannelError::invalid_input("bad model list"))?;

                let current_model = {
                    let entry = session_metadata.get(&session_key).await;
                    entry.and_then(|e| e.model.clone())
                };

                if args.is_empty() {
                    // List unique providers.
                    let mut providers: Vec<String> = models
                        .iter()
                        .filter_map(|m| {
                            m.get("provider").and_then(|v| v.as_str()).map(String::from)
                        })
                        .collect();
                    providers.dedup();

                    if providers.len() <= 1 {
                        // Single provider — list models directly.
                        return Ok(format_model_list(models, current_model.as_deref(), None));
                    }

                    // Multiple providers — list them for selection.
                    // Prefix with "providers:" so Telegram handler knows.
                    let current_provider = current_model.as_deref().and_then(|cm| {
                        models.iter().find_map(|m| {
                            let id = m.get("id").and_then(|v| v.as_str())?;
                            if id == cm {
                                m.get("provider").and_then(|v| v.as_str()).map(String::from)
                            } else {
                                None
                            }
                        })
                    });
                    let mut lines = vec!["providers:".to_string()];
                    for (i, p) in providers.iter().enumerate() {
                        let count = models
                            .iter()
                            .filter(|m| m.get("provider").and_then(|v| v.as_str()) == Some(p))
                            .count();
                        let marker = if current_provider.as_deref() == Some(p) {
                            " *"
                        } else {
                            ""
                        };
                        lines.push(format!("{}. {} ({} models){}", i + 1, p, count, marker));
                    }
                    Ok(lines.join("\n"))
                } else if let Some(provider) = args.strip_prefix("provider:") {
                    // List models for a specific provider.
                    Ok(format_model_list(
                        models,
                        current_model.as_deref(),
                        Some(provider),
                    ))
                } else {
                    // Switch mode — arg is a 1-based global index.
                    let n: usize = args
                        .parse()
                        .map_err(|_| ChannelError::invalid_input("usage: /model [number]"))?;
                    if n == 0 || n > models.len() {
                        return Err(ChannelError::invalid_input(format!(
                            "invalid model number. Use 1–{}.",
                            models.len()
                        )));
                    }
                    let chosen = &models[n - 1];
                    let model_id = chosen
                        .get("id")
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| ChannelError::invalid_input("model has no id"))?;
                    let display = chosen
                        .get("displayName")
                        .and_then(|v| v.as_str())
                        .unwrap_or(model_id);

                    let patch_res = state
                        .services
                        .session
                        .patch(serde_json::json!({
                            "key": &session_key,
                            "model": model_id,
                        }))
                        .await
                        .map_err(ChannelError::unavailable)?;
                    let version = patch_res
                        .get("version")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);

                    broadcast(
                        state,
                        "session",
                        serde_json::json!({
                            "kind": "patched",
                            "sessionKey": &session_key,
                            "version": version,
                        }),
                        BroadcastOpts {
                            drop_if_slow: true,
                            ..Default::default()
                        },
                    )
                    .await;

                    Ok(format!("Model switched to: {display}"))
                }
            },
            "sandbox" => {
                let is_enabled = if let Some(ref router) = state.sandbox_router {
                    router.is_sandboxed(&session_key).await
                } else {
                    false
                };

                if args.is_empty() {
                    // Show current status and image list.
                    let current_image = {
                        let entry = session_metadata.get(&session_key).await;
                        let session_img = entry.and_then(|e| e.sandbox_image.clone());
                        match session_img {
                            Some(img) if !img.is_empty() => img,
                            _ => {
                                if let Some(ref router) = state.sandbox_router {
                                    router.default_image().await
                                } else {
                                    moltis_tools::sandbox::DEFAULT_SANDBOX_IMAGE.to_string()
                                }
                            },
                        }
                    };

                    let status = if is_enabled {
                        "on"
                    } else {
                        "off"
                    };

                    // List available images.
                    let builder = moltis_tools::image_cache::DockerImageBuilder::new();
                    let cached = builder.list_cached().await.unwrap_or_default();

                    let default_img = moltis_tools::sandbox::DEFAULT_SANDBOX_IMAGE.to_string();
                    let mut images: Vec<(String, Option<String>)> =
                        vec![(default_img.clone(), None)];
                    for img in &cached {
                        images.push((
                            img.tag.clone(),
                            Some(format!("{} ({})", img.skill_name, img.size)),
                        ));
                    }

                    let mut lines = vec![format!("status:{status}")];
                    for (i, (tag, subtitle)) in images.iter().enumerate() {
                        let marker = if *tag == current_image {
                            " *"
                        } else {
                            ""
                        };
                        let label = if let Some(sub) = subtitle {
                            format!("{}. {} — {}{}", i + 1, tag, sub, marker)
                        } else {
                            format!("{}. {}{}", i + 1, tag, marker)
                        };
                        lines.push(label);
                    }
                    Ok(lines.join("\n"))
                } else if args == "on" || args == "off" {
                    let new_val = args == "on";
                    let patch_res = state
                        .services
                        .session
                        .patch(serde_json::json!({
                            "key": &session_key,
                            "sandbox_enabled": new_val,
                        }))
                        .await
                        .map_err(ChannelError::unavailable)?;
                    let version = patch_res
                        .get("version")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    broadcast(
                        state,
                        "session",
                        serde_json::json!({
                            "kind": "patched",
                            "sessionKey": &session_key,
                            "version": version,
                        }),
                        BroadcastOpts {
                            drop_if_slow: true,
                            ..Default::default()
                        },
                    )
                    .await;
                    let label = if new_val {
                        "enabled"
                    } else {
                        "disabled"
                    };
                    Ok(format!("Sandbox {label}."))
                } else if let Some(rest) = args.strip_prefix("image ") {
                    let n: usize = rest.parse().map_err(|_| {
                        ChannelError::invalid_input("usage: /sandbox image [number]")
                    })?;

                    let default_img = moltis_tools::sandbox::DEFAULT_SANDBOX_IMAGE.to_string();
                    let builder = moltis_tools::image_cache::DockerImageBuilder::new();
                    let cached = builder.list_cached().await.unwrap_or_default();
                    let mut images: Vec<String> = vec![default_img];
                    for img in &cached {
                        images.push(img.tag.clone());
                    }

                    if n == 0 || n > images.len() {
                        return Err(ChannelError::invalid_input(format!(
                            "invalid image number. Use 1–{}.",
                            images.len()
                        )));
                    }
                    let chosen = &images[n - 1];

                    // If choosing the default image, clear the session override.
                    let patch_value = if n == 1 {
                        ""
                    } else {
                        chosen.as_str()
                    };
                    let patch_res = state
                        .services
                        .session
                        .patch(serde_json::json!({
                            "key": &session_key,
                            "sandbox_image": patch_value,
                        }))
                        .await
                        .map_err(ChannelError::unavailable)?;
                    let version = patch_res
                        .get("version")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);

                    broadcast(
                        state,
                        "session",
                        serde_json::json!({
                            "kind": "patched",
                            "sessionKey": &session_key,
                            "version": version,
                        }),
                        BroadcastOpts {
                            drop_if_slow: true,
                            ..Default::default()
                        },
                    )
                    .await;

                    Ok(format!("Image set to: {chosen}"))
                } else {
                    Err(ChannelError::invalid_input(
                        "usage: /sandbox [on|off|image N]",
                    ))
                }
            },
            "sh" => {
                let route = if let Some(ref router) = state.sandbox_router {
                    if router.is_sandboxed(&session_key).await {
                        "sandboxed"
                    } else {
                        "host"
                    }
                } else {
                    "host"
                };

                match args {
                    "" | "on" => {
                        state.set_channel_command_mode(&session_key, true).await;
                        Ok(format!(
                            "Command mode enabled ({route}). Send commands as plain messages. Use /sh off (or /sh exit) to leave."
                        ))
                    },
                    "off" | "exit" => {
                        state.set_channel_command_mode(&session_key, false).await;
                        Ok("Command mode disabled. Back to normal chat mode.".to_string())
                    },
                    "status" => {
                        let enabled = state.is_channel_command_mode_enabled(&session_key).await;
                        if enabled {
                            Ok(format!(
                                "Command mode is enabled ({route}). Use /sh off (or /sh exit) to leave."
                            ))
                        } else {
                            Ok(format!(
                                "Command mode is disabled ({route}). Use /sh to enable."
                            ))
                        }
                    },
                    _ => Err(ChannelError::invalid_input(
                        "usage: /sh [on|off|exit|status]",
                    )),
                }
            },
            _ => Err(ChannelError::invalid_input(format!(
                "unknown command: /{cmd}"
            ))),
        }
    }
}

/// Format a numbered model list, optionally filtered by provider.
///
/// Each line is: `N. DisplayName [provider] *` (where `*` marks the current model).
/// Uses the global index (across all models) so the switch command works with
/// the same numbering regardless of filtering.
fn format_model_list(
    models: &[serde_json::Value],
    current_model: Option<&str>,
    provider_filter: Option<&str>,
) -> String {
    let mut lines = Vec::new();
    for (i, m) in models.iter().enumerate() {
        let id = m.get("id").and_then(|v| v.as_str()).unwrap_or("?");
        let provider = m.get("provider").and_then(|v| v.as_str()).unwrap_or("");
        let display = m.get("displayName").and_then(|v| v.as_str()).unwrap_or(id);
        if let Some(filter) = provider_filter
            && provider != filter
        {
            continue;
        }
        let marker = if current_model == Some(id) {
            " *"
        } else {
            ""
        };
        lines.push(format!("{}. {} [{}]{}", i + 1, display, provider, marker));
    }
    lines.join("\n")
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use {super::*, moltis_channels::ChannelType};

    #[test]
    fn channel_event_serialization() {
        let event = ChannelEvent::InboundMessage {
            channel_type: ChannelType::Telegram,
            account_id: "bot1".into(),
            peer_id: "123".into(),
            username: Some("alice".into()),
            sender_name: Some("Alice".into()),
            message_count: Some(5),
            access_granted: true,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "inbound_message");
        assert_eq!(json["channel_type"], "telegram");
        assert_eq!(json["account_id"], "bot1");
        assert_eq!(json["peer_id"], "123");
        assert_eq!(json["username"], "alice");
        assert_eq!(json["sender_name"], "Alice");
        assert_eq!(json["message_count"], 5);
        assert_eq!(json["access_granted"], true);
    }

    #[test]
    fn channel_session_key_format() {
        let target = ChannelReplyTarget {
            channel_type: ChannelType::Telegram,
            account_id: "bot1".into(),
            chat_id: "12345".into(),
            message_id: None,
        };
        assert_eq!(default_channel_session_key(&target), "telegram:bot1:12345");
    }

    #[test]
    fn channel_session_key_group() {
        let target = ChannelReplyTarget {
            channel_type: ChannelType::Telegram,
            account_id: "bot1".into(),
            chat_id: "-100999".into(),
            message_id: None,
        };
        assert_eq!(
            default_channel_session_key(&target),
            "telegram:bot1:-100999"
        );
    }

    #[test]
    fn channel_event_serialization_nulls() {
        let event = ChannelEvent::InboundMessage {
            channel_type: ChannelType::Telegram,
            account_id: "bot1".into(),
            peer_id: "123".into(),
            username: None,
            sender_name: None,
            message_count: None,
            access_granted: false,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "inbound_message");
        assert!(json["username"].is_null());
        assert_eq!(json["access_granted"], false);
    }

    #[test]
    fn shell_mode_rewrite_plain_text() {
        assert_eq!(
            rewrite_for_shell_mode("uname -a").as_deref(),
            Some("/sh uname -a")
        );
    }

    #[test]
    fn shell_mode_rewrite_skips_control_commands() {
        assert!(rewrite_for_shell_mode("/context").is_none());
        assert!(rewrite_for_shell_mode("/sh uname -a").is_none());
        assert!(rewrite_for_shell_mode("/handoff writer").is_none());
    }

    #[test]
    fn attachment_media_type_classification() {
        assert!(is_image_media_type("image/png"));
        assert!(is_image_media_type(" IMAGE/JPEG "));
        assert!(!is_image_media_type(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        ));
    }

    #[test]
    fn attachment_placeholder_selection() {
        assert_eq!(attachment_placeholder_text(true, false), "[Image]");
        assert_eq!(attachment_placeholder_text(false, true), "[Attachment]");
        assert_eq!(
            attachment_placeholder_text(true, true),
            "[Image + Attachment]"
        );
        assert_eq!(attachment_placeholder_text(false, false), "");
    }

    #[test]
    fn resolve_agent_selector_supports_id_only() {
        let agents = vec![
            crate::agent_persona::AgentPersona {
                id: "main".into(),
                name: "Bob".into(),
                is_default: true,
                aliases: vec![],
                emoji: None,
                theme: None,
                description: None,
                created_at: 0,
                updated_at: 0,
            },
            crate::agent_persona::AgentPersona {
                id: "writer".into(),
                name: "Writer".into(),
                is_default: false,
                aliases: vec!["alice".into()],
                emoji: None,
                theme: None,
                description: None,
                created_at: 0,
                updated_at: 0,
            },
        ];

        assert_eq!(resolve_agent_selector(&agents, "writer").unwrap().id, "writer");
        assert_eq!(resolve_agent_selector(&agents, "WRITER").unwrap().id, "writer");
        assert!(resolve_agent_selector(&agents, "2").is_err());
        assert!(resolve_agent_selector(&agents, "alice").is_err());
        assert!(resolve_agent_selector(&agents, "Bob").is_err());
    }

    #[test]
    fn parse_handoff_args_supports_mode_and_note() {
        let (selector, mode, note) =
            parse_handoff_args("alice --mode fork_session focus on marketing").unwrap();
        assert_eq!(selector, "alice");
        assert_eq!(mode, HandoffMode::ForkSession);
        assert_eq!(note, "focus on marketing");

        let (_, mode2, note2) = parse_handoff_args("writer new_session").unwrap();
        assert_eq!(mode2, HandoffMode::NewSession);
        assert_eq!(note2, "");
    }

    #[test]
    fn parse_sessions_command_args_supports_all_variants() {
        assert_eq!(
            parse_sessions_command_args("").unwrap(),
            SessionsCommand::List
        );
        assert_eq!(
            parse_sessions_command_args("3").unwrap(),
            SessionsCommand::Switch(3)
        );
        assert_eq!(
            parse_sessions_command_args("archive 2").unwrap(),
            SessionsCommand::Archive(2)
        );
        assert_eq!(
            parse_sessions_command_args("unarchive 5").unwrap(),
            SessionsCommand::Unarchive(5)
        );
    }

    #[test]
    fn parse_sessions_command_args_rejects_invalid_inputs() {
        assert!(parse_sessions_command_args("archive").is_err());
        assert!(parse_sessions_command_args("unarchive 0").is_err());
        assert!(parse_sessions_command_args("2 extra").is_err());
    }
}
