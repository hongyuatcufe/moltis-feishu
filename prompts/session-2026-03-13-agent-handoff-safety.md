# Agent And Handoff Safety Revision

Date: 2026-03-13

## Goal

Reduce cross-agent identity and memory contamination by collapsing the model to two behaviors only:

- `/agent` => ephemeral + same_session + no memory write
- `/handoff` => attached + new_session + sanitized handoff summary

## What Changed

### 1. Session metadata now separates persona from memory ownership

Added to `SessionEntry` and SQLite-backed session metadata:

- `agent_id`: active answering persona
- `memory_owner_agent_id`: long-term memory owner for the session
- `agent_mode`: `attached` or `ephemeral`

This lets the system borrow an agent persona without implicitly reassigning memory ownership.

### 2. Chat runtime now uses split agent semantics

In `crates/chat/src/lib.rs`:

- prompt identity / soul / tools / agents text resolve from `agent_id`
- memory text and agent-scoped memory tools resolve from `memory_owner_agent_id`
- memory writes are only enabled when `agent_mode == attached`
- compact/silent-memory behavior skips writing agent memory in `ephemeral` mode

Result:

- `/agent writer` can answer as `writer`
- session memory still belongs to the original owner
- temporary switches no longer write contaminated memory into the borrowed agent workspace

### 3. `/agent` is now explicitly temporary

In `crates/gateway/src/channel_events.rs`:

- `/agent <id>` stays in the current session
- sets `agent_id = <id>`
- preserves current `memory_owner_agent_id`
- sets `agent_mode = ephemeral`
- returns UI text making the temporary behavior explicit

### 4. `/handoff` is now single-mode and isolated

`/handoff` was simplified to the only supported model:

- always create a new session
- bind both `agent_id` and `memory_owner_agent_id` to the target agent
- mark the new session as `attached`
- switch the channel’s active session to the new session

No additional `/handoff` mode matrix is kept.

### 5. Handoff context is sanitized before transfer

Instead of forwarding raw recent history, the new handoff packet can include a generated summary built from recent turns:

- keeps user requests, assistant progress, and tool outcomes
- strips persona/identity-style content such as self-names and identity markers
- aims to pass task state, not assistant personality residue

## Tests Added Or Updated

- `moltis-chat`: `session_agent_helpers_split_persona_from_memory_owner`
- `moltis-gateway`: `agent_command_updates_current_session_only`
- `moltis-gateway`: `handoff_new_session_creates_isolated_session_and_one_shot_context`
- `moltis-gateway`: `parse_handoff_args_supports_note_only`

## Validation Run

Passed:

- `cargo test -p moltis-sessions test_sqlite_agent_id -- --nocapture`
- `cargo test -p moltis-chat session_agent_helpers_split_persona_from_memory_owner -- --nocapture`
- `cargo check -p moltis-chat -p moltis-gateway -p moltis-sessions`

Previously run and passed during implementation:

- `cargo test -p moltis-gateway agent_command_updates_current_session_only -- --nocapture`
- `cargo test -p moltis-gateway handoff_new_session_creates_isolated_session_and_one_shot_context -- --nocapture`
- `cargo test -p moltis-gateway parse_handoff_args_supports_note_only -- --nocapture`

## Remaining Follow-Up

- Add a stricter sanitized-summary test covering more identity/persona phrases
- Consider UI wording updates where `/handoff` help text still references old mental models
- Run broader repo validation before merge if this branch is headed to PR
