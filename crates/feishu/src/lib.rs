//! Feishu channel plugin.

mod auth;
mod config;
mod outbound;
mod plugin;
mod state;
mod ws;
mod ws_frame;

pub use {config::FeishuAccountConfig, plugin::FeishuPlugin};
