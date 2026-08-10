# CRM Swift — Development Makefile
# Works with GNU Make (Linux, macOS, Windows via WSL or chocolatey)

.PHONY: help build up down restart logs ps clean db-shell redis-shell mailpit push

SHELL := /bin/bash

help:          	## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-15s\033[0m %s\n", $$1, $$2}'

build:         	## Build the app container image
	docker compose build app

up:            	## Start all services in background
	docker compose up -d

down:          	## Stop all services
	docker compose down

restart:       	## Rebuild and restart just the app
	docker compose build app && docker compose up -d

logs:          	## Tail all logs
	docker compose logs -f

ps:            	## Show running services
	docker compose ps

clean:         	## Stop everything and wipe volumes
	docker compose down -v

db-shell:      	## Open psql in the running Postgres container
	docker compose exec postgres psql -U crm_swift crm_swift

redis-shell:   	## Open redis-cli in the running Redis container
	docker compose exec redis redis-cli

mailpit:       	## Open Mailpit web UI in default browser
	xdg-open http://localhost:8025

push:          	## Build and push to a registry (edit the tag)
	docker compose build app
	docker tag crm-swift-app:latest ghcr.io/your-org/crm-swift:latest
	docker push ghcr.io/your-org/crm-swift:latest
