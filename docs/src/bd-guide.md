# bd Guide / bd 使用指南

`bd` is the task and issue tracking tool used by this repository. It is not a build tool or a formatter. It is the system this repo uses to track what should be done next.

`bd` 是这个仓库使用的任务和 issue 跟踪工具。它不是构建工具，也不是格式化工具。它的职责是管理“接下来要做什么”。

You can think of the tool split like this:

你可以这样理解这几个工具的分工：

- `git`: code history
- `cargo`: Rust build and dependency management
- `bd`: task tracking, follow-up work, dependencies, and work status

- `git`：代码版本历史
- `cargo`：Rust 构建和依赖管理
- `bd`：任务跟踪、后续工作、依赖关系和状态管理

---

## Why This Repo Uses `bd` / 为什么这个仓库要用 `bd`

This repository spans multiple areas:

这个仓库涉及的范围比较广：

- Feishu integration
- Search and reading tools
- Gateway / Config / UI / CI / Release

- 飞书接入
- 搜索与全文读取工具
- Gateway / Config / UI / CI / Release

During development, it is common to finish one piece of work and discover a second piece that should be tracked separately. If that follow-up lives only in chat history or in someone's memory, it will get lost.

开发过程中经常会出现这种情况：当前功能做完了，但顺手发现了另一个应该单独跟踪的后续任务。如果这些内容只留在聊天记录里，或者只靠人记忆，后面就很容易丢。

`bd` is useful because it gives those follow-ups a durable place to live:

`bd` 的价值就在于，它能把这些后续工作正式记录下来：

1. Turn follow-up work into a formal issue
2. Record dependencies between pieces of work
3. Assign priority and status
4. Work well from the CLI and in agent workflows

1. 把后续工作变成正式 issue
2. 记录任务之间的依赖关系
3. 区分优先级和状态
4. 适合 CLI 和 agent 工作流

For example, we already simplified `web_read` to `jina + metaso + spider`, but `web_crawler` should be designed separately. That belongs in `bd`, not only in chat.

例如这次我们已经把 `web_read` 收敛成 `jina + metaso + spider`，但 `web_crawler` 应该单独设计。这种事情就应该进 `bd`，而不是只留在聊天里。

---

## Core Concepts / 核心概念

### Issue / 问题单

The basic unit in `bd` is an issue. An issue typically contains:

`bd` 里的基本单位就是一个 issue。通常包含：

- `id`
- `title`
- `description`
- `type`
- `priority`
- `status`

### Dependency / 依赖关系

Issues can depend on each other. That allows you to express:

issue 之间可以有依赖关系。这样你就能表达：

- A is blocked by B
- B was discovered while working on A

- A 被 B 阻塞
- B 是从 A 的工作里发现出来的

That is much more useful than a flat TODO list.

这比一个平铺的 TODO 清单强很多。

### Dolt Backend / Dolt 后端

In this repository, `bd` uses Dolt as its storage backend. You will see:

在这个仓库里，`bd` 使用 Dolt 做后端存储。你会看到：

- `.beads/`

That directory holds local `bd` runtime state.

这个目录保存的是 `bd` 的本地运行状态。

Useful diagnostics:

常用诊断命令：

```bash
bd doctor
bd dolt status
```

---

## Human Workflow / 人类开发者的日常用法

As a human developer, these are the commands you will use most often.

作为人类开发者，你最常用的是下面这些命令。

### 1. See what exists / 看当前有哪些任务

```bash
bd list
```

If you want to see what is ready to work on now:

如果你想看“现在最适合做什么”：

```bash
bd ready
```

### 2. Show one issue / 查看某个 issue 详情

```bash
bd show <issue-id>
```

Example:

例如：

```bash
bd show moltis-feishu-ifx
```

### 3. Create a new issue / 创建新 issue

```bash
bd create "Title" --description "Details" -t feature -p 2
```

Common issue types:

常见 issue 类型：

- `bug`
- `feature`
- `task`
- `chore`

Priority levels:

优先级：

- `0`: highest
- `1`: high
- `2`: medium
- `3`: low
- `4`: backlog

- `0`：最高
- `1`：高
- `2`：中
- `3`：低
- `4`：backlog

### 4. Update an issue / 更新 issue

```bash
bd update <issue-id> --priority 1
```

You can also change the title, description, and other fields.

你也可以修改标题、描述和其他字段。

### 5. Close an issue / 关闭 issue

```bash
bd close <issue-id> --reason "Completed"
```

### 6. See overall database status / 查看整个库的状态

```bash
bd status
```

---

## Commands You Will Actually Use / 真正常用的命令

This set is enough for everyday usage:

日常使用，记住这一组基本就够了：

```bash
bd list
bd ready
bd show <issue-id>
bd create "..." --description "..." -t feature -p 2
bd update <issue-id> ...
bd close <issue-id> --reason "Completed"
bd doctor
bd dolt status
```

Later, when you are comfortable, you can explore these:

熟悉之后，再去看下面这些进阶命令：

```bash
bd dep
bd search
bd query
bd history
bd github
```

---

## Good Practices For This Repo / 这个仓库里的推荐用法

### Create follow-up work explicitly / 明确创建后续任务

Do not leave follow-up work only in:

不要把后续工作只留在：

- chat history
- temporary TODO notes
- your head

- 聊天记录
- 临时 TODO
- 脑子里

Create a formal issue instead.

应该建成正式 issue。

Examples:

例如：

- `Design web_crawler`
- `Split CI workflows`
- `Add spider JS-render mode`

- “单独设计 `web_crawler`”
- “拆分 CI workflow”
- “补 spider 的 JS 渲染模式”

### Keep titles concrete / 标题要具体

Bad titles:

差的标题：

- `fix search`
- `improve crawler`

Good titles:

好的标题：

- `Register web_read in gateway tool registry`
- `Design web_crawler tool on top of spider`

### One issue, one clear unit of work / 一条 issue 只表达一个清晰工作单元

Do not make one issue cover unrelated work.

不要把不相干的事情塞进一条 issue。

Bad example:

不好的例子：

- `fix web_read + update README + rebuild CI + prepare release`

Split that into separate issues.

这种应该拆成多条。

---

## Typical Examples / 常见示例

### Example: record a follow-up feature / 记录一个后续 feature

```bash
bd create \
  "Design web_crawler tool on top of spider" \
  --description "Separate multi-page crawling from web_read. Add domain allowlists, page/depth budgets, timeout controls, and output shape." \
  -t feature \
  -p 2
```

### Example: close completed work / 关闭已完成工作

```bash
bd close moltis-feishu-ifx --reason "Completed"
```

### Example: check whether the local database is healthy / 检查本地数据库是否健康

```bash
bd doctor
bd dolt status
```

---

## Troubleshooting / 排障

### `bd` exists, but issue commands fail / `bd` 命令存在，但 issue 操作失败

Start here:

先从这里开始看：

```bash
bd doctor
bd dolt status
```

Common causes:

常见原因：

- the Dolt server is running but the project database is missing
- `.beads` state became inconsistent after switching branches
- this checkout has not been initialized correctly

- Dolt server 在跑，但项目数据库不存在
- 切换分支后 `.beads` 状态不一致
- 当前 checkout 没有正确初始化

### `database not found` / `database not found`

This usually means:

这通常意味着：

- the Dolt server itself is fine
- but the repository database does not exist

- Dolt server 本身正常
- 但这个仓库对应的数据库不存在

Do not guess. Run:

先不要猜，先跑：

```bash
bd doctor
bd dolt status
```

### `bd` does not work after a fresh clone / fresh clone 后 `bd` 不工作

Try this first:

优先试这个：

```bash
bd doctor
bd bootstrap
```

If that still does not fix it, inspect `.beads/` and the Dolt status for the repo.

如果还是不行，再检查 `.beads/` 和 Dolt 状态。

---

## For Agents vs Humans / 对 agent 和人类的区别

Humans usually use:

人类通常用：

```bash
bd list
bd show <issue-id>
bd create ...
bd close ...
```

Agents usually use:

agent 通常用：

```bash
bd ready --json
bd create ... --json
bd update ... --json
bd close ... --json
```

The underlying issue system is the same. Only the output format differs.

底层其实是同一套 issue 系统，区别主要是输出格式。

---

## Summary / 总结

If you only remember three things:

如果你只记三件事：

1. `bd` tracks work; it does not build code
2. When you discover follow-up work, create an issue instead of leaving it only in chat
3. When something looks wrong, start with:

1. `bd` 是用来跟踪工作的，不是用来构建代码的
2. 发现后续工作时，建 issue，不要只留在聊天里
3. 如果状态不对，先看：

```bash
bd doctor
bd dolt status
```
