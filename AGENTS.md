# AGENTS.md — Vibe Engineering Rules for AI Agents

## Rust Guardrails (MANDATORY)
- **Zero unsafe blocks** unless explicitly approved by the Lead Architect
- **Zero .unwrap() or .expect()** in non-test production code — use `thiserror`/`anyhow`
- **All async state must implement Send + Sync**
- **Parameterized SQL only** — use `sqlx::query_as!` for compile-time validation
- **Secrets in env vars only** — never hardcoded
- **cargo fmt** before commit

## Verification Sequence (NON-NEGOTIABLE)
After ANY code change:
1. `cargo check` — syntax + borrow checker. Read stderr. Fix. Repeat until clean.
2. `cargo test` — all tests must pass
3. `cargo clippy -- -D warnings` — zero warnings tolerated
4. `cargo fmt -- --check` — formatting must be consistent

## Self-Correction Loop
- Compiler error → read diagnostic → understand → fix → re-compile
- Test failure → fix logic → re-run
- Clippy warning → clean up → re-run
- **NEVER paste errors to a human. FIX THEM.**
- 3 attempts max, then escalate with evidence of what you tried.

## Hermes Delegation Pattern
For complex feature implementation:
1. Draft trait signatures and types FIRST
2. Run `cargo check` to validate types before writing method bodies
3. Then implement method logic — iterate with check/test/clippy
4. Re-run full verification before declaring done

## Build Lock Protocol
- ALWAYS use `/opt/swift/build-lock.sh <app> <command>`
- Never raw `cargo build --release` on shared repos
- Exit 2 = another bot building → wait 30s, retry once
- Stale lock >30min: clear and proceed

## Post-Deploy Smoke Test
- `curl -s -o /dev/null -w "%{http_code}" <domain>` must return 200

## Project File Architecture
```
src/account/handlers.rs
src/account/mod.rs
src/account/models.rs
src/account/settings.rs
src/activities/handlers.rs
src/activities/mod.rs
src/admin_actions/handlers.rs
src/admin_actions/mod.rs
src/affiliates/handlers.rs
src/affiliates/mod.rs
src/affiliates/models.rs
src/ai/engine.rs
src/ai/handlers.rs
src/ai/mod.rs
src/ai/models.rs
src/ai/router.rs
src/analytics/handlers.rs
src/analytics/mod.rs
src/audit/handlers.rs
src/audit/logger.rs
src/audit/mod.rs
src/auth/handlers.rs
src/auth/middleware.rs
src/auth/mod.rs
src/auth/models.rs
src/automation/actions.rs
src/automation/engine.rs
src/automation/handlers.rs
src/automation/mod.rs
src/automation/models.rs
src/billing/credits.rs
src/billing/handlers.rs
src/billing/mod.rs
src/billing/models.rs
src/bookings/handlers.rs
src/bookings/mod.rs
src/bookings/models.rs
src/campaigns/handlers.rs
src/campaigns/mod.rs
src/campaigns/models.rs
```
