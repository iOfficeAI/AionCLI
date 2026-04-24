# AGENTS.md

Project-specific rules and conventions for AI assistants and contributors.

## Architecture

Cargo workspace with 17 crates under `crates/`. Dependencies flow downward:

- `aionui-common` — shared types, enums, error types, crypto utilities
- `aionui-api-types` — API request/response types, shared across crates
- `aionui-db` — SQLite database layer (sqlx), repository traits and implementations
- `aionui-auth` — JWT, CSRF, password hashing, auth middleware
- `aionui-realtime` — WebSocket manager, event broadcasting
- Domain crates (`aionui-conversation`, `aionui-channel`, `aionui-team`, `aionui-cron`, `aionui-file`, `aionui-office`, `aionui-shell`, `aionui-mcp`, `aionui-ai-agent`, `aionui-extension`, `aionui-system`) — each owns its routes, service, and tests
- `aionui-app` — top-level binary, composes all crates into the axum server

Never introduce circular dependencies or upward references.

Binary name: `aionui-backend` (produced by `crates/aionui-app`).

## Architecture Rules

> For detailed background and design decisions, see [ARCHITECTURE.md](./ARCHITECTURE.md).

### Crate Hierarchy & Dependencies

- Four layers: Foundation → Capability → Domain → Composition
- ✅ Upper layers may depend on lower layers (including cross-layer)
- ✅ Same-layer interaction through trait abstractions only
- ❌ No lower-layer depending on upper-layer
- ❌ No circular dependencies
- Changes to foundation crates (common, api-types, db) require impact assessment

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

- Rust 2024 edition, stable toolchain
- `cargo clippy` must pass without warnings
- `cargo fmt` must pass
- Comments in English, commit messages in English
- Each `.rs` file follows single responsibility — one module, one concern
- Max 1000 lines per `.rs` file; split into submodules when approaching the limit

## Quick Recipes

**Add endpoint to existing crate:**
1. Request/response types → `aionui-api-types/src/{domain}.rs`
2. Handler function → `crates/aionui-{domain}/src/routes.rs`
3. Business logic → `crates/aionui-{domain}/src/service.rs`
4. Register route in `domain_routes()` function
5. Add test → `crates/aionui-{domain}/tests/` or `crates/aionui-app/tests/`

**Add migration:**
1. Next number → `ls crates/aionui-db/migrations/`
2. Create `NNN_descriptive_name.sql` with `IF NOT EXISTS`

**Add WebSocket event:**
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
> - `cargo test --workspace` takes 10+ minutes. MUST use `run_in_background: true` when calling via Bash tool, otherwise it will timeout.
> - `cargo clippy --workspace` also takes several minutes — use `run_in_background: true`.
> - `cargo test -p aionui-<crate>` and `cargo clippy -p aionui-<crate>` typically complete in under 1 minute.

### During Development (fast feedback loop)

```bash
cargo test -p aionui-<crate>                          # Test the crate you changed
cargo clippy -p aionui-<crate> -- -D warnings         # Lint the crate you changed
```

### Before Commit (affected crates)

```bash
cargo test -p aionui-<crate1> -p aionui-<crate2>     # Test all affected crates
cargo fmt --all -- --check                             # Format check
```

### Final Verification (run in background, 10+ min)

```bash
cargo test --workspace                                 # Full test suite
cargo clippy --workspace -- -D warnings                # Full lint
cargo fmt --all                                        # Format
```
