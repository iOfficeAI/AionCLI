# Assistant Snake-Case Realignment — Design Spec

**Date:** 2026-04-24
**Scope:** Scrub the last camelCase residues from the wire surface — the
assistant pilot's 7 `rename_all = "camelCase"` structs in
`aionui-api-types/src/assistant.rs`, the `builtin.rs` data-file loader,
the 20-entry `assistants.json` manifest, and two drive-by camelCase
bugs the skill realignment pilot left on the follow-up list (ACP
`setModel` / `setConfigOption`, filesystem `createTempFile` /
`createUploadFile`). Bring the whole repo in line with the
project-wide snake_case convention set by `dae96f8`.

**Companion spec (frontend plan + team layout):**
[`AionUi/docs/backend-migration/specs/2026-04-24-assistant-snake-case-realignment-design.md`](../../../../AionUi/docs/backend-migration/specs/2026-04-24-assistant-snake-case-realignment-design.md)

---

## 1. Root Cause

Three distinct camelCase pockets survived the prior realignment:

1. **Assistant pilot (primary)** — scaffold commit `1376947` (2026-04-23
   16:04) landed 5 hours *after* `dae96f8` (2026-04-23 11:25) established
   snake_case as the project-wide wire convention, but the author didn't
   read the fresh-off-the-press convention and designed the contract in
   camelCase. Both sides shipped camelCase, so there's no runtime
   contract mismatch — the residue is pure convention divergence.

2. **ACP `setModel` / `setConfigOption` (hotfix)** — backend
   `api-types/acp.rs` fields were always snake_case (`model_id`,
   `config_id`), no `rename_all` attr. Frontend `ipcBridge.ts:584-599`
   sends `modelId` / `configId`. This is a genuine runtime-broken
   contract, same shape as skill's H1. Users don't hit 400s only because
   these endpoints are low-traffic and errors may be swallowed in the
   callers.

3. **File `createTempFile` / `createUploadFile` (hotfix)** — backend
   `api-types/file.rs` field is `file_name`, frontend sends `fileName`.
   Same shape as above.

**Convention oracle:** `dae96f8` ("Remove rename_all='camelCase' from
all struct definitions across 18 files… Update all 152+ test assertions
to expect snake_case JSON keys.") — this is the project's wire-format
convention. Anything that deviates is either (a) pre-`dae96f8` and
unfixed, or (b) post-`dae96f8` and mis-oriented by an author who didn't
check main's history.

**Why now:** the skill realignment pilot's handoff logged ACP +
`createUploadFile` as follow-ups ("may or may not be broken at runtime
— not audited"). The user separately spotted `/api/assistants`'s
camelCase response. Clean all three in one coordinated pilot.

## 2. Goals

1. Remove all 7 `rename_all = "camelCase"` attributes from
   `crates/aionui-api-types/src/assistant.rs`. Keep the one
   `rename_all = "lowercase"` on the `AssistantSource` enum (unrelated,
   idiomatic lowercase enum serialization).
2. Remove the 1 `rename_all = "camelCase"` from
   `crates/aionui-assistant/src/builtin.rs` (the `BuiltinAssistant`
   data-file loader).
3. Flip all 20 entries in
   `crates/aionui-app/assets/builtin-assistants/assistants.json`
   from camelCase to snake_case keys. Use jq `walk` to rewrite keys
   only — never naive sed (might touch value strings).
4. Flip backend unit tests in `assistant.rs`'s `#[cfg(test)]` module
   (6 tests) and in `crates/aionui-app/tests/assistants_e2e.rs` (9
   hardcoded camelCase JSON keys). Add 1 new regression test
   `assistant_response_rejects_camel_case`.
5. Flip frontend `Assistant` type + all ~209 access sites across 43
   files in the AionUi repo to snake_case. Bulk rename via codemod +
   manual audit. Destructured local variable names preserved via
   `const { snake_name: camelName } = x` pattern to minimize
   downstream diff.
6. Flip `ipcBridge.ts` wire bodies for `setModel`, `setConfigOption`,
   `createTempFile`, `createUploadFile` to snake_case. Update all
   call-site argument names (tsc enforces).
7. Playwright E2E + Vitest + `skills_builtin_e2e` + `assistants_e2e`
   all green end-to-end.

## 3. Non-Goals

- No changes to `channel/plugins/weixin/types.rs` or
  `channel/plugins/dingtalk/types.rs` — those `rename_all = "camelCase"`
  attrs model external WeChat / DingTalk webhook payloads that we
  passively adapt to. Out of scope.
- No audit of other potential camelCase-on-the-wire endpoints beyond
  the three listed in §1 (Q6 resolved: "b" — only fix known). Other
  endpoints like `getMode` / `getConfigOptions` only flip if they're
  already snake on backend (no change needed) or get caught by the
  frontend tsc compile once the corresponding request types flip.
- No DB schema / migration changes. `crates/aionui-db/src/models/assistant.rs`
  is already snake_case throughout with no `rename_all`, as is
  `003_assistants.sql`.
- No backward-compatibility shims (no serde aliases). Frontend and
  backend ship together; no external consumer.
- No URL path parameter renames (e.g. `:conversation_id` stays as-is).

## 4. Backend Changes

### 4.1 `crates/aionui-api-types/src/assistant.rs`

Delete the 7 `#[serde(rename_all = "camelCase")]` attributes at lines
26, 68, 99, 129, 142, 149, 160. After deletion:

```bash
grep -c 'rename_all = "camelCase"' crates/aionui-api-types/src/assistant.rs
# 0
```

Keep line 16's `#[serde(rename_all = "lowercase")]` on `AssistantSource`
(enum-variant lowercase is idiomatic and consistent with skill pilot's
handling of similar enums).

**Resulting wire shapes** (Rust field names serialize as-is, snake_case):

| Struct | Wire keys |
|---|---|
| `AssistantResponse` | `id, source, name, name_i18n, description, description_i18n, avatar, enabled, sort_order, preset_agent_type, enabled_skills, custom_skill_names, disabled_builtin_skills, context, context_i18n, prompts, prompts_i18n, models, last_used_at` |
| `CreateAssistantRequest` | `id, name, description, avatar, preset_agent_type, enabled_skills, custom_skill_names, disabled_builtin_skills, prompts, models, name_i18n, description_i18n, prompts_i18n` |
| `UpdateAssistantRequest` | same as `CreateAssistantRequest` (all optional) |
| `SetAssistantStateRequest` | `enabled, sort_order, last_used_at` |
| `ImportAssistantsRequest` | `assistants` |
| `ImportAssistantsResult` | `imported, skipped, failed, errors` |
| `ImportError` | `id, error` |

### 4.2 `crates/aionui-api-types/src/assistant.rs` — inline tests

Flip the 6 tests in the `#[cfg(test)]` module:

- `assistant_source_camel_case_serializes_lowercase` → rename to
  `assistant_source_serializes_lowercase` (drop misleading camel_case
  in test name); assertions unchanged.
- `assistant_response_round_trip_camel_case` → rename to
  `assistant_response_round_trip_snake_case`; flip assertions
  `json["presetAgentType"]` → `json["preset_agent_type"]`, `"sortOrder"`
  → `"sort_order"`, `"lastUsedAt"` → `"last_used_at"`.
- `create_assistant_request_accepts_minimal_body`, `update_assistant_request_supports_partial`,
  `set_state_request_all_optional`, `import_result_default_is_zeroes`
  — don't reference camelCase keys; no change.

Add one new regression test:

```rust
#[test]
fn assistant_response_rejects_camel_case() {
    let json = serde_json::json!({
        "id": "a1",
        "source": "user",
        "name": "X",
        "presetAgentType": "gemini",  // ← legacy camelCase
        "enabled": true,
        "sortOrder": 0,
    });
    let resp: AssistantResponse = serde_json::from_value(json).unwrap();
    // `presetAgentType` silently drops; field stays default (empty string).
    assert_eq!(resp.preset_agent_type, "");
    assert_eq!(resp.sort_order, 0);
}
```

This pins down the "camel is silently ignored now, not aliased"
behavior so future refactors don't reintroduce dual-accept.

### 4.3 `crates/aionui-assistant/src/builtin.rs`

Delete line 34's `#[serde(rename_all = "camelCase")]` from
`BuiltinAssistant`. The struct's Rust fields are already snake_case
(`preset_agent_type`, `name_i18n`, `rule_file`, `skill_file`, etc.) so
removal alone flips deserialization to expect snake_case JSON keys.

### 4.4 `crates/aionui-app/assets/builtin-assistants/assistants.json`

Rewrite all camelCase keys in all 20 assistant entries. Use `jq walk`
(not sed) to avoid value-string collisions:

```bash
jq 'walk(
  if type == "object" then
    with_entries(
      .key |= (
        if . == "nameI18n" then "name_i18n"
        elif . == "descriptionI18n" then "description_i18n"
        elif . == "presetAgentType" then "preset_agent_type"
        elif . == "enabledSkills" then "enabled_skills"
        elif . == "customSkillNames" then "custom_skill_names"
        elif . == "disabledBuiltinSkills" then "disabled_builtin_skills"
        elif . == "ruleFile" then "rule_file"
        elif . == "skillFile" then "skill_file"
        elif . == "promptsI18n" then "prompts_i18n"
        else . end
      )
    )
  else . end
)' assets/builtin-assistants/assistants.json > assets/builtin-assistants/assistants.json.tmp \
  && mv assets/builtin-assistants/assistants.json.tmp assets/builtin-assistants/assistants.json
```

Approximately ~200 key substitutions across the file.
`preset-id-whitelist.json` is a flat string array and needs no change.

### 4.5 `crates/aionui-app/tests/assistants_e2e.rs`

Flip the 9 hardcoded camelCase JSON keys at lines 97, 105, 517, 525,
537, 547, 555 — all `serde_json::json!({...})` request bodies or
response key assertions (`"presetAgentType"`, `"sortOrder"`). No other
test semantics change.

### 4.6 Handlers, routes, service

**No changes.** Handlers access fields by Rust identifier (e.g.
`assistant.preset_agent_type`), which is serde-rename-transparent.

### 4.7 Regression — skill pilot must stay green

`crates/aionui-app/tests/skills_builtin_e2e.rs` (14 tests) must stay
green. The skill pilot's api-types are in a different file
(`skill.rs`, already realigned to snake) and don't touch assistant
wire. No cross-impact expected.

### 4.8 DoD (backend)

- [ ] `grep -c 'rename_all = "camelCase"' crates/aionui-api-types/src/assistant.rs` → 0
- [ ] `grep -c 'rename_all = "camelCase"' crates/aionui-assistant/src/builtin.rs` → 0
- [ ] `grep -cE '"(presetAgentType|nameI18n|descriptionI18n|enabledSkills|customSkillNames|disabledBuiltinSkills|promptsI18n|ruleFile|skillFile)"' crates/aionui-app/assets/builtin-assistants/assistants.json` → 0
- [ ] `cargo test -p aionui-api-types` all green, including new `assistant_response_rejects_camel_case`
- [ ] `cargo test -p aionui-assistant` all green
- [ ] `cargo test --test assistants_e2e` 44/44 green
- [ ] `cargo test --test skills_builtin_e2e` 14/14 green (regression)
- [ ] `cargo clippy --workspace -- -D warnings` no new warnings
- [ ] `cargo fmt --all -- --check` passes
- [ ] `cargo build --release` succeeds; `~/.cargo/bin/aionui-backend` refreshed

## 5. Frontend Changes

Three independent feature branches, one team sequence.

### 5.1 Branch `feat/assistant-snake-case` — Assistant bulk rename

#### 5.1.1 Core types — `src/common/types/assistantTypes.ts`

Flip every field in `Assistant`, `CreateAssistantRequest`,
`UpdateAssistantRequest`, `SetAssistantStateRequest`,
`ImportAssistantsRequest`, `ImportAssistantsResult`, `ImportError`:

| camelCase | snake_case |
|---|---|
| `nameI18n` | `name_i18n` |
| `descriptionI18n` | `description_i18n` |
| `sortOrder` | `sort_order` |
| `presetAgentType` | `preset_agent_type` |
| `enabledSkills` | `enabled_skills` |
| `customSkillNames` | `custom_skill_names` |
| `disabledBuiltinSkills` | `disabled_builtin_skills` |
| `contextI18n` | `context_i18n` |
| `promptsI18n` | `prompts_i18n` |
| `lastUsedAt` | `last_used_at` |

TypeScript allows snake_case identifiers; tsc does not complain.

#### 5.1.2 Bulk rename — 209 access sites, 43 files

Two-wave approach:

**Wave 1 — automated codemod** (target ~90% coverage). Use `ts-morph`
to traverse AST, matching property access / object literals / type
references where the target type is `Assistant` /
`CreateAssistantRequest` / etc. Rewrite property name only. Input:
field-map table (§5.1.1).

**Wave 2 — manual pass.** Grep residual camelCase names. Disambiguate:
does this reference still point at an `Assistant` field? If yes, flip;
if it's a field on some unrelated local type, leave.

#### 5.1.3 Destructuring — preserve local variable names

To minimize downstream churn, when destructuring from an `Assistant`:

```ts
// Before
const { sortOrder, nameI18n } = assistant;

// After
const { sort_order: sortOrder, name_i18n: nameI18n } = assistant;
```

Local variables keep camelCase; only the property-name side flips.
Consumer code of `sortOrder` / `nameI18n` locals remains untouched.
Codemod handles the rewrite.

#### 5.1.4 `ipcBridge.ts` — assistants block

The `assistants.list` / `create` / `update` / `setState` / `import`
signatures are typed by `Assistant[]` / `CreateAssistantRequest`, so
the flip propagates via type inference — no manual literal field
edits expected inside `ipcBridge.ts`'s `assistants` block.

#### 5.1.5 Electron process code — `src/process/extensions/resolvers/AssistantResolver.ts`, `src/process/utils/initAgent.ts`, `src/process/utils/migrateAssistants.ts`

Three files make heavy use of `Assistant` fields.

`migrateAssistants.ts` is the trickiest: it reads **legacy Electron
ConfigStorage** (camelCase, never changing) and constructs
`CreateAssistantRequest` (now snake_case) for the backend. Split the
shape-shifting into an explicit mapping function:

```ts
// new helper, standalone, unit-tested
export function legacyAssistantToCreateRequest(legacy: LegacyAssistant): CreateAssistantRequest {
  return {
    id: legacy.id,
    name: legacy.name,
    name_i18n: legacy.nameI18n,            // ← explicit mapping
    description_i18n: legacy.descriptionI18n,
    preset_agent_type: legacy.presetAgentType,
    enabled_skills: legacy.enabledSkills,
    custom_skill_names: legacy.customSkillNames,
    disabled_builtin_skills: legacy.disabledBuiltinSkills,
    prompts_i18n: legacy.promptsI18n,
    // ...etc
  };
}
```

This keeps the camel/snake boundary inside one testable function
rather than scattered across migrate code.

#### 5.1.6 Vitest

- `tests/unit/assistant/*.test.ts` — fixtures flipped to snake_case
- `tests/unit/migrateAssistants.test.ts` — input fixture stays camelCase
  (legacy shape), output assertions flip to snake_case (new shape).
  Ideally use a real legacy `aionui-config.txt` excerpt as fixture per
  migration-playbook lesson.
- Other hook / component tests: assistant-shaped mock objects flipped.

#### 5.1.7 E2E — Playwright

`tests/e2e/features/assistant-*` — any hardcoded `presetAgentType` /
`sortOrder` / etc. JSON literals in payload assertions flip.

### 5.2 Branch `fix/acp-camelcase-hotfix`

**Scope**: `src/common/adapter/ipcBridge.ts` lines ~584-599.

```ts
// setModel — before
setModel: httpPut<void, { conversationId: string; modelId: string }>(
  (p) => `/api/conversations/${p.conversationId}/acp/model`,
  (p) => ({ modelId: p.modelId }),   // ← camel body (broken)
),
// setModel — after
setModel: httpPut<void, { conversationId: string; modelId: string }>(
  (p) => `/api/conversations/${p.conversationId}/acp/model`,
  (p) => ({ model_id: p.modelId }),  // ← snake body (correct)
),
```

Local parameter `p.modelId` kept camelCase for TS idiom; only the body
key flips. Same pattern for `setConfigOption`: verify the body has a
`configId` field and flip to `config_id`.

Add 2 Vitest regression tests that call the bridge via mock `fetch` and
assert the request body uses snake keys.

### 5.3 Branch `fix/fs-temp-camelcase-hotfix`

**Scope**: `src/common/adapter/ipcBridge.ts` lines ~305-306.

```ts
// Before
createTempFile: httpPost<string, { fileName: string }>('/api/fs/temp'),
createUploadFile: httpPost<string, { fileName: string; conversationId?: string }>('/api/fs/temp'),

// After
createTempFile: httpPost<string, { file_name: string }>('/api/fs/temp'),
createUploadFile: httpPost<string, { file_name: string; conversation_id?: string }>('/api/fs/temp'),
```

This flips the request **type signature**, so all call sites that pass
`{ fileName: 'x.txt' }` must update to `{ file_name: 'x.txt' }`. tsc
will enforce this. Grep `createTempFile\|createUploadFile` in the
whole repo and update each call.

Add 2 Vitest regression tests.

### 5.4 DoD (frontend, all three branches combined)

- [ ] `grep -rE "\.(nameI18n|descriptionI18n|sortOrder|presetAgentType|enabledSkills|customSkillNames|disabledBuiltinSkills|contextI18n|promptsI18n|lastUsedAt)\b" src/` → 0
- [ ] `grep -rE "['\"](nameI18n|descriptionI18n|sortOrder|presetAgentType|enabledSkills|customSkillNames|disabledBuiltinSkills|contextI18n|promptsI18n|lastUsedAt)['\"]" src/` → 0 (excluding `tests/fixtures/` legacy-config samples used as migration inputs)
- [ ] `ipcBridge.ts` body-key search for `modelId|configId|fileName` inside `(p) => ({ ... })` blocks → 0 (local param names are fine)
- [ ] `bunx tsc --noEmit` zero errors
- [ ] `bun run test --run` all green
- [ ] `bun run lint --quiet` no new warnings
- [ ] Playwright all green

## 6. Rollout / Team Plan

### 6.1 Branch model

- **Backend** (aionui-backend repo): single branch
  `feat/assistant-snake-case`. All backend work lands here.
- **Frontend** (AionUi repo): three branches:
  - `feat/assistant-snake-case` (heavy — §5.1)
  - `fix/acp-camelcase-hotfix` (light — §5.2)
  - `fix/fs-temp-camelcase-hotfix` (light — §5.3)
- **Coordinator branch** (AionUi): `feat/backend-migration-coordinator`
  — merges all three frontend branches and drives packaging smoke.

### 6.2 Roles

- **coordinator (me)** — spec, plan, task dispatch, mid-point check,
  branch merge, packaging smoke, handoff.
- **backend-dev** — T1 (all backend changes §4).
- **frontend-dev** — T2a (§5.1), T2b (§5.2), T2c (§5.3), serial.
- **e2e-tester** — T3 (Playwright end-to-end + assistants_e2e +
  skills_builtin_e2e rerun).

### 6.3 Task DAG

```
T1 (backend)                T2b (acp)            T2c (fs-temp)
     │                           │                     │
     └──→ T2a (assistant FE) ←───┴──→ (independent)   │
                  │                                   │
                  └──→ T3 (e2e) ←────────────────────┘
                              │
                              └──→ T4 (coordinator merge + smoke + handoff)
```

- T2b / T2c do not block on T1 (backend is already snake-case for
  those endpoints; only frontend flip needed).
- T2a depends on T1 (backend wire must be snake first so frontend
  assertions + E2E can be flipped against a working contract).
- T3 depends on T1, T2a, T2b, T2c.
- T4 depends on T3.

### 6.4 Task table

| Task | Owner | Depends on | Deliverable |
|---|---|---|---|
| T1 | backend-dev | — | `feat/assistant-snake-case` backend: §4 complete, pushed |
| T2a | frontend-dev | T1 | `feat/assistant-snake-case` AionUi: §5.1 complete, pushed |
| T2b | frontend-dev | — | `fix/acp-camelcase-hotfix` AionUi: §5.2 complete, pushed |
| T2c | frontend-dev | — | `fix/fs-temp-camelcase-hotfix` AionUi: §5.3 complete, pushed |
| T3 | e2e-tester | T1 + T2a + T2b + T2c | Playwright all green; report in `AionUi/docs/backend-migration/runs/YYYY-MM-DD-assistant-snake-case-e2e.md` |
| T4 | coordinator | T3 | Merge all three AionUi branches into `feat/backend-migration-coordinator`; packaging smoke; handoff |

### 6.5 Critical path

T1 → T2a → T3 → T4. T2b + T2c fit inside the T2a window without
extending critical path.

### 6.6 Mid-point coordinator verification (after T1 push)

Before unblocking T2a, coordinator probes live:

```bash
curl -s http://127.0.0.1:25905/api/assistants | jq '.data[0] | keys'
# expect: ["description", "enabled", "id", "name", "preset_agent_type",
#          "sort_order", "source", ...]

curl -s -X POST ... -d '{"name":"X","preset_agent_type":"gemini"}'
# expect: 201 Created

curl -s -X POST ... -d '{"name":"X","presetAgentType":"gemini"}' | jq '.data.preset_agent_type'
# expect: "" (serde silently ignores unknown keys; field stays default,
#           proving camelCase key is NOT being aliased anymore)
```

Catches missed structs early.

### 6.7 PRs

No PRs raised, per user convention. All branches push to origin only.

## 7. Risks & Mitigations

| # | Risk | Trigger | Mitigation |
|---|---|---|---|
| R1 | Bulk rename misses a site | codemod doesn't cover dynamic `assistant[key]` or string-form field references | DoD §5.4 grep bracket; tsc strict compile surfaces property-access misses; Vitest + E2E provide final gate |
| R2 | T2a stuck — 209 sites too large | frontend-dev times out | spawn prompt requires 10-min progress report; if >2h without push, coordinator checks status and may split T2a.1 (codemod) / T2a.2 (manual) |
| R3 | `assistants.json` rewrite damages value strings | naive sed catches value-side "enabledSkills" inside an English description field | Use `jq walk` (§4.4) — key-only rewrite, zero value-string risk |
| R4 | `migrateAssistants.ts` field mix-up drops data | legacy camel + new snake intermingle in same function | §5.1.5 requires a standalone `legacyAssistantToCreateRequest` mapping function + Vitest with real legacy config fixture (migration-playbook lesson: real-user-data dry-run) |
| R5 | `builtin.rs` attribute removed but `assistants.json` not updated | backend-dev changes Rust only | T1 DoD includes `cargo test -p aionui-assistant` — builtin loading tests fail loudly if JSON not updated |
| R6 | E2E fixture has independent camelCase residue | `.json` or `.ts` fixtures outside the main source tree | T1 DoD's grep is scoped to both `assets/` and `tests/`; e2e-tester T3 runs `assistants_e2e` independently as cross-check |
| R7 | Coordinator merge conflicts across 3 frontend branches | `ipcBridge.ts` modified by all 3 | Merge order: T2b + T2c first (small), T2a last (large); resolve with `git checkout --theirs` (pilot-side wins) — skill pilot precedent |
| R8 | `~/.cargo/bin/aionui-backend` symlink stale | backend-dev rebuilds but doesn't refresh symlink | T1 spawn prompt explicitly requires symlink refresh; coordinator T4 smoke uses `mktemp -d` + direct binary copy, bypassing symlink entirely |
| R9 | `stat -f` on symlink reports wrong mtime | runbook reads link mtime not target | Use `stat -L` / `ls -laL` — playbook-documented |
| R10 | frontend-dev zombie during T2a | idle 10+ min with no git/TaskUpdate progress | 10-min spot-check by coordinator; zombie detection threshold unchanged; autonomous replacement (playbook) |
| R11 | "Task complete" reported before push | teammate's mental model treats tests-pass as done | spawn prompt mandates "complete = tests pass AND pushed AND upstream sync verified"; coordinator runs `git log origin/<branch>` to verify SHA before acknowledging |
| R12 | ACP / fs-temp hotfix misses call sites | wrappers or indirect callers of `createTempFile` | tsc enforces because signature changes; T2b/T2c DoD requires repo-wide grep of `createTempFile\|createUploadFile\|setModel\|setConfigOption` to confirm |

## 8. Definition of Done (whole pilot)

See individual checklists: §4.8 (backend), §5.4 (frontend). Rollup:

### Code

- §4.8 all boxes checked
- §5.4 all boxes checked

### Integration

- [ ] Playwright all green
- [ ] e2e-tester report in `AionUi/docs/backend-migration/runs/`

### Packaging smoke (coordinator T4)

- [ ] `cargo build --release` → `mktemp -d` → copy binary alone (no sibling `assets/`)
- [ ] Launch: `$TMPDIR/aionui-backend --local --port 25905 --data-dir $TMPDIR/data`
- [ ] `GET /api/assistants` → row keys all snake_case
- [ ] `POST /api/assistants` with snake body → 201
- [ ] `POST /api/assistants` with camel body → 201 but `preset_agent_type` stays default empty string (serde silently ignores unknown `presetAgentType`; the test is that camelCase is NOT being aliased — if it were, the field would be populated)
- [ ] `GET /api/assistants` returns all 20 built-in entries (proves embedded JSON loaded)

### Merge

- [ ] 3 AionUi branches merged into `feat/backend-migration-coordinator`
- [ ] backend branch `feat/assistant-snake-case` — no merge needed, used directly
- [ ] coordinator handoff at `AionUi/docs/backend-migration/handoffs/coordinator-assistant-snake-case-YYYY-MM-DD.md`
- [ ] Repo-wide camelCase audit confirms remaining instances are only external-protocol (channel/plugins/{weixin,dingtalk})

### Out-of-scope (explicitly deferred)

- Serde alias / backward-compat shim
- Audit of other endpoints beyond §1's three
- Other domain camelCase (none known; channel/plugins stays)
