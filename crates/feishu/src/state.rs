use std::{
    collections::HashMap,
    sync::{Arc, RwLock, atomic::AtomicBool},
};

use tokio_util::sync::CancellationToken;

use crate::{auth::CachedAccessToken, config::FeishuAccountConfig};

pub type AccountStateMap = Arc<RwLock<HashMap<String, AccountState>>>;

#[derive(Clone)]
pub struct AccountState {
    pub config: FeishuAccountConfig,
    pub cancel: CancellationToken,
    pub http: reqwest::Client,
    pub token_cache: Arc<tokio::sync::Mutex<Option<CachedAccessToken>>>,
    pub bot_open_id: Option<String>,
    pub ws_connected: Arc<AtomicBool>,
}
