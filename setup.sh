#!/usr/bin/env bash
# ================================================================
# CRM Swift — Quick Start
# ================================================================
# Run: chmod +x setup.sh && ./setup.sh
# Prerequisites: Docker + Docker Compose v2
# ================================================================

set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

echo "╔══════════════════════════════════════════════════╗"
echo "║          CRM Swift — Setup                       ║"
echo "╚══════════════════════════════════════════════════╝"

# 1. Generate a secure JWT secret
JWT_SECRET=$(openssl rand -base64 48 2>/dev/null || echo "change…tion")
export JWT_SECRET

# 2. Create .env from example if not exist
if [ ! -f "$SCRIPT_DIR/.env" ]; then
    cp "$SCRIPT_DIR/.env.example" "$SCRIPT_DIR/.env"
    # Inject generated secret
    sed -i.bak "s|JWT_SECRET=.*|JWT_SECRET=${JWT_SECRET}|" "$SCRIPT_DIR/.env"
    rm -f "$SCRIPT_DIR/.env.bak"
    echo "🔑 Generated JWT secret"
fi

echo "📦 Starting PostgreSQL + Redis + App..."
docker compose -f "$SCRIPT_DIR/docker-compose.yml" up -d

echo ""
echo "⏳ Waiting for services..."
until curl -sf http://localhost:8080/api/health > /dev/null 2>&1; do
    printf "."
    sleep 2
done
echo " ready!"

echo ""
echo "╔══════════════════════════════════════════════════╗"
echo "║  CRM Swift is running!                           ║"
echo "║                                                  ║"
echo "║  API:      http://localhost:8080                  ║"
echo "║  Health:   http://localhost:8080/api/health       ║"
echo "║  PG:       localhost:5432                         ║"
echo "║  Redis:    localhost:6379                         ║"
echo "║                                                  ║"
echo "║  To register:                                    ║"
echo "║    curl -X POST http://localhost:8080/api/auth/register \ ║"
echo "║      -H 'Content-Type: application/json'          ║"
echo "║      -d '{"name":"Admin","email":"admin@test.com","password":"password123","tenant_name":"My Agency","tenant_slug":"my-agency"}' ║"
echo "║                                                  ║"
echo "║  To stop:  docker compose down                    ║"
echo "║  Logs:     docker compose logs -f                  ║"
echo "╚══════════════════════════════════════════════════╝"
