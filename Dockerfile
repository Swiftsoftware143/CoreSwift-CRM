# Build stage
FROM rust:1.97-alpine AS builder

RUN apk add --no-cache musl-dev pkgconfig openssl-dev

WORKDIR /app

# Copy manifests
COPY Cargo.toml Cargo.lock* ./

# Create a dummy main.rs to build dependencies first
RUN mkdir src && echo "fn main() {}" > src/main.rs
RUN cargo build --release 2>/dev/null || true

# Copy the real source
COPY src/ src/
COPY migrations/ migrations/

# Touch main.rs to force rebuild
RUN touch src/main.rs

# Build for real
RUN cargo build --release

# Runtime stage
FROM alpine:3.19

RUN apk add --no-cache ca-certificates libgcc

WORKDIR /app

# Copy binary (build stage produces a Linux binary, no .exe suffix)
COPY --from=builder /app/target/release/crm-swift /app/crm-swift
COPY --from=builder /app/migrations/ /app/migrations/
COPY .env.example /app/.env

EXPOSE 8080

CMD ["/app/crm-swift"]
