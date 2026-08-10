# CoreSwift CRM — Handoff to Linux Agent (June 25, 2026)

## The Product
**Name:** CoreSwift CRM (renamed from CRM Swift, June 25, 2026)
**Future name:** Just **CoreSwift** (drop "CRM" once established)
**What it is:** Multi-tenant operating system that sits at the center of all SwiftSoftware apps — FunnelSwift, WorkflowSwift, AdaSwift, MissedCall Responder, CheatLayer. Handles CRM, automation, AI, communications, billing, and integrations.

## The Repo
- **GitHub:** `Swiftsoftware204/CRMSwift` (rename repo to `CoreSwift` when ready)
- **Clone:** `https://github.com/Swiftsoftware204/CRMSwift.git`
- **Branch:** `master`
- **Token in remote URL:** `ghp_XXX...XXX` (already set in origin)
- **Local path:** `C:\Users\Administrator\.openclaw\workspace\crm-swift` (SwiftSoftware workspace)

## What's in the Repo NOW (already committed)
Single commit `bb04b47` — "Initial commit - CRMSwift full Rust API backend with 29 migrations"
- Axum REST API, ~7,600 lines Rust, 80 source files
- 29 SQLx migrations
- Multi-tenant architecture
- JWT auth + Argon2 password hashing
- Docker + docker-compose (Postgres 16, Redis 7, Mailpit)
- Modules present: auth, tenants, contacts, companies, pipelines, tags, scoring, lists, automation, integrations, analytics, ai, billing, affiliates, audit, events, communications, checklists, monitoring, notifications, native_apps, admin_actions, campaigns, webhook, worker

## Features That Need TO BE WRITTEN (NOT in the repo — only spec'd in TASKS.md)

### 1. Self-Service Account Onboarding (Migration 026)
- Auto-create tenant on register
- Return tenant info in signup response
- Invite system: POST /api/auth/invite, GET /api/auth/invites
- Register with invite_token joins existing tenant
- Need: migration file + handler changes

### 2. Native App Connectors (Migration 025 + 6 connector files)
Already have the module structure and connector files at:
- `src/native_apps/connectors/adaswift.rs`
- `src/native_apps/connectors/cheatlayer.rs`
- `src/native_apps/connectors/funnelswift.rs`
- `src/native_apps/connectors/workflowswift.rs`
- `src/native_apps/connectors/missedcall_responder.rs`
- `src/native_apps/connectors/multi_directory.rs`

BUT these need to be verified working — they exist in the commit as file stubs.

### 3. Admin Chat Actions (single endpoint to run business from Telegram)
- POST /api/admin/chat-action
- GET /api/admin/chat-action/intents
- create_affiliate, create_affiliate_in_funnelswift, create_tenant_account
- Missing field prompting, multi-step flows

### 4. Webhook System (28+ actions)
- Every tenant gets auto-generated webhook token on signup
- POST /api/webhook/{token}/{action} — single endpoint for WorkflowSwift
- Actions: contacts.*, tags.*, lists.*, pipelines.*, affiliates.*, comms.*, ai.*, events.*, billing.*, webhooks.*, users.*, tenants.settings, scoring.calculate, analytics.contacts, audit.log, search.query

### 5. Ada Campaign Triggers
- Replace Mailgun for welcome emails, scan reports
- Triggers: user_created, contact_created, account_activated, scan_complete, referral_confirmed, commission_earned, payout_processed, affiliate_activated

### 6. Affiliate Self-Serve Product Selection (Migration 028)
- GET /api/affiliates/my-products
- POST /api/affiliates/my-products/select
- POST /api/affiliates/my-products/unselect
- tenants.create webhook action (FunnelSwift triggers account creation)

### 7. Rename Everything
- Package name in Cargo.toml → "coreswift"
- README, docs, comments → "CoreSwift CRM" / "CoreSwift"
- Repo name → CoreSwift (or CoreSwiftCRM)

### 8. Deployment
- Linux agent already has this deployed to a VPS — verify what's running
- Docker Compose on VPS with Postgres 16 + Redis 7
- SSL cert
- CI/CD (GitHub Actions)

## Key Differences From Current Repo State
| Feature | In Repo? | Notes |
|---------|----------|-------|
| Base CRM (contacts, pipelines, tags, etc.) | ✅ Yes | Already committed |
| Communications (Mailgun, SMTP, Telnyx) | ✅ Yes | Modules exist |
| AI Engine (DeepSeek/OpenAI/Anthropic) | ✅ Yes | Router + scoring |
| Billing & Credits | ✅ Yes | Plans, tiers, transactions |
| **Self-Service Onboarding** | ❌ No | Never committed |
| **Native App Connectors** | ❌ Only stubs | 6 connector files exist but unverified |
| **Admin Chat Actions** | ❌ No | Never committed |
| **Full Webhook System** | ❌ No | Never committed |
| **Ada Campaign Triggers** | ❌ No | Never committed |
| **Affiliate Product Selection** | ❌ No | Never committed |
| **Phase 3 Launch Prep** | ❌ No | All checklist items pending |
| **Rename to CoreSwift** | ❌ No | Docs updated locally only |

## GitHub Token
```
ghp_XXX...XXX
```
Already set as remote URL credential. If it's expired, get a new one from David.

## Order of Operations for Linux Agent
1. Pull the repo (Swiftsoftware204/CRMSwift)
2. Build and test (`cargo check` / `cargo build`)
3. Write and test all Phase 4 features (migrations + handlers)
4. Rename the project to CoreSwift (Cargo.toml, comments, repo name)
5. Deploy to VPS via Docker Compose
6. Verify all endpoints working
