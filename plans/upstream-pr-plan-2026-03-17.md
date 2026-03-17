# Upstream PR Plan After Syncing With Moltis v0.10.18

This note summarizes which parts of the `moltis-feishu` fork should be proposed upstream, which parts should remain fork-only, and how to split the upstream work into reviewable PRs.

## Goal

Reduce long-term fork maintenance cost without trying to upstream the whole fork.

The guiding rule is:

- upstream generic fixes and reusable extension points
- keep fork-specific product semantics in the fork

## Current Fork Delta Buckets

After syncing with upstream `v0.10.18`, the meaningful fork-specific changes fall into four buckets:

1. Feishu channel integration
2. Chinese web tools
3. Agent/session policy changes
4. Generic bugfixes and infrastructure improvements

Only buckets 1, 2, and 4 contain realistic upstream candidates right now.

Bucket 3 should stay fork-only for now.

## What Should Be Proposed Upstream

### PR 1: MiniMax OpenAI-compatible system message fix

Files:

- `crates/providers/src/openai.rs`

Why:

- generic provider compatibility fix
- low-risk and easy to test
- not tied to Feishu or fork-specific session behavior

Scope:

- keep MiniMax system instructions in the standard `messages` array
- do not use a provider-specific top-level `system` field for MiniMax

Why first:

- smallest PR
- easy for upstream to review and accept independently

### PR 2: Channel config sanitization and masked-secret-preserving updates

Files:

- `crates/gateway/src/channel.rs`
- `crates/service-traits/src/lib.rs`

Why:

- generic channel-management improvement
- useful for any channel backend with secrets
- enables safe runtime access to channel account settings

Scope:

- add gateway-side `account_config(...)` access
- redact secrets from returned channel config
- preserve existing secrets when config updates contain placeholders such as `[REDACTED]`

Why this matters:

- avoids secret loss during UI-based config edits
- creates a cleaner host capability for future channel plugins

### PR 3: Preserve original inbound attachment filenames

Files:

- `crates/channels/src/plugin.rs`

Why:

- small, generic metadata improvement
- useful across any file-capable channel backend

Scope:

- add `original_name` to `ChannelAttachment`
- keep the field optional

Why separate:

- very small review surface
- clearly generic

### PR 4: Generic gateway attachment store for inbound channel files

Files:

- `crates/gateway/src/attachment_store.rs`
- `crates/gateway/migrations/20260228112000_attachment_store.sql`
- any small integration glue needed for generic storage

Why:

- general infrastructure for inbound files
- not Feishu-specific in concept
- reusable for Discord, Slack, Telegram, and future channels

Scope:

- hash-based deduplicated blob storage
- metadata references keyed by session/channel/message context
- original filename preservation where available

Review note:

- this should be framed as generic channel-file infrastructure, not as a Feishu feature

## What Should Not Be Proposed Upstream Yet

### Do not propose the current `/agent` and `/handoff` session policy

Files include:

- `crates/chat/src/lib.rs`
- `crates/sessions/src/metadata.rs`
- `crates/sessions/migrations/20260317000000_session_memory_owner_and_mode.sql`
- `crates/gateway/src/session.rs`
- `crates/gateway/src/channel_events.rs`

Why not now:

- these changes alter core session semantics
- they introduce fork-specific product behavior:
  - separate active agent vs memory owner
  - `ephemeral` agent switching
  - `attached` handoff into a new session
- they cut across chat, session metadata, compaction, and memory write policy

Upstream is likely to want a broader design discussion before accepting such changes.

Recommendation:

- keep this in the fork for now
- only open an upstream design discussion later if we first identify a narrow extension point, such as pluggable session/agent policy hooks

### Do not propose full Feishu support as a single large PR

Why not:

- the business logic is reasonably isolated already
- the main upstream problem is missing extension seams, not just the presence of Feishu code
- a large feature PR would be harder to review than a few small infrastructure PRs

Recommendation:

- upstream extension points first
- keep Feishu runtime implementation in the fork until the host architecture is easier to extend cleanly

## What Can Stay Fork-Only For Now

These remain valuable, but should stay in the fork until upstream extension points improve:

- `crates/feishu/*`
- `crates/tools/src/web_cn_search.rs`
- `crates/tools/src/web_read.rs`
- fork-specific example configs and docs
- fork-specific session/agent semantics

## Recommended Upstream Sequence

1. MiniMax provider fix
2. Channel config sanitization and masked-secret merge
3. `ChannelAttachment.original_name`
4. Generic attachment store

This order keeps the first PRs small and de-risks later discussions.

## Draft Upstream Discussion Text

The text below can be adapted into a GitHub Discussion, issue, or PR series introduction.

---

Title: Proposal: upstream a small set of generic fixes and extension points from our Feishu fork

Hi Moltis team,

We maintain a Feishu-focused fork that was recently rebased onto `v0.10.18`. During that work, we found that our fork-specific functionality falls into two very different categories:

1. generic fixes and extension points that seem broadly useful upstream
2. product-specific session/agent semantics that are probably better kept out of upstream for now

To keep the scope small and reviewable, we would like to propose upstreaming only the first category.

### Proposed upstream candidates

#### 1. MiniMax OpenAI-compatible system message handling

We found that MiniMax works correctly when system instructions remain in the standard `messages` array, rather than being extracted into a top-level `system` field.

This change is small, well-contained, and covered by focused tests. It appears to be a provider compatibility fix rather than a fork-specific behavior change.

Why this seems upstream-worthy:

- generic provider compatibility improvement
- no product-level behavior change outside MiniMax integration
- easy to validate with existing provider tests

#### 2. Sanitized channel account config access from the gateway

Our fork needed a way for gateway-side command and session logic to read channel account settings in a safe way. We introduced:

- a `ChannelService::account_config(...)` API
- recursive secret redaction for returned config values
- update-time merging that preserves existing secrets when the UI sends masked placeholders such as `[REDACTED]`

Why this seems upstream-worthy:

- useful for all channel types, not just Feishu
- improves admin/update UX for secret-bearing channel configs
- creates a cleaner extension point for future channel-specific logic

#### 3. Preserve original filenames on inbound channel attachments

We added `original_name` to inbound `ChannelAttachment` metadata so channels that provide an original uploaded filename can preserve it downstream.

Why this seems upstream-worthy:

- generic attachment metadata
- useful across multiple channel backends
- small surface area and low risk

#### 4. Generic attachment blob store for inbound channel files

Our fork also introduced a gateway-side attachment store that deduplicates inbound files by hash and keeps metadata references.

Why this seems upstream-worthy:

- generic infrastructure, not Feishu-specific
- useful for any channel that supports file/document uploads
- provides a better base for future file-aware tooling

### What we are not proposing upstream right now

We are intentionally not proposing our session/agent behavior changes at this stage. In our fork we introduced concepts like:

- separate active agent vs memory owner
- ephemeral `/agent` switching
- attached `/handoff` into a fresh session

These changes are useful for our product, but they affect core session/chat semantics and likely need a more explicit upstream design discussion before they would make sense in mainline Moltis.

### Suggested review strategy

To keep review manageable, we would split this into a few small PRs:

1. MiniMax provider fix
2. channel config redaction + masked-secret merge + account config access
3. attachment filename support
4. generic attachment store

If this direction makes sense, we can prepare the PRs in that order.

### Goal

Our goal is not to upstream our fork wholesale. We want to upstream only the small, generic pieces that:

- reduce our long-term fork delta
- improve extensibility for future channel integrations
- are broadly useful outside Feishu

If helpful, we can also open a separate design discussion later about whether Moltis should support pluggable session/agent policies, but we do not want to mix that larger topic into the initial PRs.

Thanks.

---

## Practical Notes

- `gh` must be authenticated with a valid token before PR creation can be automated from this machine.
- `bd` was not available on this machine during this review session.
