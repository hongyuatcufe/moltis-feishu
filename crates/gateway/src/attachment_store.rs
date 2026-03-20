use std::path::{Path, PathBuf};

use {
    anyhow::Result,
    sha2::{Digest, Sha256},
    sqlx::SqlitePool,
};

#[derive(Debug, Clone)]
pub struct StoredAttachment {
    pub blob_sha256: String,
    pub media_type: String,
    pub size_bytes: u64,
    pub original_name: String,
    pub relative_path: String,
    pub absolute_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct SaveChannelAttachment<'a> {
    pub session_key: &'a str,
    pub channel_type: &'a str,
    pub account_id: &'a str,
    pub chat_id: &'a str,
    pub message_id: Option<&'a str>,
    pub media_type: &'a str,
    pub original_name: Option<&'a str>,
    pub data: &'a [u8],
}

pub struct AttachmentStore {
    pool: SqlitePool,
    base_dir: PathBuf,
}

#[derive(sqlx::FromRow)]
struct BlobRow {
    storage_path: String,
}

impl AttachmentStore {
    pub fn new(pool: SqlitePool, base_dir: PathBuf) -> Self {
        Self { pool, base_dir }
    }

    pub async fn save_channel_attachment(
        &self,
        req: SaveChannelAttachment<'_>,
    ) -> Result<StoredAttachment> {
        let now = now_ms();
        let mut hasher = Sha256::new();
        hasher.update(req.data);
        let blob_sha256 = format!("{:x}", hasher.finalize());

        let ext = infer_extension(req.media_type, req.original_name);
        let dir = format!("attachments/blobs/{}", &blob_sha256[..2]);
        let candidate_rel_path = format!("{dir}/{blob_sha256}.{ext}");
        let candidate_abs_path = self.base_dir.join(&candidate_rel_path);
        let mut stored_rel_path = candidate_rel_path.clone();

        let existing_blob = sqlx::query_as::<_, BlobRow>(
            "SELECT storage_path FROM attachment_blobs WHERE sha256 = ?",
        )
        .bind(&blob_sha256)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(existing) = existing_blob {
            stored_rel_path = existing.storage_path;
            let existing_abs = self.base_dir.join(&stored_rel_path);
            if tokio::fs::metadata(&existing_abs).await.is_err() {
                write_blob_file(&existing_abs, req.data).await?;
            }
            sqlx::query("UPDATE attachment_blobs SET last_accessed_at = ? WHERE sha256 = ?")
                .bind(now)
                .bind(&blob_sha256)
                .execute(&self.pool)
                .await?;
        } else {
            write_blob_file(&candidate_abs_path, req.data).await?;
            sqlx::query(
                "INSERT INTO attachment_blobs (sha256, media_type, ext, size_bytes, storage_path, created_at, last_accessed_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(sha256) DO UPDATE SET
                   last_accessed_at = excluded.last_accessed_at",
            )
            .bind(&blob_sha256)
            .bind(req.media_type)
            .bind(&ext)
            .bind(req.data.len() as i64)
            .bind(&candidate_rel_path)
            .bind(now)
            .bind(now)
            .execute(&self.pool)
            .await?;

            if let Some(row) = sqlx::query_as::<_, BlobRow>(
                "SELECT storage_path FROM attachment_blobs WHERE sha256 = ?",
            )
            .bind(&blob_sha256)
            .fetch_optional(&self.pool)
            .await?
            {
                stored_rel_path = row.storage_path;
            }

            if stored_rel_path != candidate_rel_path {
                let stored_abs = self.base_dir.join(&stored_rel_path);
                if tokio::fs::metadata(&stored_abs).await.is_err() {
                    write_blob_file(&stored_abs, req.data).await?;
                }
                let _ = tokio::fs::remove_file(&candidate_abs_path).await;
            }
        }

        let stored_ext = Path::new(&stored_rel_path)
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or(ext.as_str());
        let original_name = sanitize_original_name(req.original_name, &blob_sha256, stored_ext);
        sqlx::query(
            "INSERT INTO attachment_refs
                (id, session_key, channel_type, account_id, chat_id, message_id, blob_sha256, original_name, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(req.session_key)
        .bind(req.channel_type)
        .bind(req.account_id)
        .bind(req.chat_id)
        .bind(req.message_id)
        .bind(&blob_sha256)
        .bind(&original_name)
        .bind(now)
        .execute(&self.pool)
        .await?;

        Ok(StoredAttachment {
            blob_sha256,
            media_type: req.media_type.to_string(),
            size_bytes: req.data.len() as u64,
            original_name,
            relative_path: stored_rel_path.clone(),
            absolute_path: self.base_dir.join(stored_rel_path),
        })
    }
}

async fn write_blob_file(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("attachment path has no parent: {}", path.display()))?;
    tokio::fs::create_dir_all(parent).await?;
    tokio::fs::write(path, data).await?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn infer_extension(media_type: &str, original_name: Option<&str>) -> String {
    if let Some(ext) = original_name
        .map(Path::new)
        .and_then(Path::extension)
        .and_then(|e| e.to_str())
    {
        let lower = ext.trim().to_ascii_lowercase();
        if !lower.is_empty()
            && lower.len() <= 12
            && lower.chars().all(|ch| ch.is_ascii_alphanumeric())
        {
            return lower;
        }
    }
    match media_type.to_ascii_lowercase().as_str() {
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            "docx".to_string()
        },
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx".to_string(),
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            "pptx".to_string()
        },
        "application/msword" => "doc".to_string(),
        "application/vnd.ms-excel" => "xls".to_string(),
        "application/pdf" => "pdf".to_string(),
        "text/plain" => "txt".to_string(),
        "text/csv" => "csv".to_string(),
        "application/json" => "json".to_string(),
        "application/zip" => "zip".to_string(),
        "image/png" => "png".to_string(),
        "image/jpeg" => "jpg".to_string(),
        "image/webp" => "webp".to_string(),
        "image/gif" => "gif".to_string(),
        _ => "bin".to_string(),
    }
}

fn sanitize_original_name(raw_name: Option<&str>, blob_sha256: &str, ext: &str) -> String {
    if let Some(name) = raw_name {
        let trimmed = name.trim();
        if !trimmed.is_empty() {
            let replaced = trimmed
                .chars()
                .map(|c| {
                    if c == '/' || c == '\\' || c.is_control() {
                        '_'
                    } else {
                        c
                    }
                })
                .collect::<String>();
            if !replaced.is_empty() {
                return replaced;
            }
        }
    }
    format!("attachment-{}.{}", &blob_sha256[..12], ext)
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use super::*;

    async fn init_for_tests(pool: &SqlitePool) {
        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS attachment_blobs (
                sha256            TEXT PRIMARY KEY,
                media_type        TEXT NOT NULL,
                ext               TEXT NOT NULL,
                size_bytes        INTEGER NOT NULL,
                storage_path      TEXT NOT NULL,
                created_at        INTEGER NOT NULL,
                last_accessed_at  INTEGER NOT NULL
            )"#,
        )
        .execute(pool)
        .await
        .unwrap();

        sqlx::query(
            r#"CREATE TABLE IF NOT EXISTS attachment_refs (
                id            TEXT PRIMARY KEY,
                session_key   TEXT NOT NULL,
                channel_type  TEXT NOT NULL,
                account_id    TEXT NOT NULL,
                chat_id       TEXT NOT NULL,
                message_id    TEXT,
                blob_sha256   TEXT NOT NULL,
                original_name TEXT,
                created_at    INTEGER NOT NULL
            )"#,
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn deduplicates_blob_and_records_refs() {
        let dir = tempfile::tempdir().unwrap();
        let pool = SqlitePool::connect("sqlite::memory:").await.unwrap();
        init_for_tests(&pool).await;
        let store = AttachmentStore::new(pool.clone(), dir.path().to_path_buf());
        let bytes = b"docx-binary";

        let first = store
            .save_channel_attachment(SaveChannelAttachment {
                session_key: "session:a",
                channel_type: "feishu",
                account_id: "main-bot",
                chat_id: "oc_123",
                message_id: Some("om_1"),
                media_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                original_name: Some("plan.docx"),
                data: bytes,
            })
            .await
            .unwrap();
        let second = store
            .save_channel_attachment(SaveChannelAttachment {
                session_key: "session:b",
                channel_type: "feishu",
                account_id: "main-bot",
                chat_id: "oc_123",
                message_id: Some("om_2"),
                media_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                original_name: Some("another.docx"),
                data: bytes,
            })
            .await
            .unwrap();

        assert_eq!(first.blob_sha256, second.blob_sha256);
        assert_eq!(first.relative_path, second.relative_path);
        assert!(first.absolute_path.exists());

        let blob_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attachment_blobs")
            .fetch_one(&pool)
            .await
            .unwrap();
        let ref_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM attachment_refs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(blob_count, 1);
        assert_eq!(ref_count, 2);
    }
}
