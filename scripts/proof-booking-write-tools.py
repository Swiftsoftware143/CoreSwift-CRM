#!/usr/bin/env python3
"""The four writes the fleet used to forward, proved on CoreSwift's OWN MCP door (kanban t_bda9b277).

Before the cleanup the fleet MCP exposed four CoreSwift write tools and forwarded them here:
`coreswift_booking_set_product_checkout`, `coreswift_booking_set_calendar_checkout`,
`coreswift_booking_rotate_payment_key`, `coreswift_domain_set_redirect`. The fleet reader is
read-only now (David: reads in the fleet, writes inside each software), so the same four names live
on this app's own door at POST /api/mcp. This harness proves each one with a REAL write against a
probe calendar, a probe product and a probe address, reads the row back from the API **and** the
database, and then deletes every probe row it made.

What a status means (the door reports the app's own status inside its sentence):
  * a tool result that carries `{"status": 2xx, "body": …}`  -> the write happened
  * `not_found` for a random id                              -> the write is scoped to this account
  * `this tool requires role `workspace``                     -> a read-only key cannot reach it
  * `the app answered 400: …https…`                           -> the console's own validation still runs

Usage:
    python3 scripts/proof-booking-write-tools.py
    python3 scripts/proof-booking-write-tools.py --base http://127.0.0.1:8084

Exit code 0 only when every leg passed and the residue count is zero.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.request
import uuid
from typing import Any

PASS: list[str] = []
FAIL: list[str] = []


def check(name: str, ok: bool, detail: str = "") -> None:
    (PASS if ok else FAIL).append(name)
    print(("PASS  " if ok else "FAIL  ") + name + (f"  [{detail}]" if detail else ""))


class Db:
    """psql on the host, with the credential read from the app container's DATABASE_URL."""

    def __init__(self, container: str) -> None:
        url = subprocess.run(
            ["docker", "exec", container, "printenv", "DATABASE_URL"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        m = re.match(r"postgres://([^:]+):([^@]+)@[^/]+/(\w+)", url)
        if not m:
            raise SystemExit("cannot parse DATABASE_URL from the app container")
        self.user, self.password, self.name = m.group(1), m.group(2), m.group(3)

    def one(self, sql: str) -> str:
        env = dict(os.environ, PGPASSWORD=self.password)
        out = subprocess.run(
            ["psql", "-h", "127.0.0.1", "-U", self.user, "-d", self.name,
             "-At", "-v", "ON_ERROR_STOP=1", "-c", sql],
            capture_output=True, text=True, env=env,
        )
        if out.returncode != 0:
            raise SystemExit(f"psql failed: {out.stderr.strip()[:400]}")
        # `-At` still prints the command tag for a statement with RETURNING ("INSERT 0 1"), so the
        # value is the FIRST line — otherwise an id reads as "<uuid>\nINSERT 0 1" and the cleanup
        # that uses it fails (measured on the first run of this harness).
        return out.stdout.strip().splitlines()[0] if out.stdout.strip() else ""


class Door:
    def __init__(self, base: str) -> None:
        self.url = base.rstrip("/") + "/api/mcp"

    def rpc(self, token: str, method: str, params: dict | None = None) -> dict:
        payload = {"jsonrpc": "2.0", "id": 1, "method": method}
        if params is not None:
            payload["params"] = params
        req = urllib.request.Request(
            self.url,
            data=json.dumps(payload).encode(),
            method="POST",
            headers={
                "Content-Type": "application/json",
                "Accept": "application/json, text/event-stream",
                "Authorization": "Bearer " + token,
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                return json.loads(resp.read().decode())
        except urllib.error.HTTPError as err:
            return {"http": err.code, "body": err.read().decode()[:300]}

    def tools(self, token: str) -> list[str]:
        out = self.rpc(token, "tools/list")
        tools = (out.get("result") or {}).get("tools") or []
        return [t.get("name", "") for t in tools]

    def call(self, token: str, name: str, args: dict) -> tuple[Any, Any, str]:
        """-> (status, body, error_sentence). `status` is the app's own status inside the envelope."""
        out = self.rpc(token, "tools/call", {"name": name, "arguments": args})
        result = out.get("result") or {}
        text = (result.get("content") or [{}])[0].get("text", "")
        if result.get("isError"):
            return "error", None, text
        try:
            parsed = json.loads(text)
            return parsed.get("status"), parsed.get("body"), ""
        except Exception:
            return "?", None, text


def mint_key(db: Db, tenant: str, role: str, name: str) -> tuple[str, str]:
    token = "csk_" + uuid.uuid4().hex + uuid.uuid4().hex
    digest = hashlib.sha256(token.encode()).hexdigest()
    user = db.one(
        "SELECT id FROM users WHERE tenant_id = '%s' AND (is_active IS NULL OR is_active) "
        "ORDER BY (role = 'account_owner') DESC, created_at LIMIT 1" % tenant
    )
    if not user:
        raise SystemExit("the probe tenant has no active user to act as")
    key_id = db.one(
        "INSERT INTO personal_api_keys (tenant_id, user_id, name, key_hash, key_prefix, is_active, api_role) "
        "VALUES ('%s', '%s', '%s', '%s', '%s', true, '%s') RETURNING id"
        % (tenant, user, name, digest, token[:12], role)
    )
    return token, key_id


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8084")
    ap.add_argument("--container", default="coreswiftcrm")
    ap.add_argument("--tenant", default="821f3ad5-26a8-4a74-be49-772c0798d568",
                    help="the account the probe runs in (ZaarHub: the owner's own, no customer)")
    ap.add_argument("--app-domain", default="coreswiftcrm.com")
    args = ap.parse_args()

    db = Db(args.container)
    door = Door(args.base)
    slug = "probe-t-bda9b277"
    sub = "probe-t-bda9b277"
    host = f"{sub}.{args.app_domain}"
    product_checkout = f"https://pay.example.com/{slug}-product"
    calendar_checkout = f"https://pay.example.com/{slug}-calendar"

    # A previous interrupted run must not make this one fail on a duplicate slug.
    db.one(
        "DELETE FROM booking_calendars WHERE tenant_id = '%s' AND slug = '%s'" % (args.tenant, slug)
    )
    db.one("DELETE FROM domain_redirects WHERE lower(host) = lower('%s')" % host)

    key, key_id = mint_key(db, args.tenant, "workspace", "probe t_bda9b277")
    ro_key, ro_id = mint_key(db, args.tenant, "read_only", "probe t_bda9b277 read-only")
    cal_id = ""
    resource_id = ""
    try:
        names = door.tools(key)
        for tool in (
            "coreswift_booking_set_product_checkout",
            "coreswift_booking_set_calendar_checkout",
            "coreswift_booking_rotate_payment_key",
            "coreswift_domain_set_redirect",
        ):
            check(f"tools/list carries {tool}", tool in names, f"{len(names)} tools")

        # ── fixtures, created through the door itself ──────────────────────────────────────────
        status, body, err = door.call(key, "coreswift_api", {
            "method": "POST",
            "path": "booking/calendars",
            "body": {"name": "Probe t_bda9b277", "slug": slug, "kind": "appointment"},
        })
        cal_id = body.get("id", "") if isinstance(body, dict) else ""
        check("probe calendar created through the door", status == 201 and bool(cal_id), f"status {status} id {cal_id[:8]}")

        status, body, err = door.call(key, "coreswift_api", {
            "method": "POST",
            "path": f"booking/calendars/{cal_id}/resources",
            "body": {"name": "Probe product", "price_cents": 2500, "duration_min": 30, "capacity": 1},
        })
        resource_id = body.get("id", "") if isinstance(body, dict) else ""
        check("probe product created", status == 201 and bool(resource_id), f"status {status}")

        # ── 1. the product's own checkout ──────────────────────────────────────────────────────
        status, body, err = door.call(key, "coreswift_booking_set_product_checkout", {
            "resource_id": resource_id, "checkout": product_checkout, "payment_mode": "before",
        })
        check("set_product_checkout wrote", status == 201 or status == 200, f"status {status} {err[:80]}")

        _, cal, _ = door.call(key, "coreswift_api", {"method": "GET", "path": f"booking/calendars/{cal_id}"})
        res = next((r for r in (cal or {}).get("resources", []) if r.get("id") == resource_id), {})
        check("product checkout read back", res.get("checkout_embed") == product_checkout, str(res.get("checkout_embed"))[:60])
        check("product payment timing read back", res.get("payment_mode") == "before", str(res.get("payment_mode")))
        check("product price untouched by a checkout-only change", res.get("price_cents") == 2500, str(res.get("price_cents")))
        check("product name untouched", res.get("name") == "Probe product", str(res.get("name")))

        # ── 2. the calendar's default checkout ────────────────────────────────────────────────
        status, body, err = door.call(key, "coreswift_booking_set_calendar_checkout", {
            "calendar_id": cal_id, "checkout": calendar_checkout, "payment_mode": "after",
        })
        check("set_calendar_checkout wrote", status == 200, f"status {status} {err[:80]}")

        _, cal, _ = door.call(key, "coreswift_api", {"method": "GET", "path": f"booking/calendars/{cal_id}"})
        one = (cal or {}).get("calendar", {})
        check("calendar checkout read back", one.get("payment_embed") == calendar_checkout, str(one.get("payment_embed"))[:60])
        check("calendar payment timing read back", one.get("payment_mode") == "after", str(one.get("payment_mode")))
        check("calendar name untouched", one.get("name") == "Probe t_bda9b277", str(one.get("name")))

        # ── 3. the payment key rotation ───────────────────────────────────────────────────────
        old_key = one.get("payment_webhook_key") or ""
        status, body, err = door.call(key, "coreswift_booking_rotate_payment_key", {"calendar_id": cal_id})
        new_key = (body or {}).get("key", "") if isinstance(body, dict) else ""
        check("rotate_payment_key returned a new key", bool(new_key) and new_key != old_key, f"old {old_key[:6]}… new {new_key[:6]}…")

        _, cal, _ = door.call(key, "coreswift_api", {"method": "GET", "path": f"booking/calendars/{cal_id}"})
        live_key = (cal or {}).get("calendar", {}).get("payment_webhook_key")
        check("rotated key is the one the app now serves", live_key == new_key, f"{str(live_key)[:6]}…")
        db_key = db.one("SELECT payment_webhook_key FROM booking_calendars WHERE id = '%s'" % cal_id)
        check("rotated key is the row's real value", db_key == new_key, f"{db_key[:6]}…")

        # ── 4. the address redirect, on an address this account really owns ───────────────────
        status, body, err = door.call(key, "coreswift_api", {
            "method": "PUT", "path": f"bookings/calendars/{slug}/subdomain", "body": {"subdomain": sub},
        })
        check("probe calendar address claimed", status in (200, 201), f"status {status} {err[:80]}")

        status, body, err = door.call(key, "coreswift_domain_set_redirect", {
            "host": host, "target_url": "/pricing", "status_code": 302,
        })
        check("domain_set_redirect wrote", status == 200, f"status {status} {err[:100]}")

        _, listing, _ = door.call(key, "coreswift_api", {"method": "GET", "path": "domain-redirects"})
        row = next((a for a in (listing or {}).get("addresses", []) if a.get("host") == host), {})
        check("redirect read back from the address list", row.get("sends_visitors_to") == "/pricing", str(row.get("sends_visitors_to")))
        check("redirect status read back", row.get("status_code") == 302, str(row.get("status_code")))
        db_target = db.one(
            "SELECT coalesce(target_url,'') FROM domain_redirects WHERE lower(host) = lower('%s')" % host
        )
        check("redirect is the row's real value", db_target == "/pricing", db_target)

        # ── the door still enforces what it enforced before ───────────────────────────────────
        status, body, err = door.call(ro_key, "coreswift_booking_set_product_checkout", {
            "resource_id": resource_id, "checkout": product_checkout,
        })
        check("a read-only key cannot reach the writer", status == "error" and "workspace" in err, err[:80])

        status, body, err = door.call(key, "coreswift_booking_set_product_checkout", {
            "resource_id": str(uuid.uuid4()), "checkout": product_checkout,
        })
        check("a foreign id is not_found, not a write", status == "error" and "not_found" in err, err[:60])

        status, body, err = door.call(key, "coreswift_booking_set_product_checkout", {
            "resource_id": resource_id, "checkout": "http://pay.example.com/insecure",
        })
        check("the app still refuses a non-https address", status == "error" and "https" in err, err[:90])

        status, body, err = door.call(key, "coreswift_domain_set_redirect", {
            "host": "not-this-accounts.example.com", "target_url": "/pricing",
        })
        check("an address this account does not own is refused", status == "error" and "not one of this account" in err, err[:90])
    finally:
        # ── cleanup: every probe row, by id ───────────────────────────────────────────────────
        door.call(key, "coreswift_api", {"method": "DELETE", "path": f"domain-redirects/{host}"})
        if resource_id and cal_id:
            db.one("DELETE FROM booking_resources WHERE id = '%s' AND calendar_id = '%s'" % (resource_id, cal_id))
        if cal_id:
            db.one("DELETE FROM booking_calendars WHERE id = '%s' AND tenant_id = '%s'" % (cal_id, args.tenant))
        db.one("DELETE FROM calendar_subdomains WHERE lower(subdomain) = lower('%s')" % sub)
        db.one("DELETE FROM domain_redirects WHERE lower(host) = lower('%s')" % host)
        db.one("DELETE FROM personal_api_keys WHERE id IN ('%s', '%s')" % (key_id, ro_id))

    left = db.one(
        "SELECT (SELECT count(*) FROM booking_calendars WHERE slug = '%s') || ',' || "
        "(SELECT count(*) FROM calendar_subdomains WHERE lower(subdomain) = lower('%s')) || ',' || "
        "(SELECT count(*) FROM domain_redirects WHERE lower(host) = lower('%s')) || ',' || "
        "(SELECT count(*) FROM personal_api_keys WHERE id IN ('%s', '%s'))"
        % (slug, sub, host, key_id, ro_id)
    )
    check("probe residue is zero (calendars,subdomains,redirects,keys)", left == "0,0,0,0", left)

    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 1 if FAIL else 0


if __name__ == "__main__":
    sys.exit(main())
