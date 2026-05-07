# AGENTS.md

Project-specific rules and conventions for AI assistants and contributors.

## High-Priority Rules

### Do NOT add fields to `AcpAgentManager` unless every alternative is exhausted

`AcpAgentManager` (in `crates/aionui-ai-agent/src/acp_agent.rs`) is already large and carries multiple overlapping state holders (e.g. `runtime_snapshot`, `state`, `preferred_mode`, `config`). New fields tend to duplicate semantics that `AcpRuntimeSnapshot` or `AcpState` already model, which fragments the source of truth and makes resume/new paths diverge.

Before adding a field:
1. Can the value live in `AcpRuntimeSnapshot`? (runtime/session-scoped state, including user-selected current_mode/current_model/config_selections)
2. Can it be derived from existing fields (`metadata`, `config`, `runtime_snapshot`, `state`)?
3. Can it be persisted via `acp_session.session_config` + `preload_persisted` instead of a new in-memory field?
4. If it must be in-memory and transient, can it be scoped to the call site (local variable, channel, task state) rather than the manager?

Only after exhausting the above — and explicitly documenting why each option is insufficient — add a new field. When doing so, also document its lifecycle (who writes, who reads, when it is invalidated) in a doc comment on the field.

## Architecture

> For detailed background and design decisions, see [ARCHITECTURE.md](./ARCHITECTURE.md).

Cargo workspace with 19 crates under `crates/`. Dependencies flow downward through four layers:

**Foundation:** `aionui-common`, `aionui-api-types`, `aionui-db`, `aionui-assets`
**Capability:** `aionui-auth`, `aionui-realtime`
**Domain:** `aionui-conversation`, `aionui-channel`, `aionui-team`, `aionui-cron`, `aionui-file`, `aionui-office`, `aionui-shell`, `aionui-mcp`, `aionui-ai-agent`, `aionui-extension`, `aionui-system`, `aionui-assistant`
**Composition:** `aionui-app` — top-level binary, composes all crates into the axum server

Binary name: `aionui-backend` (produced by `crates/aionui-app`).

### Crate Hierarchy & Dependencies

- ✅ Upper layers may depend on lower layers (including cross-layer)
- ✅ Same-layer interaction through trait abstractions only
- ❌ No lower-layer depending on upper-layer
- ❌ No circular dependencies
- Changes to foundation crates require impact assessment

### Domain Crate Structure

Every domain crate must follow:
- `lib.rs` — module exports only, no business logic
- `routes.rs` — export `domain_routes(state) -> Router`, handlers do request/response transformation only
- `service.rs` — sole location for business logic, must not import axum
- `state.rs` — `#[derive(Clone)]` RouterState holding Arc-wrapped dependencies

### API Conventions

- Route prefix: `/api/`
- Resource names: kebab-case
- Response format: `ApiResponse<T>` (success) / `ErrorResponse` (failure)
- All request/response types defined in `aionui-api-types`
- `aionui-api-types` must NOT depend on axum/tower or any HTTP framework

### WebSocket Events

- Format: `domain.camelCaseAction` (two-level structure)
- Message type: `WebSocketMessage<T>` (name + data)
- Existing kebab-case or three-level names are legacy — new events must follow the convention

### Data Layer

- Repository traits in `aionui-db`, prefixed with `I`
- Concrete implementations prefixed with `Sqlite`
- Row models in `aionui-db/src/models/`
- Params objects co-located in repository files
- Migrations: `NNN_descriptive_name.sql`, no manual DB modifications
- Services depend on traits, never on concrete implementations

### Dependency Injection

- `AppServices` is the sole service construction center
- Domain crates only define RouterState, never construct their own dependencies
- All assembly happens in `aionui-app`'s `build_*_state()` functions

### Security

- New endpoints must be evaluated for auth middleware requirement
- State-changing operations must be CSRF-protected
- Sensitive operations should have rate limiting
- Error responses must not leak internal details
- Secrets must never be hardcoded

## Route Map

| Prefix | Crate | Auth |
|--------|-------|------|
| `POST /login`, `/api/auth/*` | aionui-auth | Public (rate-limited) |
| `POST /logout`, `/api/auth/user`, `/api/auth/change-password`, `/api/ws-token` | aionui-auth | Yes |
| `/api/conversations/*`, `/api/messages/*` | aionui-conversation | Yes |
| `/api/agents`, `/api/agents/refresh`, `/api/agents/test` | aionui-ai-agent | Yes |
| `/api/acp/*`, `/api/conversations/{id}/acp/*` | aionui-ai-agent | Yes |
| `/api/bedrock/*`, `/api/gemini/*` | aionui-ai-agent | Yes |
| `/api/conversations/{id}/workspace`, `/api/conversations/{id}/side-question`, `/api/conversations/{id}/slash-commands`, `/api/conversations/{id}/reload-context` | aionui-ai-agent | Yes |
| `/api/remote-agents/*` | aionui-ai-agent | Yes |
| `/api/settings/*`, `/api/providers/*`, `/api/system/*` | aionui-system | Yes |
| `/api/fs/*` | aionui-file | Yes |
| `/api/mcp/*` | aionui-mcp | Yes |
| `/api/extensions/*`, `/api/hub/*`, `/api/skills/*` | aionui-extension | Yes |
| `/api/channel/*` | aionui-channel | Yes |
| `/api/teams/*` | aionui-team | Yes |
| `/api/cron/*` | aionui-cron | Yes |
| `/api/word-preview/*`, `/api/excel-preview/*`, `/api/ppt-preview/*`, `/api/preview-history/*`, `/api/star-office/*`, `/api/document/*` | aionui-office | Yes |
| `/api/ppt-proxy/*`, `/api/office-watch-proxy/*` | aionui-office | Public (iframe) |
| `/api/shell/*`, `/api/stt` | aionui-shell | Yes |
| `/ws` | aionui-realtime | Token callback |
| `/health` | aionui-app | Public |

## Code Style

- Rust 2024 edition, stable toolchain (pinned in `rust-toolchain.toml`)
- Comments in English, commit messages in English
- Each `.rs` file follows single responsibility — one module, one concern
- Max 1000 lines per `.rs` file; split into submodules when approaching the limit

## Development Workflow

### Bundled bun Runtime

The backend embeds a bun runtime for self-contained distribution. Relevant env vars:

- `AIONUI_EMBED_BUN=1` — enable bun download + embed during `cargo build`.
  Release CI sets this; local dev builds skip it (faster, no network).
- `BUN_VARIANT=default|baseline` — select which Linux x64 variant to
  embed. `baseline` targets CPUs without AVX2.
- `AIONUI_BUN_PATH=/abs/path/to/bun` — runtime override. When set and
  pointing to an executable file, `resolve_bun()` returns it verbatim,
  skipping the embedded + `which` fallback chain. Useful for testing
  custom bun builds or bisecting bun regressions.

The bun version is pinned in
`crates/aionui-runtime/Cargo.toml` under
`[package.metadata.aionui-runtime] bun_version = "..."`. Upgrading bun is
a one-line change — no source edits required.

### Pushing Code

Always use `just push` instead of `git push`.
It runs fmt → clippy → test before pushing, preventing CI failures.
Supports the same arguments as `git push` (e.g. `just push -u origin feat/branch`).

### Add Endpoint to Existing Crate

1. Request/response types → `aionui-api-types/src/{domain}.rs`
2. Handler function → `crates/aionui-{domain}/src/routes.rs`
3. Business logic → `crates/aionui-{domain}/src/service.rs`
4. Register route in `domain_routes()` function
5. Add test → `crates/aionui-{domain}/tests/` or `crates/aionui-app/tests/`

### Add Migration

1. Next number → `ls crates/aionui-db/migrations/`
2. Create `NNN_descriptive_name.sql` with `IF NOT EXISTS`

### Add WebSocket Event

1. Event type → `aionui-api-types`
2. Emit via `event_bus.broadcast()` in service
3. Naming: `domain.camelCaseAction`

## Test Organization

| Location | What goes there |
|----------|----------------|
| Inline `#[cfg(test)]` in each `.rs` file | Unit tests for that module's internals |
| `crates/<crate>/tests/` | Integration / E2E tests for that crate |

### Testing Rules

- Database tests use `init_database_memory()`
- Prefer real in-memory DB over mocks; mock only to isolate unneeded dependencies
- New features must include tests

### Test Scope Requirements

**Happy Path (Critical Paths)**

Every new or modified feature must have integration tests covering its normal flow. Critical paths that always require test coverage:
- Authentication flow (login, token refresh, permission checks)
- Message sending and retrieval
- Agent session creation and interaction
- File upload/download
- WebSocket connection and event delivery

**Bad Path (Error Paths)**

New endpoints or business logic must include tests for these scenarios:
- Invalid input (missing fields, wrong types, oversized content)
- Resource not found (404)
- Insufficient permissions (unauthenticated, accessing another user's resources)
- Business rule violations (duplicate creation, operations not allowed in current state)

Bad path tests must assert specific error codes or error messages — asserting merely "not success" is not acceptable.

**Security Tests**

Endpoints involving authentication, authorization, or data isolation must include security tests:
- Unauthenticated requests are rejected (401)
- Cross-user data isolation (user A cannot access user B's resources)
- State-changing requests are rejected when CSRF token is missing or invalid
- Sensitive fields (passwords, tokens) never appear in responses

**WebSocket Event Tests**

New WebSocket events must verify:
- The event is emitted after the correct business operation
- Event payload conforms to `WebSocketMessage<T>` structure
- Events are only delivered to authorized subscribers (no leakage to unrelated users)

### Test Failure Handling

When a test fails, do NOT modify the test to make it pass. First determine:

1. **Test assertion still represents correct behavior** → fix implementation, not the test
2. **Requirements/interface intentionally changed** → may update test, but must confirm:
   - The change is intentional (not an unintended side effect)
   - New assertions still validate meaningful behavior
3. **Uncertain** → stop, trace back the change, clarify before proceeding

Prohibited:
- ❌ Deleting failing tests to "fix" the problem
- ❌ Weakening specific assertions to vague ones (e.g., `assert_eq!(status, 201)` → `assert!(status.is_success())`)

## Verification Strategy

> ⚠️ **When to run what:**
> - During development: only test the crate you're working on → `cargo test -p aionui-<crate>`
> - After implementation complete: full verification → `cargo test --workspace`
> - Do NOT run `cargo test --workspace` at the start of a task.
>
> ⚠️ **Performance:**
> - `cargo clippy --workspace` takes several minutes — use `run_in_background: true`.
> - `cargo test --workspace` takes 10+ minutes. MUST use `run_in_background: true` when calling via Bash tool, otherwise it will timeout.
> - `cargo clippy -p aionui-<crate>` and `cargo test -p aionui-<crate>` typically complete in under 1 minute.

### During Development (fast feedback loop)

```bash
cargo test -p aionui-<crate>                          # Test the crate you changed
cargo clippy -p aionui-<crate> -- -D warnings         # Lint the crate you changed
```

### Before Commit (affected crates)

```bash
cargo fmt --all -- --check                                                      # Format gate (instant)
cargo clippy -p aionui-<crate1> -p aionui-<crate2> -- -D warnings              # Lint affected crates
cargo test -p aionui-<crate1> -p aionui-<crate2>                               # Test affected crates
```

### Before Push (full workspace)

```bash
just push                                             # fmt → clippy → test → git push
```
