# CRM Swift — PowerShell Dev Helper
# Source in your PowerShell profile or dot-source as needed
# . .\crm-swift.ps1
# Then use commands like: Start-CRM, Build-CRM, etc.

$PROJECT = "crm-swift"

function Build-CRM {
    docker compose -f "$PSScriptRoot\docker-compose.yml" build app
}

function Start-CRM {
    docker compose -f "$PSScriptRoot\docker-compose.yml" up -d
}

function Stop-CRM {
    docker compose -f "$PSScriptRoot\docker-compose.yml" down
}

function Restart-CRM {
    Build-CRM
    Start-CRM
}

function Logs-CRM {
    docker compose -f "$PSScriptRoot\docker-compose.yml" logs -f
}

function Status-CRM {
    docker compose -f "$PSScriptRoot\docker-compose.yml" ps
}

function Wipe-CRM {
    docker compose -f "$PSScriptRoot\docker-compose.yml" down -v
}

function Db-CRM {
    docker compose -f "$PSScriptRoot\docker-compose.yml" exec postgres psql -U crm_swift crm_swift
}

function Redis-CRM {
    docker compose -f "$PSScriptRoot\docker-compose.yml" exec redis redis-cli
}

function Mailpit-CRM {
    Start-Process "http://localhost:8025"
}

function Clean-CRM {
    docker compose -f "$PSScriptRoot\docker-compose.yml" down -v
    docker image prune -f
}

Export-ModuleMember -Function Build-CRM, Start-CRM, Stop-CRM, Restart-CRM, Logs-CRM, Status-CRM, Wipe-CRM, Db-CRM, Redis-CRM, Mailpit-CRM, Clean-CRM
