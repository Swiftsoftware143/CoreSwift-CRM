# ============================================================
# CoreSwift CRM — PRODUCTION Dockerfile (canonical deploy path)
# ============================================================
# Build strategy: HOST-BUILD the binary, then COPY it into a
# minimal Ubuntu image. Do NOT `cargo build` inside the image.
#
# Deploy flow (run on the VPS):
#   1. /root/.cargo/bin/cargo build --release     # in repo root (host toolchain)
#   2. cp target/release/crm-swift  <context>/crm-swift
#   3. cp -r migrations             <context>/migrations
#   4. docker build -t crm-swift:latest <context>
#   5. docker compose up -d --force-recreate    # in /opt/swift/docker/crm-swift/
#
# Authoritative deploy context: /opt/swift/docker/crm-swift/
# (host-built binary + migrations are staged there, NOT in git).
# ============================================================
FROM ubuntu:24.04
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates libssl3 curl && rm -rf /var/lib/apt/lists/*
RUN groupadd -r crm-swift && useradd -r -g crm-swift crm-swift
WORKDIR /app
COPY crm-swift /app/crm-swift
COPY migrations /app/migrations
COPY .env.example /app/.env.example
RUN chmod +x /app/crm-swift && chown -R crm-swift:crm-swift /app
USER crm-swift
EXPOSE 8084
CMD ["/app/crm-swift"]
