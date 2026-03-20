use {async_trait::async_trait, base64::Engine, secrecy::ExposeSecret, tracing::debug};

use {
    moltis_channels::{
        Error as ChannelError, Result as ChannelResult,
        plugin::{ChannelOutbound, ChannelStreamOutbound, StreamEvent, StreamReceiver},
    },
    moltis_common::types::{MediaAttachment, ReplyPayload},
};

use crate::{auth::get_access_token, config::FeishuAccountConfig, state::AccountStateMap};

/// Outbound sender for Feishu channel accounts.
pub struct FeishuOutbound {
    pub(crate) accounts: AccountStateMap,
}

struct AccountSnapshot {
    config: FeishuAccountConfig,
    http: reqwest::Client,
    token_cache: std::sync::Arc<tokio::sync::Mutex<Option<crate::auth::CachedAccessToken>>>,
}

impl FeishuOutbound {
    fn account_snapshot(&self, account_id: &str) -> ChannelResult<AccountSnapshot> {
        let accounts = self.accounts.read().unwrap_or_else(|e| e.into_inner());
        let state = accounts
            .get(account_id)
            .ok_or_else(|| ChannelError::unknown_account(account_id))?;
        Ok(AccountSnapshot {
            config: state.config.clone(),
            http: state.http.clone(),
            token_cache: std::sync::Arc::clone(&state.token_cache),
        })
    }

    async fn send_message(
        &self,
        account_id: &str,
        receive_id: &str,
        msg_type: &str,
        content: serde_json::Value,
    ) -> ChannelResult<()> {
        let snapshot = self.account_snapshot(account_id)?;
        let token = get_access_token(&snapshot.http, &snapshot.config, &snapshot.token_cache)
            .await
            .map_err(|e| ChannelError::unavailable(format!("Feishu token acquisition: {e}")))?;
        let url = format!(
            "{}/open-apis/im/v1/messages?receive_id_type=chat_id",
            snapshot.config.base_url.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "receive_id": receive_id,
            "msg_type": msg_type,
            "content": content.to_string(),
        });
        let resp = snapshot
            .http
            .post(url)
            .bearer_auth(token.expose_secret())
            .json(&body)
            .send()
            .await
            .map_err(|e| ChannelError::external("Feishu HTTP send", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ChannelError::external(
                "Feishu send failed",
                std::io::Error::other(format!("{status}: {text}")),
            ));
        }
        Ok(())
    }

    async fn upload_image(
        snapshot: &AccountSnapshot,
        media: &MediaAttachment,
    ) -> ChannelResult<String> {
        let token = get_access_token(&snapshot.http, &snapshot.config, &snapshot.token_cache)
            .await
            .map_err(|e| ChannelError::unavailable(format!("Feishu token acquisition: {e}")))?;
        let (bytes, filename, mime) = fetch_media_bytes(&snapshot.http, media).await?;
        let form = reqwest::multipart::Form::new()
            .text("image_type", "message")
            .part(
                "image",
                reqwest::multipart::Part::bytes(bytes)
                    .file_name(filename)
                    .mime_str(&mime)
                    .map_err(|e| ChannelError::invalid_input(format!("invalid mime: {e}")))?,
            );
        let url = format!(
            "{}/open-apis/im/v1/images",
            snapshot.config.base_url.trim_end_matches('/')
        );
        let resp = snapshot
            .http
            .post(url)
            .bearer_auth(token.expose_secret())
            .multipart(form)
            .send()
            .await
            .map_err(|e| ChannelError::external("Feishu upload image", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ChannelError::external(
                "Feishu upload image failed",
                std::io::Error::other(format!("{status}: {text}")),
            ));
        }
        let data: serde_json::Value = resp.json().await.unwrap_or_default();
        data.get("data")
            .and_then(|d| d.get("image_key"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| ChannelError::unavailable("missing image_key"))
    }

    async fn upload_file(
        snapshot: &AccountSnapshot,
        media: &MediaAttachment,
        file_type: &str,
    ) -> ChannelResult<String> {
        let token = get_access_token(&snapshot.http, &snapshot.config, &snapshot.token_cache)
            .await
            .map_err(|e| ChannelError::unavailable(format!("Feishu token acquisition: {e}")))?;
        let (bytes, filename, mime) = fetch_media_bytes(&snapshot.http, media).await?;
        let form = reqwest::multipart::Form::new()
            .text("file_type", file_type.to_string())
            .text("file_name", filename.clone())
            .part(
                "file",
                reqwest::multipart::Part::bytes(bytes)
                    .file_name(filename)
                    .mime_str(&mime)
                    .map_err(|e| ChannelError::invalid_input(format!("invalid mime: {e}")))?,
            );
        let url = format!(
            "{}/open-apis/im/v1/files",
            snapshot.config.base_url.trim_end_matches('/')
        );
        let resp = snapshot
            .http
            .post(url)
            .bearer_auth(token.expose_secret())
            .multipart(form)
            .send()
            .await
            .map_err(|e| ChannelError::external("Feishu upload file", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(ChannelError::external(
                "Feishu upload file failed",
                std::io::Error::other(format!("{status}: {text}")),
            ));
        }
        let data: serde_json::Value = resp.json().await.unwrap_or_default();
        data.get("data")
            .and_then(|d| d.get("file_key"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| ChannelError::unavailable("missing file_key"))
    }
}

#[async_trait]
impl ChannelOutbound for FeishuOutbound {
    async fn send_text(
        &self,
        account_id: &str,
        to: &str,
        text: &str,
        _reply_to: Option<&str>,
    ) -> ChannelResult<()> {
        debug!(account_id, to, text_len = text.len(), "sending feishu text");
        self.send_message(account_id, to, "text", serde_json::json!({"text": text}))
            .await
    }

    async fn send_media(
        &self,
        account_id: &str,
        to: &str,
        payload: &ReplyPayload,
        reply_to: Option<&str>,
    ) -> ChannelResult<()> {
        if let Some(media) = payload.media.as_ref() {
            let snapshot = self.account_snapshot(account_id)?;
            if media.mime_type.starts_with("image/") {
                let image_key = Self::upload_image(&snapshot, media).await?;
                let content = serde_json::json!({ "image_key": image_key });
                self.send_message(account_id, to, "image", content).await?;
            } else {
                let file_key = Self::upload_file(&snapshot, media, "stream").await?;
                let content = serde_json::json!({
                    "file_key": file_key,
                    "file_name": filename_from_media(
                        media,
                        if media.mime_type.starts_with("audio/") {
                            "audio"
                        } else {
                            "file"
                        },
                    ),
                });
                self.send_message(account_id, to, "file", content).await?;
            }
        }
        if !payload.text.is_empty() {
            self.send_text(account_id, to, &payload.text, reply_to)
                .await?;
        }
        Ok(())
    }

    async fn send_typing(&self, _account_id: &str, _to: &str) -> ChannelResult<()> {
        Ok(())
    }

    async fn send_text_with_suffix(
        &self,
        account_id: &str,
        to: &str,
        text: &str,
        suffix_html: &str,
        reply_to: Option<&str>,
    ) -> ChannelResult<()> {
        let mut merged = text.to_string();
        if !suffix_html.is_empty() {
            merged.push_str("\n\n");
            merged.push_str(suffix_html);
        }
        self.send_text(account_id, to, &merged, reply_to).await
    }

    async fn send_html(
        &self,
        account_id: &str,
        to: &str,
        html: &str,
        reply_to: Option<&str>,
    ) -> ChannelResult<()> {
        self.send_text(account_id, to, html, reply_to).await
    }

    async fn send_location(
        &self,
        account_id: &str,
        to: &str,
        latitude: f64,
        longitude: f64,
        title: Option<&str>,
        reply_to: Option<&str>,
    ) -> ChannelResult<()> {
        let mut text = String::new();
        if let Some(title) = title {
            text.push_str(title);
            text.push('\n');
        }
        text.push_str(&format!(
            "https://www.google.com/maps?q={latitude:.6},{longitude:.6}"
        ));
        self.send_text(account_id, to, &text, reply_to).await
    }
}

#[async_trait]
impl ChannelStreamOutbound for FeishuOutbound {
    async fn send_stream(
        &self,
        account_id: &str,
        to: &str,
        reply_to: Option<&str>,
        mut stream: StreamReceiver,
    ) -> ChannelResult<()> {
        let mut text = String::new();
        while let Some(event) = stream.recv().await {
            match event {
                StreamEvent::Delta(delta) => text.push_str(&delta),
                StreamEvent::Done => break,
                StreamEvent::Error(err) => {
                    debug!(account_id, chat_id = to, "Feishu stream error: {err}");
                    if text.is_empty() {
                        text = err;
                    }
                    break;
                },
            }
        }
        if text.is_empty() {
            return Ok(());
        }
        self.send_text(account_id, to, &text, reply_to).await
    }

    async fn is_stream_enabled(&self, _account_id: &str) -> bool {
        false
    }
}

async fn fetch_media_bytes(
    http: &reqwest::Client,
    media: &MediaAttachment,
) -> ChannelResult<(Vec<u8>, String, String)> {
    if let Some((mime, data)) = parse_data_url(&media.url) {
        let filename = extension_from_mime(&mime)
            .map(|ext| format!("upload.{ext}"))
            .unwrap_or_else(|| "upload".to_string());
        return Ok((data, filename, mime));
    }
    let resp = http
        .get(&media.url)
        .send()
        .await
        .map_err(|e| ChannelError::external("Feishu media fetch", e))?;
    if !resp.status().is_success() {
        return Err(ChannelError::unavailable(format!(
            "media fetch HTTP {}",
            resp.status()
        )));
    }
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or(&media.mime_type)
        .to_string();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| ChannelError::external("Feishu media fetch", e))?;
    let filename = filename_from_url(&media.url)
        .or_else(|| extension_from_mime(&mime).map(|ext| format!("upload.{ext}")))
        .unwrap_or_else(|| "upload".to_string());
    Ok((bytes.to_vec(), filename, mime))
}

fn parse_data_url(url: &str) -> Option<(String, Vec<u8>)> {
    if !url.starts_with("data:") {
        return None;
    }
    let parts: Vec<&str> = url.splitn(2, ',').collect();
    if parts.len() != 2 {
        return None;
    }
    let meta = parts[0];
    let data = parts[1];
    let mime = meta
        .trim_start_matches("data:")
        .split(';')
        .next()
        .unwrap_or("application/octet-stream")
        .to_string();
    let bytes = if meta.ends_with(";base64") {
        base64::engine::general_purpose::STANDARD
            .decode(data)
            .ok()?
    } else {
        urlencoding::decode_binary(data.as_bytes()).to_vec()
    };
    Some((mime, bytes))
}

fn filename_from_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let seg = parsed.path_segments()?.last()?;
    if seg.is_empty() {
        None
    } else {
        Some(seg.to_string())
    }
}

fn extension_from_mime(mime_type: &str) -> Option<&'static str> {
    match mime_type.to_ascii_lowercase().as_str() {
        "audio/ogg" | "audio/opus" => Some("ogg"),
        "audio/mpeg" | "audio/mp3" => Some("mp3"),
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac" => Some("m4a"),
        "audio/wav" | "audio/x-wav" | "audio/pcm" => Some("wav"),
        "audio/webm" => Some("webm"),
        "application/pdf" => Some("pdf"),
        "image/png" => Some("png"),
        "image/jpeg" | "image/jpg" => Some("jpg"),
        _ => None,
    }
}

fn filename_from_media(media: &MediaAttachment, fallback_stem: &str) -> String {
    if let Some(name) = filename_from_url(&media.url) {
        return name;
    }

    if let Some(ext) = extension_from_mime(&media.mime_type) {
        return format!("{fallback_stem}.{ext}");
    }

    fallback_stem.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_from_media_prefers_url_filename() {
        let media = MediaAttachment {
            url: "https://example.com/path/voice-note.ogg".to_string(),
            mime_type: "audio/ogg".to_string(),
        };
        assert_eq!(filename_from_media(&media, "audio"), "voice-note.ogg");
    }

    #[test]
    fn filename_from_media_falls_back_to_mime_extension() {
        let media = MediaAttachment {
            url: "data:audio/webm;base64,AAA".to_string(),
            mime_type: "audio/webm".to_string(),
        };
        assert_eq!(filename_from_media(&media, "audio"), "audio.webm");
    }

    #[test]
    fn filename_from_media_falls_back_to_stem() {
        let media = MediaAttachment {
            url: "data:application/octet-stream;base64,AAA".to_string(),
            mime_type: "application/octet-stream".to_string(),
        };
        assert_eq!(filename_from_media(&media, "file"), "file");
    }

    #[tokio::test]
    async fn fetch_media_bytes_data_url_uses_mime_extension() {
        let media = MediaAttachment {
            url: "data:audio/ogg;base64,T2dnUw==".to_string(),
            mime_type: "audio/ogg".to_string(),
        };
        let result = fetch_media_bytes(&reqwest::Client::new(), &media).await;
        assert!(result.is_ok());
        let (bytes, filename, mime) = result.unwrap_or_else(|_| unreachable!());
        assert_eq!(bytes, b"OggS");
        assert_eq!(filename, "upload.ogg");
        assert_eq!(mime, "audio/ogg");
    }
}
