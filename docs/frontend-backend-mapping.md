# CoreSwift CRM — Frontend-to-Backend Route Mapping

Generated: 2026-07-13

## API Base: `/api/` (port 8084)

| Frontend Page | JS Render Function | Backend Route(s) | Methods | Status |
|---|---|---|---|---|
| Dashboard | `renderDashboard` | `/api/dashboard/stats` | GET | ✅ |
| | | `/api/contacts` | GET (fallback) | ✅ |
| | | `/api/companies` | GET (fallback) | ✅ |
| | | `/api/pipelines/deals` | GET (fallback) | ✅ |
| Contacts | `renderContacts` | `/api/contacts` | GET, POST | ✅ |
| | | `/api/contacts/:id` | PATCH | ✅ |
| Companies | `renderCompanies` | `/api/companies` | GET, POST | ✅ |
| | | `/api/companies/:id` | PATCH | ✅ |
| Pipelines | `renderPipelines` | `/api/pipelines` | GET, POST | ✅ |
| | | `/api/pipelines/:id` | PATCH | ✅ |
| Deals | `renderDeals` | `/api/pipelines/deals` | GET, POST | ✅ |
| | | `/api/pipelines/deals/:id` | PATCH | ✅ |
| Activities | `renderActivities` | `/api/activities` | GET | ✅ |
| | | `/api/events` | GET (fallback) | ✅ |
| Portfolio | `renderPortfolio` | `/api/portfolio` | GET | ✅ |
| Campaigns | `renderCampaigns` | `/api/campaigns` | GET, POST | ✅ |
| | | `/api/campaigns/:id` | PATCH | ✅ |
| | | `/api/campaigns/:id/activate` | POST | ✅ |
| Email Sequences | `renderEmailSequences` | `/api/campaigns` | GET | ✅ |
| | | `/api/campaigns/:id` | GET | ✅ |
| | | `/api/campaigns/:id/enrollments` | GET | ✅ |
| | | `/api/campaigns/:id/activate` | POST | ✅ |
| | | `/api/campaigns/:id/pause` | POST | ✅ |
| Automation Rules | `renderAutomationRules` | `/api/automation/rules` | GET, POST | ✅ |
| | | `/api/automation/rules/:id` | PATCH, DELETE | ✅ |
| Checklists | `renderChecklists` | `/api/checklists/templates` | GET, POST | ✅ |
| | | `/api/checklists/templates/:id` | PATCH, DELETE | ✅ |
| Call Logs | `renderCallLogs` | `/api/telnyx/call-logs` | GET | ✅ |
| Lists | `renderLists` | `/api/lists` | GET, POST | ✅ |
| | | `/api/lists/:id` | PATCH, DELETE | ✅ |
| | | `/api/lists/:id/members` | GET, POST | ✅ |
| | | `/api/lists/:id/members/:contact_id` | DELETE | ✅ |
| Agency Sequences | `renderAgencySequences` | `/api/automation/rules?agency=true` | GET | ✅ |
| Directory Onboarding | `renderDirectoryOnboarding` | `/api/checklists/instances?directory=true` | GET | ✅ |
| Health Monitor | `renderHealthMonitor` | `/api/monitoring/account-health/check` | POST | ✅ |
| | | `/api/monitoring/health` | GET | ✅ |
| Message Templates | `renderMessageTemplates` | `/api/comms/templates` | GET, POST | ✅ |
| | | `/api/comms/templates/:id` | PATCH, DELETE | ✅ |
| Nurture Campaigns | `renderNurtureCampaigns` | `/api/campaigns` | GET | ✅ |
| | | `/api/campaigns/:id/enrollments` | GET | ✅ |
| Telnyx Config | `renderTelnyxConfig` | `/api/telnyx/config` | GET, PUT | ✅ |
| | | `/api/telnyx/available` | GET | ✅ |
| Feature Management | `renderFeatureManagement` | `/api/billing/features` | GET | ✅ |
| System Health | `renderSystemHealth` | `/api/health` | GET | ✅ |
| Settings | `renderSettings` | (localStorage only) | — | ✅ |
| Tenants | `renderTenants` | `/api/admin/tenants` | GET | ✅ |
| | | `/api/admin/tenants/:id` | PATCH | ✅ |
| | | `/api/admin/tenants/:id/users` | GET | ✅ |
| | | `/api/admin/tenants/:id/plan` | PATCH | ✅ |
| | | `/api/admin/impersonate` | POST | ✅ |
| Plans | `renderPlans` | `/api/billing/plans` | GET, POST | ✅ |
| | | `/api/billing/plans/:id` | PATCH, DELETE | ✅ |
| Audit | `renderAudit` | `/api/audit` | GET | ✅ |

## Auth & Session
- Login: `POST /api/auth/login` → returns `{access_token, user}` ✅
- Me: `GET /api/auth/me` → returns `{id, name, email, role, tenant_id, features}` ✅
- Token stored in localStorage as `crm_token` ✅
- Impersonation: backup token in `crm_admin_token` ✅

## Plan Features (18 total)
All feature flags editable in Plans page:
`api_access, ai, campaigns, white_label, portfolio, automation, sms, email_sequences, checklists, directory_onboarding, account_health, crm_migration, call_logs, message_templates, nurture_campaigns, agency_sequences, lists, advanced_reports`

Plus limit fields: contacts, deals, users (stored on plan as max_contacts, max_deals, max_users)

## Notes
- The `renderAutomationRules` and `renderChecklists` use JSON textarea fields for trigger/action/stage config for flexibility
- Mobile responsive: sidebar collapses to 60px at <768px
- Feature gating: elements with `data-feature` attribute are hidden if not in user's plan features
- Free access model (no "trial" language used anywhere)
- Accordion menus use pure CSS max-height transitions
