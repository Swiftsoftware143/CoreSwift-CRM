# CoreSwift CRM - Task Board
_Last updated: 2026-06-25_

> ℹ️ **Renamed from CRM Swift → CoreSwift CRM** (June 25, 2026)
> Future: Drop "CRM" when established, just **CoreSwift**

## Legend
- ✅ Done
- 🔄 In Progress
- ⏳ Next Up
- 📅 Future / Stretch

---

## 🟢 Phase 1: Foundation (Done)

### Core
- ✅ Project scaffolding - Axum REST API, 80 source files, ~7,600 lines Rust
- ✅ Multi-tenant architecture (shared DB, scoped by `tenant_id`)
- ✅ Auth system - JWT access + refresh tokens, Argon2 password hashing
- ✅ RBAC - owner, admin, member roles per tenant
- ✅ Config centralized in `config.rs` with dotenv

### Database
- ✅ 24 SQLx migrations from scratch
- ✅ All FK relationships with cascade/set-null rules
- ✅ Seed data for pipelines, stages, plan tiers

### API Modules
- ✅ Tenants CRUD
- ✅ Users CRUD + invitations
- ✅ Contacts CRUD + search
- ✅ Companies CRUD
- ✅ Pipelines & Stages CRUD
- ✅ Opportunities (deals) through pipeline stages
- ✅ Tags with JSONB metadata & tag mappings
- ✅ Smart Lists with membership tracking
- ✅ Score rules + contact scoring
- ✅ Automation rules engine (If-Then triggers)
- ✅ Integrations (OAuth config storage)
- ✅ Webhooks
- ✅ Event bus with delayed actions
- ✅ Checklists & health monitoring
- ✅ Follow-up queue (scheduled messages)
- ✅ Audit log

### Deployment
- ✅ Dockerfile (multi-stage build, 1.81-alpine → alpine:3.19)
- ✅ Docker Compose (Postgres 16, Redis 7, Mailpit, App)
- ✅ `.env.dev.example` / `.env.example` (real env files are gitignored and never published)
- ✅ `.gitignore`
- ✅ Makefile + PowerShell helper
- ✅ Code verification report (`VERIFICATION.md`)

---

## 🟡 Phase 2: Communications & Monetization (Done)

### Communications
- ✅ Mailgun provider (REST API integration)
- ✅ SMTP.com provider (email backup)
- ✅ Telnyx provider (SMS)
- ✅ Per-tenant provider config via `PATCH /api/comms/providers`
- ✅ Outbound message queue in `communications_outbound` table
- ✅ Delivery status tracking (pending → delivered/failed)
- ✅ Mailpit for local email capture in dev

### AI Integration
- ✅ AI router with DeepSeek (primary) → OpenAI → Anthropic (fallback)
- ✅ Multi-LLM automatic fallback on failure
- ✅ AI-powered message composition
- ✅ Per-tenant LLM API keys via `tenants.settings->'ai'->'providers'`
- ✅ Template fallback if all LLMs fail
- ✅ Churn risk scoring model (lead score 40% / inactivity 30% / signals 20% / trial 10%)
- ✅ AI channel selection (email vs SMS based on engagement history)
- ✅ 10 context-specific templates with urgency tiers
- ✅ Human escalation at >70% churn probability

### Credit & Billing
- ✅ 4 plan tiers: Free (200 credits), Starter ($29/2k), Pro ($79/10k), Enterprise ($199/50k)
- ✅ Credit consumption per action (1-10 credits)
- ✅ Overage pricing per tier
- ✅ Feature flags per tier (AI gated behind Pro+)
- ✅ `credit_transactions` audit table
- ✅ API: balance, usage history, buy credits

---

## 🔵 Phase 3: Polish & Launch Prep

### ⏳ Before First Deploy
- [ ] **Install Rust toolchain** on dev machine
- [ ] **Run `cargo check`** to confirm compilation
- [ ] **Install Docker Desktop** (if not already)
- [ ] **`docker compose build app`** - verify Docker build
- [ ] **`docker compose up -d`** - full local spin-up test
- [ ] **Hit `localhost:8080/health`** - confirm API responds
- [ ] **Register a tenant** via POST /api/tenants
- [ ] **Create a user**, auth flow end-to-end
- [ ] **Team member management** — each tenant can have team members (users with roles like admin/member), limited by plan level
  - Basic plan: 1 user (the owner)
  - Pro plan: up to 5 team members
  - Enterprise: unlimited
  - This is NOT tenant sub-hierarchy — sister companies don't have tenants, they have team access
  - `users.invite` webhook action exists already
- [ ] **Run seed data** - confirm pipelines, plans, default templates load
- [ ] **Send a test email** - verify Mailpit captures it
- [ ] **Send a test SMS via Telnyx** (uses real API - test with a free credit)
- [ ] **SSL cert setup** - for production API domain

### ⏳ Security Hardening
- [ ] **Rate limiting** - verify AUTH_RATE_LIMIT_PER_MINUTE and API_RATE_LIMIT_PER_MINUTE work
- [ ] **Input validation** - audit all POST/PATCH/PUT handlers for missing validation
- [ ] **CORS** - tighten to specific origins in production
- [ ] **JWT rotation** - add token blacklist on password change
- [ ] **SQL injection** - verify all queries use sqlx parameterized binds (not format!/raw strings)
- [ ] **Audit log review** - make sure sensitive actions (delete, role change, payment) all log

---

## 🟣 Phase 4: Feature Gaps & Enhancements

### ✅ Self-Service Account Onboarding (Done - Migration 026)
- ✅ Every person who signs up gets their own isolated account (tenant) - no shared accounts
- ✅ Admins and tenants are the same: both are account holders with their own login
- ✅ Register creates a new tenant automatically (auto-generates name/slug from email)
- ✅ Register returns tenant info + next steps in response
- ✅ First user in tenant gets "owner" role (full admin access)
- ✅ Invite system - owners/admins can create invite links to add team members
  - `POST /api/auth/invite` - create invite (role: admin/member, 7-day expiry)
  - `GET /api/auth/invites` - list active invites
  - Register with `invite_token` joins that existing tenant
- ✅ Clean separation: FunnelSwift admin, MissedCall admin, tenants - each their own account

### ✅ Native App Integration System (Done - Migration 025 + 6 connector files)
- ✅ 6 native app connectors with per-tenant credential storage
- ✅ AdaSwift (admin-only) - client portal, push contacts, trigger campaigns
- ✅ CheatLayer (admin-only) - RPA engine, trigger workflows
- ✅ FunnelSwift (admin+tenant) - push/pull leads, funnels
- ✅ WorkflowSwift (admin+tenant) - trigger/pull n8n workflows
- ✅ MissedCall Responder (admin+tenant) - push leads, pull conversations, trigger SMS replies
- ✅ Multi-Directory App (admin+tenant) - sync business listings, pull reviews/analytics
- ✅ Ada Campaign Triggers - replaces Mailgun for welcome emails + scan reports
  - Triggers: user_created, contact_created, account_activated, scan_complete
  - CRM Swift automation rules now fire Ada campaigns instead of raw email
- ✅ Each app gets its own connection login - separate API keys per app per tenant, isolated sync audit trails
- ✅ **Affiliate product board** - products/services with tags, commissions, checkout links
- ✅ **Public webhook** (`POST /api/webhook/{token}/{action}`) - single endpoint for WorkflowSwift to orchestrate all automation
  - Every tenant gets an auto-generated webhook token on signup
  - Supports 28+ actions across all features (contacts, tags, lists, pipelines, affiliates, comms, AI, events, billing)
  - Full audit log of every webhook call
  - Admins/tenants don't need direct OpenClaw/n8n/CheatLayer access - they connect once to WorkflowSwift
  - WorkflowSwift uses this webhook internally to talk to CRM Swift

### 🏛 Affiliate Email Delivery (via Ada campaigns)
- CRM Swift also powers the in-house affiliate system's email delivery
- Instead of sending commission notifications / payout alerts / referral confirmations through raw SMTP:
  - Affiliate gets a new referral → CRM Swift triggers Ada campaign → Ada sends referral confirmation
  - Commission earned → CRM Swift triggers Ada campaign → Ada sends commission notification
  - Payout processed → CRM Swift triggers Ada campaign → Ada sends payout alert
- Uses the same Ada campaign trigger system already built
- Triggers supported: `referral_confirmed`, `commission_earned`, `payout_processed`, `affiliate_activated`

### ✅ Admin Chat Actions (Done - full business from Telegram)
- ✅ `POST /api/admin/chat-action` - single endpoint to drive the entire business
- ✅ `GET /api/admin/chat-action/intents` - discover all available actions
- ✅ **create_affiliate** - creates CRM Swift account + affiliate profile + code + login
- ✅ **create_affiliate_in_funnelswift** - creates FunnelSwift product + CRM Swift account + tag + Ada welcome campaign
- ✅ **create_tenant_account** - creates tenant + admin user + free plan + auto webhook token
- ✅ Missing field prompts - returns specific fields it needs so I can ask you in chat
- ✅ Multi-step flow - one intent auto-triggers across CRM Swift + FunnelSwift + AdaSwift
- ✅ Example: "create affiliate John Doe" → I prompt for email/rate → you reply → full setup done
- ✅ **Affiliate self-serve product selection** (Migration 028)
  - `GET /api/affiliates/my-products` - affiliates see what they're promoting + what's available
  - `POST /api/affiliates/my-products/select` - start promoting a product
  - `POST /api/affiliates/my-products/unselect` - stop promoting
  - Affiliates log into FunnelSwift back-end to pick which products to promote
  - Available via webhook: `affiliate_products.my`, `affiliate_products.select`, `affiliate_products.unselect`
- ✅ **tenants.create webhook action** - FunnelSwift calls `POST /api/webhook/{token}/tenants.create` with affiliate's name/email → CRM Swift auto-creates tenant + owner user + free plan + affiliate profile + code
  - This is how FunnelSwift triggers the account creation when someone signs up as an affiliate
  - No need for CRM Swift front-end signup - all automation originates from FunnelSwift level
- ✅ **12 new webhook actions filling remaining gaps** - complete automation coverage
  - `webhooks.generate` - Create a new webhook token (no chicken-and-egg problem; admin token exists)
  - `webhooks.revoke` - Deactivate a token
  - `webhooks.list` - List all tokens (masked)
  - `pipelines.stages` - List stages for a pipeline
  - `pipelines.create_stage` - Create a new stage
  - `users.invite` - Invite a team member
  - `users.list` - List tenant users
  - `tenants.settings` - Get tenant config
  - `scoring.calculate` - Recalculate contact score
  - `analytics.contacts` - Contact analytics summary
  - `audit.log` - Recent audit log entries
  - `search.query` - Cross-entity search (contacts, tags, lists)
  - All 12 added to auto-generated webhook token allowed_actions in migration 027

### ⏳ Next Up
- ✅ Ada campaign triggers extended for affiliate events (done in migration 025)
  - `referral_confirmed`, `commission_earned`, `payout_processed`, `affiliate_activated`
- ✅ **Webhook action gaps filled** (12 new actions added to `actions.rs`)
  - `webhooks.generate`, `webhooks.revoke`, `webhooks.list`
  - `pipelines.stages`, `pipelines.create_stage`
  - `users.invite`, `users.list`
  - `tenants.settings`
  - `scoring.calculate`
  - `analytics.contacts`
  - `audit.log`
  - `search.query`
- [ ] **White-label multi-tenancy** - agencies resell to their own clients
  - Sub-tenant creation flow
  - Branding config (logo, colors, domain) per tenant
  - Agency dashboard with rollup reporting
- [ ] **Custom domain support** - per-tenant CNAME + SSL
- [ ] **Web UI** - admin dashboard (React/Vue/Svelte?) for non-API users

### ⏳ Medium Priority
- [ ] **Email templates** - HTML drag-and-drop editor or rich template system
- [ ] **SMS opt-in/opt-out** - compliance (TCPA, GDPR)
- [ ] **Two-factor auth** - TOTP or SMS codes
- [ ] **File uploads** - S3/R2 integration for attachments in communications
- [ ] **API key management** - tenants generate API keys for their own integrations
- [ ] **Webhook retries** - exponential backoff, dead letter queue
- [ ] **Bulk operations** - batch import/export contacts (CSV)

### ⏳ Lower Priority
- [ ] **Reporting & analytics dashboard** - pipeline conversion, churn trends, revenue
- [ ] **UI theme builder** - whitelabel tenants customize look & feel
- [ ] **Mobile push notifications** - Firebase/APNs integration
- [ ] **Calendar sync** - Google Calendar / Outlook integration
- [ ] **Zapier/Make.com connector** - webhook-based external automation
- [ ] **Public API docs** - OpenAPI/Swagger generation
- [ ] **Horizontal scaling** - add nginx/reverse-proxy, read replicas, app auto-scaling

---

## 🚀 Phase 5: Production Deploy

### ⏳ Infrastructure
- [ ] **Choose hosting** - VPS (Linode/DigitalOcean/AWS EC2) or container platform (Railway/Render/Fly)
- [ ] **Managed Postgres** - production DB (RDS, Cloud SQL, or managed on Railway)
- [ ] **Managed Redis** - production cache (Upstash or Railway Redis)
- [ ] **CI/CD** - GitHub Actions: test → build → push → deploy
- [ ] **Domain + DNS** - point API domain to your server
- [ ] **Reverse proxy** - Caddy or nginx for SSL termination
- [ ] **Monitoring** - health checks, uptime monitoring, error tracking (Sentry)
- [ ] **DB backups** - automated daily, retention policy

### ⏳ Launch Checklist
- [ ] **Load test** - k6 or locust, confirm it handles target traffic
- [ ] **Credit card processing** - Stripe/Paddle integration for plan purchases + credit top-ups
- [ ] **Invoice generation** - PDF invoice on payment
- [ ] **Terms of Service + Privacy Policy** - public pages
- [ ] **Waitlist / beta signup** - controlled launch
- [ ] **Go live** 🚀
