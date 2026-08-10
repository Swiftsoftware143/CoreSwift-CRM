# CRM Swift — Quality & Performance Requirements

Apply the same standards as WorkflowSwift (the main app):

## Git & Filesystem
- git config core.longpaths true (already set)
- Use Git Bash for git operations on Windows to avoid MAX_PATH issues
- CRLF handling: git config core.autocrlf true (Windows native)

## Security
- Use argon2 for password hashing (already in Cargo.toml)
- JWT with refresh tokens (short-lived access, long-lived refresh)
- All SQL queries parameterized (SQLx does this by default)
- CORS configured per tenant domain
- Rate limiting on auth: 5 req/min for login, 20/min for API
- XSS protection: all API responses as JSON only (no HTML rendering)
- SQL injection: prevented by SQLx compile-time checks
- Tenant isolation: EVERY query WHERE tenant_id = ?
- Input validation: serde + validator crate for all handlers
- Helmet-like headers: X-Content-Type-Options, X-Frame-Options, etc.

## Performance
- Connection pooling: sqlx::PgPool with min 5 / max 20 connections
- Redis caching for: score results (5 min TTL), list membership (2 min TTL), user sessions
- Pagination on ALL list endpoints (limit/offset, max 100 per page)
- JSON serialization: serde with #[serde(rename_all = "camelCase")]
- Gzip compression: tower-http compression middleware
- Response caching headers for static-ish data
- DB indexes on: tenant_id, (tenant_id, stage_id), (entity_type, entity_id), created_at

## Architecture
- Layered: handlers → services → repository
- Error handling: single AppError enum with proper HTTP mapping
- Logging: tracing crate with structured fields (tenant_id, request_id, user_id)
- Request ID middleware (uuid per request)
- Graceful shutdown with tokio signal handling

## Docker
- Multi-stage build: builder → distroless runtime (or alpine)
- docker-compose with PostgreSQL 16 + Redis 7 + timeout configs
- Healthcheck endpoints: GET /health, GET /ready
- .env.example with all config

## Testing
- Unit tests alongside modules (#[cfg(test)] mod tests)
- Integration tests in tests/ directory

## Code Quality
- No unwrap() in production code — proper error handling everywhere
- Doc comments on all public functions
- Clean module structure (mod.rs re-exports)
