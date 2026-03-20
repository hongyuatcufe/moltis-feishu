# Moltis Feishu Fork

这个仓库是 `moltis` 的定制分支，面向飞书接入、中文检索、Tavily 联网搜索和多 Agent 会话协作。

This repository is a custom `moltis` fork focused on Feishu integration, Chinese web search, Tavily-based web search, and multi-agent session workflows.

## Upstream Base / 上游基线

- 基于 `moltis` 上游代码。
- 已合入 `v0.10.18` 及之后的部分 `upstream/main` 更新。
- 默认分支：`main`

- Based on upstream `moltis`.
- Includes `v0.10.18` and later updates merged from `upstream/main`.
- Default branch: `main`

## What This Fork Adds / 本分支新增能力

- 飞书 WebSocket 长连接接入，不需要 pairing。
- Feishu WebSocket long-lived connection without pairing.

- 飞书文本、图片、文件消息接入与发送。
- Feishu text, image, and file messaging in both directions.

- Feishu 附件统一落盘到 `~/.moltis/attachments/blobs/`，并保留 `original_name`。
- Feishu attachments are persisted under `~/.moltis/attachments/blobs/` with `original_name` preserved.

- 多 Agent 会话切换：`/agent <id>`。
- Multi-agent session switching: `/agent <id>`.

- handoff 流程：`/handoff <id> [note]`，固定为“新建隔离 session + 摘要交接 + 首轮一次性消费”。
- Handoff workflow: `/handoff <id> [note]`, always using “new isolated session + sanitized summary + one-shot first-turn context”.

- 会话治理：`/sessions archive N`、`/sessions unarchive N`、`session_auto_archive_days` 自动归档。
- Session management: `/sessions archive N`, `/sessions unarchive N`, and automatic archiving via `session_auto_archive_days`.

- `web_cn_search` 工具：统一封装 Metaso、Bocha、Anspire、Jina。
- `web_cn_search`: unified wrapper for Metaso, Bocha, Anspire, and Jina.

- `web_search` 支持 Tavily，可作为通用联网检索入口。
- `web_search` supports Tavily as a configurable general web-search provider.

- `web_read` 工具：Jina、Metaso、Crawl4AI、PinchTab 回退链路。
- `web_read`: fallback chain across Jina, Metaso, Crawl4AI, and PinchTab.

- `web_fetch` 已修复中文页面抓取中的 GBK/GB18030 与 UTF-8 乱码问题。
- `web_fetch` now handles Chinese pages more reliably, including GBK/GB18030 and UTF-8 decoding paths.

- provider 缺 key 或单项配置错误时按组件降级，不阻塞其他已配置 provider。
- Missing API keys or invalid provider-specific config degrade per component instead of blocking the rest.

## Current Status / 当前状态

- `/agent` 和 `/handoff` 目前只接受 agent id，不再使用 alias 作为用户侧入口。
- `/agent` and `/handoff` currently accept agent ids only; aliases are no longer the user-facing selector.

- Feishu 出站语音回复链路已接入；入站飞书语音转写当前分支未启用。
- Outbound Feishu voice replies are wired up; inbound Feishu voice transcription is not currently enabled on this branch.

## Install From This Branch / 从本分支安装

### Recommended: Clone And Build / 推荐：克隆源码构建

```bash
git clone https://github.com/hongyuatcufe/moltis-feishu.git
cd moltis-feishu
cd crates/web/ui
./build.sh
cd ../../..
cargo build --release
./target/release/moltis
```

说明：

- 首次源码构建前需要先生成 Web UI 资源 `crates/web/src/assets/style.css`
- 因此当前分支不建议直接使用 `cargo install --git ...`

安装后运行：

After installation:

```bash
moltis
```

## Config Example / 配置示例

示例配置文件：

Example config file:

- `examples/moltis.toml.example`

推荐做法：

Recommended workflow:

1. 复制 `examples/moltis.toml.example` 到 `~/.config/moltis/moltis.toml`
2. 填入你自己的 Feishu、Tavily、Metaso、Bocha、Anspire、Jina 等密钥
3. 按需启用或关闭各 provider

1. Copy `examples/moltis.toml.example` to `~/.config/moltis/moltis.toml`
2. Fill in your Feishu, Tavily, Metaso, Bocha, Anspire, Jina, and related secrets
3. Enable or disable providers as needed

## Minimal Config / 最小配置

### Feishu

```toml
[channels.feishu.main-bot]
app_id = "cli_xxxxxxxxxxxxx"
app_secret = "xxxxxxxxxxxxxxxx"
base_url = "https://open.feishu.cn"
agent_id = "main"
allow_agent_switch = true
session_auto_archive_days = 30
```

### `web_cn_search`

至少配置一个 provider：

Configure at least one provider:

```toml
[tools.web.cn_search]
enabled = true

[tools.web.cn_search.bocha]
enabled = true

[[tools.web.cn_search.bocha.accounts]]
name = "main"
api_key = "YOUR_BOCHA_API_KEY"
enabled = true
```

### `web_search` with Tavily

```toml
[tools.web.search]
enabled = true
provider = "tavily"
max_results = 5
timeout_seconds = 30
cache_ttl_minutes = 15
duckduckgo_fallback = false

[tools.web.search.tavily]
api_key = "YOUR_TAVILY_API_KEY"
search_depth = "advanced"
include_answer = true
include_domains = []
exclude_domains = []
```

### `web_fetch`

```toml
[tools.web.fetch]
enabled = true
max_chars = 50000
timeout_seconds = 30
cache_ttl_minutes = 15
max_redirects = 3
readability = true
```

### `web_read`

```toml
[tools.web.read]
enabled = true

[tools.web.read.jina]
enabled = true

[[tools.web.read.jina.accounts]]
name = "main"
api_key = "YOUR_JINA_API_KEY"
enabled = true
```

### Supported Environment Variables / 支持的环境变量

- `METASO_API_KEY`
- `BOCHA_API_KEY`
- `ANSPIRE_API_KEY`
- `JINA_API_KEY`
- `TAVILY_API_KEY`

## Startup And Verification / 启动与验证

先检查配置：

Validate config first:

```bash
cargo run -- config check
```

然后启动：

Then start the server:

```bash
cargo run --release
```

建议的回归检查：

Suggested smoke checks:

1. 飞书文本消息是否正常收发
2. 飞书图片或文件上传后，是否出现本地附件路径提示
3. `/agent writer` 和 `/handoff writer 请继续处理` 是否生效
4. `web_cn_search` 是否能在 provider key 正确时返回结果
5. `web_search` 是否能通过 Tavily 返回结果
6. `web_read` 是否能读取指定 URL
7. `web_fetch` 是否能正确抓取中文页面全文而不乱码

1. Verify Feishu text messages work both ways
2. Upload an image or file and confirm the local attachment path appears
3. Verify `/agent writer` and `/handoff writer please continue`
4. Confirm `web_cn_search` returns results when provider keys are valid
5. Confirm `web_search` returns results through Tavily
6. Confirm `web_read` can read a target URL
7. Confirm `web_fetch` can fetch a Chinese page without mojibake

如果 `config check` 报本地旧配置字段错误，请先清理已废弃字段，再重新检查。

If `config check` reports unknown fields from an older local config, remove the stale fields and run it again.

## Security / 安全说明

- 不要提交真实密钥、令牌或本地配置文件。
- 不要提交 `~/.config/moltis/moltis.toml` 或 `provider_keys.json`。
- 只提交代码、迁移、README 和示例配置。

- Do not commit real secrets, tokens, or local config files.
- Do not commit `~/.config/moltis/moltis.toml` or `provider_keys.json`.
- Commit code, migrations, README, and example config only.
