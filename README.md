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

- `web_read` 工具：Jina、Metaso、spider 回退链路。
- `web_read`: fallback chain across Jina, Metaso, and spider.

- `web_fetch` 已修复中文页面抓取中的 GBK/GB18030 与 UTF-8 乱码问题。
- `web_fetch` now handles Chinese pages more reliably, including GBK/GB18030 and UTF-8 decoding paths.

- 工具分工更明确：`web_cn_search` 用于搜中文结果，`web_read` 用于读全文，`web_fetch` 用于直接抓取指定 URL。
- Tool responsibilities are explicit: `web_cn_search` is for Chinese search results, `web_read` is for full-text reading, and `web_fetch` is for direct fetching of a known URL.

- provider 缺 key 或单项配置错误时按组件降级，不阻塞其他已配置 provider。
- Missing API keys or invalid provider-specific config degrade per component instead of blocking the rest.

## Current Status / 当前状态

- `/agent` 和 `/handoff` 目前只接受 agent id，不再使用 alias 作为用户侧入口。
- `/agent` and `/handoff` currently accept agent ids only; aliases are no longer the user-facing selector.

- Feishu 出站语音回复链路已接入；入站飞书语音转写当前分支未启用。
- Outbound Feishu voice replies are wired up; inbound Feishu voice transcription is not currently enabled on this branch.

## Build From Source / 从源码构建

### Recommended: Clone And Build / 推荐：克隆源码构建

```bash
git clone https://github.com/hongyuatcufe/moltis-feishu.git
cd moltis-feishu
rustup toolchain install nightly-2025-11-30
cd crates/web/ui
./build.sh
cd ../../..
cargo build --release
./target/release/moltis
```

说明：

- 本仓库固定使用 `nightly-2025-11-30`
- `./build.sh` 会生成 Web UI 资源 `crates/web/src/assets/css/style.css`
- 本地源码构建完成后的可执行文件是 `./target/release/moltis`
- 因此当前仓库不建议直接使用 `cargo install --git ...`

如果你想安装到 `PATH`：

If you want to install it into your `PATH`:

```bash
cargo install --path crates/cli --force
```

本地源码构建后运行：

Run from the local build:

```bash
./target/release/moltis
```

## Download Prebuilt Binaries / 下载预构建二进制

如果你不想在本地编译，可以直接从 GitHub Releases 下载预构建二进制。

If you do not want to build locally, download a prebuilt binary from GitHub Releases:

- `https://github.com/hongyuatcufe/moltis-feishu/releases`

当前计划提供这些目标：

Planned targets:

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`
- `aarch64-unknown-linux-gnu`
- `x86_64-unknown-linux-gnu`

资产命名格式：

Asset naming format:

- `moltis-<VERSION>-<TARGET>.tar.gz`
- `moltis-<VERSION>-<TARGET>.tar.gz.sha256`

安装示例（macOS/Linux）：

Example install flow (macOS/Linux):

```bash
VERSION=20260320.01
TARGET=aarch64-apple-darwin

curl -LO "https://github.com/hongyuatcufe/moltis-feishu/releases/download/${VERSION}/moltis-${VERSION}-${TARGET}.tar.gz"
curl -LO "https://github.com/hongyuatcufe/moltis-feishu/releases/download/${VERSION}/moltis-${VERSION}-${TARGET}.tar.gz.sha256"
shasum -a 256 -c "moltis-${VERSION}-${TARGET}.tar.gz.sha256"
tar xzf "moltis-${VERSION}-${TARGET}.tar.gz"
cd "moltis-${VERSION}-${TARGET}"
cp moltis.toml.example ~/.config/moltis/moltis.toml
./moltis
```

说明：

- 这套二进制发布会用 `embedded-assets` 和 `embedded-wasm` 构建
- 解压后可直接运行，不依赖额外的 `share/moltis/` 目录
- 压缩包内包含 `moltis.toml.example`，可直接作为起始配置模板
- 配置文件仍默认读取 `~/.config/moltis/moltis.toml`

Notes:

- These binaries are built with `embedded-assets` and `embedded-wasm`
- You can run them directly after extracting the archive
- Each archive includes `moltis.toml.example` as a starting config template
- Config still defaults to `~/.config/moltis/moltis.toml`

### Maintainer Release Flow / 维护者发布流程

仓库内置了轻量发布 workflow：

The repository includes a lightweight release workflow:

- `.github/workflows/binary-release.yml`

用法：

Usage:

1. 打开 GitHub Actions
2. 选择 `Binary Release`
3. 输入版本号，例如 `20260320.01`
4. 选择是否以 draft 形式创建 release

1. Open GitHub Actions
2. Select `Binary Release`
3. Enter a version such as `20260320.01`
4. Choose whether to create the release as a draft

这条 workflow 会：

This workflow will:

1. 构建 macOS 与 Linux 的 4 个目标二进制
2. 生成 `.tar.gz` 和 `.sha256`
3. 自动创建或更新同名 GitHub Release

1. Build 4 macOS/Linux targets
2. Generate `.tar.gz` and `.sha256`
3. Create or update the matching GitHub Release automatically

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

适合“先搜到结果”：

Use this when you need search results first:

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

适合通用联网搜索，不限定中文站点：

Use this for general web search, not limited to Chinese sites:

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

适合“我已经知道目标 URL，只想直接抓页面内容”：

Use this when you already know the target URL and want to fetch that page directly:

```toml
[tools.web.fetch]
enabled = true
max_chars = 50000
timeout_seconds = 30
cache_ttl_minutes = 15
max_redirects = 3
readability = true
```

说明：

- `web_fetch` 直接请求原网页并提取内容
- 适合已知 URL 的轻量抓取
- 当前分支已修复常见中文网页的乱码问题

Notes:

- `web_fetch` requests the original page directly and extracts content
- Best for lightweight fetches of a known URL
- This branch includes fixes for common Chinese-page encoding issues

### `web_read`

适合“我要正文/全文，不只是搜索结果摘要”：

Use this when you want the main body or full text, not just search-result snippets:

```toml
[tools.web.read]
enabled = true

[tools.web.read.jina]
enabled = true

[[tools.web.read.jina.accounts]]
name = "main"
api_key = "YOUR_JINA_API_KEY"
enabled = true

[tools.web.read.metaso]
enabled = true

[[tools.web.read.metaso.accounts]]
name = "main"
api_key = "YOUR_METASO_API_KEY"
enabled = true

[tools.web.read.spider]
enabled = true
timeout_seconds = 20
```

说明：

- `web_read` 是全文读取器，不只是搜索工具
- 当前支持 `jina`、`metaso`、`spider`
- 推荐把 `jina` 或 `metaso` 作为首选全文读取后端，`spider` 作为本地 Rust 兜底
- 如果你已经有 URL 并且只想简单抓取，可优先试 `web_fetch`

Notes:

- `web_read` is a full-text reader, not just a search tool
- Supported backends: `jina`, `metaso`, `spider`
- Prefer `jina` or `metaso` as primary full-text backends, with `spider` as the local Rust fallback
- If you already have a URL and only need a simple fetch, try `web_fetch` first

### Which Tool To Use / 该用哪个工具

1. 先搜中文网页结果：`web_cn_search`
2. 先做通用联网搜索：`web_search`
3. 已知 URL，直接抓页面：`web_fetch`
4. 需要正文或全文读取：`web_read`

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
./target/release/moltis config check
```

然后启动：

Then start the server:

```bash
./target/release/moltis
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
