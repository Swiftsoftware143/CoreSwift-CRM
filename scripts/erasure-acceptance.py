#!/usr/bin/env python3
"""Live acceptance — fleet account-closure/erasure standard §6, on CoreSwift (kanban t_48b65042).

docs/account-closure-erasure-standard-2026-11-24.md §6 says every app must prove, against the
RUNNING app:

  1. POST close route with a valid API key   -> 401 (never 200)
  2. POST close route with no credential     -> 401
  3. close route in no API_KEY_ROUTES / INTERNAL_ROUTES entry   (static: route_policy tests)
  4. no MCP tool names close/erase/delete_account               (static: route_policy tests)
  5. POST erase with a session but a WRONG password -> refused AND the tenant rows still exist
  6. successful erase -> rows gone, billing documents kept, one erasure audit row

Legs 3-4 are pinned as unit tests in `src/auth/route_policy.rs`; this script proves the behavioural
legs 1, 2, 5, 6 and a few more against the live binary, then cleans up after itself.

What it creates: ONE disposable workspace through the app's own signup door (harness header set, so
`tenants.probe_harness` marks it), with a contact and a list seeded so "rows gone" is a real
measurement rather than an empty-table shrug. A SECOND probe proves the close door still works and
that its refusals are selective — a guard that blocks everything looks identical to one that works.

Usage: python3 scripts/erasure-acceptance.py [base_url]
"""
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path
from urllib.parse import unquote, urlparse

BASE = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:8084").rstrip("/")
APP_ENV = Path("/etc/swift/env/coreswift.env")
DB_MAP = {"coreswift": "coreswift_crm"}

FAILED = []


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ((" — " + detail) if detail else ""))
    if not ok:
        FAILED.append(name)


def http(method, path, body=None, token=None, api_key=None):
    url = BASE + path
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method)
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", "Bearer " + token)
    if api_key:
        req.add_header("Authorization", "Bearer " + api_key)
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            raw = r.read().decode(errors="replace")
            try:
                return r.status, json.loads(raw) if raw else {}
            except json.JSONDecodeError:
                return r.status, raw
    except urllib.error.HTTPError as e:
        raw = e.read().decode(errors="replace")
        try:
            return e.code, json.loads(raw) if raw else {}
        except json.JSONDecodeError:
            return e.code, raw
    except urllib.error.URLError as e:
        raise SystemExit(f"cannot reach {url}: {e}")


def _dsn():
    for line in APP_ENV.read_text().splitlines():
        if line.startswith("DATABASE_URL="):
            return line.split("=", 1)[1].strip().strip('"')
    raise SystemExit(f"no DATABASE_URL in {APP_ENV}")


def sql(query):
    """One statement (or a `;`-joined batch) through the app's own Postgres, tuples only."""
    u = urlparse(_dsn())
    db = DB_MAP.get("coreswift") or unquote(u.path).lstrip("/")
    r = subprocess.run(
        ["docker", "exec", "-i", "-e", f"PGPASSWORD={unquote(u.password or '')}",
         "swift-postgres", "psql", "-U", unquote(u.username or "swift"), "-d", db,
         "-tAF", "|", "-v", "ON_ERROR_STOP=1", "-c", query],
        capture_output=True, text=True, timeout=60,
    )
    if r.returncode != 0:
        raise SystemExit(f"SQL failed: {r.stderr.strip()[:400]}\n--- {query[:200]}")
    return [line.split("|") for line in r.stdout.splitlines() if line.strip()]


def scalar(query):
    rows = sql(query)
    return rows[0][0] if rows else None


def count(table, tenant_id):
    return int(scalar(f"SELECT COUNT(*) FROM {table} WHERE tenant_id = '{tenant_id}'") or 0)


def register_probe(tag):
    email = f"{tag}@local.coreswift"
    password = "ProbeErase!2026" + tag[-4:]
    status, body = http("POST", "/api/auth/register", {
        "name": "Erasure Probe",
        "email": email,
        "password": password,
        "account_name": f"Erasure Probe {tag}",
        "account_slug": tag,
    })
    if status not in (200, 201):
        raise SystemExit(f"probe signup failed: {status} {body}")
    body = body if isinstance(body, dict) else {}
    return {
        "email": email,
        "password": password,
        "token": body["access_token"],
        "tenant_id": body["account"]["id"],
        "api_key": body["api_key"]["key"],
    }


def seed_rows(p):
    """A contact, a list and a second plan line, so 'rows gone' is measured against real rows."""
    sql(
        "INSERT INTO contacts (tenant_id, first_name, last_name, email) "
        f"VALUES ('{p['tenant_id']}', 'Erasure', 'Probe', '{p['email']}');"
        "INSERT INTO lists (tenant_id, name) "
        f"VALUES ('{p['tenant_id']}', 'Erasure Probe List');"
        "INSERT INTO plan_lines (tenant_id, plan_id, plan_slug, price_monthly, price_yearly, "
        "billing_cycle, started_at, ended_at, end_reason) SELECT tenant_id, plan_id, plan_slug, "
        "price_monthly, price_yearly, billing_cycle, started_at, NOW(), 'probe-extra' "
        f"FROM plan_lines WHERE tenant_id = '{p['tenant_id']}' LIMIT 1;"
    )


def snapshot(p):
    return {
        "users": count("users", p["tenant_id"]),
        "contacts": count("contacts", p["tenant_id"]),
        "lists": count("lists", p["tenant_id"]),
        "tenant_plans": count("tenant_plans", p["tenant_id"]),
        "plan_lines": count("plan_lines", p["tenant_id"]),
        "api_keys": count("personal_api_keys", p["tenant_id"]),
    }


def main():
    tag = "erasure-probe-" + time.strftime("%Y%m%d-%H%M%S")
    probe = register_probe(tag)
    print(f"probe workspace {probe['tenant_id']} ({probe['email']})")
    seed_rows(probe)
    before = snapshot(probe)
    print("before:", before)

    # ── 1. no credential, both doors ────────────────────────────────────────────────────────────
    s, _ = http("POST", "/api/auth/erase-data", {"password": probe["password"], "confirm": "DELETE"})
    check("1a no credential on erase -> 401", s == 401, f"HTTP {s}")
    s, _ = http("POST", "/api/auth/close-account", {"password": probe["password"], "confirm": "CLOSE"})
    check("1b no credential on close -> 401", s == 401, f"HTTP {s}")

    # ── 2. a VALID issued API key, both doors ───────────────────────────────────────────────────
    s, _ = http("POST", "/api/auth/erase-data", {"password": probe["password"], "confirm": "DELETE"},
                api_key=probe["api_key"])
    check("2a valid API key on erase -> 401", s == 401, f"HTTP {s}")
    s, _ = http("POST", "/api/auth/close-account", {"password": probe["password"], "confirm": "CLOSE"},
                api_key=probe["api_key"])
    check("2b valid API key on close -> 401", s == 401, f"HTTP {s}")
    check("2c refused by key: rows untouched", snapshot(probe) == before, str(snapshot(probe)))

    # ── 5. wrong password / wrong word: refused AND the rows still exist ───────────────────────
    s, b = http("POST", "/api/auth/erase-data",
                {"password": "definitely-not-the-password", "confirm": "DELETE"}, token=probe["token"])
    check("5a wrong password on erase -> 401", s == 401, f"HTTP {s} {b}")
    check("5b ...and every probe row still exists", snapshot(probe) == before, str(snapshot(probe)))
    check("5c ...and no erasure audit row was written",
          scalar(f"SELECT COUNT(*) FROM account_erasures WHERE erased_tenant_id = '{probe['tenant_id']}'") == "0")

    s, b = http("POST", "/api/auth/erase-data",
                {"password": probe["password"], "confirm": "delete everything"}, token=probe["token"])
    check("5d wrong typed word on erase -> 400", s == 400, f"HTTP {s} {b}")
    check("5e ...and every probe row still exists", snapshot(probe) == before, str(snapshot(probe)))

    # ── 6. the real thing ──────────────────────────────────────────────────────────────────────
    s, b = http("POST", "/api/auth/erase-data",
                {"password": probe["password"], "confirm": "DELETE"}, token=probe["token"])
    check("6a erase with session + password + DELETE -> 200", s == 200, f"HTTP {s} {str(b)[:200]}")
    bd = b if isinstance(b, dict) else {}
    if s == 200:
        check("6b reports mode=hard and the destroyed counts",
              bd.get("mode") == "hard" and bd.get("users_removed") == before["users"]
              and bd.get("contacts_removed") == before["contacts"]
              and bd.get("lists_removed") == before["lists"],
              json.dumps({k: bd.get(k) for k in ("mode", "users_removed", "contacts_removed", "lists_removed")}))
    after = snapshot(probe)
    check("6c tenant row is gone",
          scalar(f"SELECT COUNT(*) FROM tenants WHERE id = '{probe['tenant_id']}'") == "0")
    check("6d users/contacts/lists/plan rows are gone",
          all(after[k] == 0 for k in ("users", "contacts", "lists", "tenant_plans", "plan_lines")),
          str(after))
    check("6e API keys are gone (keys revoked by the same cascade)", after["api_keys"] == 0)
    expected_kept = before["tenant_plans"] + before["plan_lines"]
    kept = int(scalar(
        f"SELECT COUNT(*) FROM erasure_billing_retention WHERE erased_tenant_id = '{probe['tenant_id']}'") or 0)
    check("6f billing documents kept", kept == expected_kept, f"kept {kept}, expected {expected_kept}")
    audit = sql(f"SELECT actor_email, action, records::text FROM account_erasures "
                f"WHERE erased_tenant_id = '{probe['tenant_id']}'")
    check("6g exactly ONE erasure audit row", len(audit) == 1, f"{len(audit)} row(s)")
    if len(audit) == 1:
        check("6h audit names the actor and the action",
              audit[0][0] == probe["email"] and audit[0][1] == "erase_data", str(audit[0][:2]))
        check("6i audit carries no password or token",
              probe["password"] not in audit[0][2] and probe["token"] not in audit[0][2])
    s, _ = http("GET", "/api/auth/me/usage", token=probe["token"])
    check("6j the erased session is dead (old token -> 401)", s == 401, f"HTTP {s}")

    # ── 7. close still works, and refuses selectively (guards are not a blanket ban) ───────────
    p2 = register_probe(tag + "-close")
    seed_rows(p2)
    b2 = snapshot(p2)
    s, _ = http("POST", "/api/auth/close-account", {"password": p2["password"], "confirm": "CLOSE"},
                api_key=p2["api_key"])
    check("7a close refuses a valid API key -> 401", s == 401, f"HTTP {s}")
    s, _ = http("POST", "/api/auth/close-account", {"password": "wrong", "confirm": "CLOSE"}, token=p2["token"])
    check("7b close refuses a wrong password -> 401", s == 401, f"HTTP {s}")
    check("7c ...and its rows still exist", snapshot(p2) == b2, str(snapshot(p2)))
    s, b = http("POST", "/api/auth/close-account", {"password": p2["password"], "confirm": "CLOSE"},
                token=p2["token"])
    check("7d close with session + password + CLOSE -> 200", s == 200, f"HTTP {s} {str(b)[:160]}")
    check("7e close is a STATE, not a delete: rows kept",
          snapshot(p2) == b2 and scalar(f"SELECT COUNT(*) FROM tenants WHERE id = '{p2['tenant_id']}'") == "1")

    # ── cleanup: probes and their retention/audit rows leave nothing behind ────────────────────
    ids = f"'{probe['tenant_id']}', '{p2['tenant_id']}'"
    sql(f"DELETE FROM erasure_billing_retention WHERE erased_tenant_id IN ({ids});"
        f"DELETE FROM account_erasures WHERE erased_tenant_id IN ({ids});"
        f"DELETE FROM tenants WHERE id IN ({ids});")
    residue = int(scalar(f"SELECT COUNT(*) FROM tenants WHERE id IN ({ids})") or 0) + int(
        scalar(f"SELECT COUNT(*) FROM account_erasures WHERE erased_tenant_id IN ({ids})") or 0)
    check("8 probe residue cleaned", residue == 0, f"{residue} row(s) left")

    print()
    if FAILED:
        print(f"{len(FAILED)} CHECK(S) FAILED: " + ", ".join(FAILED))
        return 1
    print("ALL CHECKS PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
