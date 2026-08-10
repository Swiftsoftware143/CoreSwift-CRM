# GUARDRAILS.md — CoreSwift CRM

**Rust Guardrails — Vibe Engineering Standard**

## Non-Negotiable
- No `unwrap()` or `expect()` in production code paths.
- `contact_scores`: `calculated_at` -> `created_at` migration already applied — do not revert.
- Mailgun delivery requires tenant-configured API keys — never hardcode.
- All new migrations: sequential numbers, never re-use.
- `cargo clippy -- -D warnings` must pass.
- Build through `/usr/local/bin/swift-build.sh coreswift-crm`.

## Verification Before Deploy
1. `cargo check && cargo clippy -- -D warnings && cargo test`
2. `sqlx migrate run`
3. `curl localhost:8084/api/health`
