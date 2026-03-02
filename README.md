# Moltis Feishu Fork

本仓库是基于 `moltis v0.9.10` 的定制分支，面向飞书与中文检索场景。

## What This Fork Adds / 本分支新增能力

- Feishu WebSocket 长连接接入（无 pairing）。
- Feishu 收发文本、图片、文件、语音（入站 STT + 出站 TTS）。
- 多 Agent 会话控制：`/agent`、`/handoff`、会话切换与继承模式。
- 会话治理：`/sessions archive N`、`/sessions unarchive N`、自动归档。
- `web_cn_search` 工具：统一封装 Metaso / Bocha / Anspire / Jina。
- `web_read` 工具：Jina / Metaso / Crawl4AI / Pinchtab 回退链路。
- 附件集中存储（blob）与 `original_name` 贯通。

## Install From This Fork / 从本分支安装

### 方式 1：本地源码构建

```bash
git clone https://github.com/hongyuatcufe/moltis-feishu.git
cd moltis-feishu
git checkout feat/feishu-cn-tools-release
cargo build --release
./target/release/moltis
```

### 方式 2：直接用 cargo 安装指定分支

```bash
cargo install --git https://github.com/hongyuatcufe/moltis-feishu.git --branch feat/feishu-cn-tools-release moltis
```

安装后运行：

```bash
moltis
```

## Config Example / 配置示例

示例文件：`examples/moltis.feishu-cn-tools.example.toml`

建议操作：

- 复制示例到你的本地配置文件路径（例如 `~/.config/moltis/moltis.toml`）。
- 仅替换你自己的密钥：`app_id`、`app_secret`、`METASO/BOCHA/ANSPIRE/JINA` 等 API Key。
- 根据需要打开或关闭 provider：`enabled = true/false`。

## Minimal Required Config / 最小必需配置

- Feishu：
- `channels.feishu.<account>.app_id`
- `channels.feishu.<account>.app_secret`

- `web_cn_search`（至少一个 provider 的 key）：
- `tools.web.cn_search.metaso` 或 `bocha` 或 `anspire` 或 `jina`

## Run & Verify / 启动与验证

```bash
cargo run --release
```

建议先做：

```bash
cargo run -- config check
```

飞书里测试：

- 文本消息收发。
- 上传文件后是否能生成附件保存路径提示。
- 语音消息是否可转写。
- `web_cn_search` 是否能返回检索结果。

## Security Notes / 安全说明

- 不要把真实密钥提交到 Git 仓库。
- 不要提交你的本地配置文件（如 `~/.config/moltis/moltis.toml`）。
- 只提交代码、迁移、README、example 配置。
