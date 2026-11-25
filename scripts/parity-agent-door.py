#!/usr/bin/env python3
"""Does the account's own key reach everything the console screens can do?

Fleet standard §10, measured the way `ui-action-parity-by-key` prescribes: every call below is one the
console itself makes (censused from `www-app/coreswift/index.html`), issued through CoreSwift's own MCP
door (`coreswift_api`) with an ISSUED ACCOUNT KEY — no console session.

How to read a status:
  * 200/201                     reachable and working
  * 400/422 naming a field      reachable — the handler ran, only the body was thin
  * 404 on a random uuid        reachable — the handler ran and scoped the id
  * 401                         GAP: nothing accepted the key
  * 403 naming a role           reachable by another ROLE only — classify before calling it a gap

Deliberately NOT exercised, and why: anything that would change live mail configuration or send mail
(`private-email/boxes`, `domains`, `keys`, `auto-replies`, `send`). A probe that posts `{}` to a handler
which overwrites configuration is not a test, it is an outage waiting for a typo. Those are listed with
`skipped` and a reason instead of a fake pass.

Usage:  python3 scripts/parity-agent-door.py            # writes scripts/parity-agent-door.json
"""
import json
import os
import sys
import urllib.error
import urllib.request
import uuid

DOOR = os.environ.get("DOOR", "http://127.0.0.1:8084/api/mcp")


def key():
    path = os.environ.get("KEY_FILE", "/tmp/wp_key")
    with open(path) as fh:
        return fh.read().strip()


def door(method: str, path: str, body=None):
    args = {"method": method, "path": path}
    if body is not None:
        args["body"] = body
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "coreswift_api", "arguments": args},
    }
    req = urllib.request.Request(
        DOOR,
        data=json.dumps(payload).encode(),
        method="POST",
        headers={
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "Authorization": "Bearer " + key(),
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=40) as resp:
            envelope = json.loads(resp.read().decode())
    except urllib.error.HTTPError as err:
        return err.code, {"transport": err.read().decode()[:200]}
    result = envelope.get("result", {})
    text = (result.get("content") or [{}])[0].get("text", "")
    if result.get("isError"):
        # The door reports the app's own status inside the sentence it returns.
        for code in (401, 403, 404, 422, 400, 500):
            if f" {code}:" in text or f"answered {code}" in text:
                return code, {"app": text[:200]}
        return "error", {"app": text[:200]}
    try:
        parsed = json.loads(text)
        return parsed.get("status", 200), parsed.get("body")
    except Exception:
        return "?", {"raw": text[:160]}


def classify(status, app_says=""):
    if status in (200, 201, 204):
        return "reachable"
    if status in (400, 422):
        return "reachable (body refused)"
    if status == 404:
        return "reachable (id scoped)"
    if status == 401:
        return "GAP"
    if status == 403:
        return "role/credential excluded"
    return "unexpected"


# (area, method, path-template, note) — path templates are the console's own, with <id> where a real id goes.
READS = [
    ("calendar", "GET", "booking/calendars", "calendar list"),
    ("calendar", "GET", "booking/calendars/summary", "every calendar + problems in one call"),
    ("calendar", "GET", "booking/calendars/<cal>", "one calendar"),
    ("calendar", "GET", "booking/calendars/<cal>/orders", "bookings for a calendar"),
    ("calendar", "GET", "booking/calendars/<cal>/form-fields", "booking questions"),
    ("calendar", "GET", "booking/calendars/<cal>/confirmation-email", "confirmation wording"),
    ("calendar", "GET", "booking/calendars/<cal>/readiness", "what still needs setting up"),
    ("calendar", "GET", "booking/calendars/<cal>/payment-wiring", "payment wiring state"),
    ("inbox", "GET", "private-email/messages?limit=5", "newest messages"),
    ("inbox", "GET", "private-email/messages/counts", "what is new, per folder"),
    ("inbox", "GET", "private-email/messages/<msg>", "one message"),
    ("inbox", "GET", "private-email/folders", "folders"),
    ("inbox", "GET", "private-email/addresses", "the account's own mailboxes"),
    ("inbox", "GET", "private-email/readiness", "mail setup state"),
    ("tickets", "GET", "tickets", "ticket list"),
    ("tickets", "GET", "tickets/stats", "counts by status"),
    ("tickets", "GET", "tickets/<ticket>", "one ticket"),
    ("tickets", "GET", "helpdesk/tickets/<ticket>/attachments", "attachments the console asks for"),
]

# Writes proved by creating and then deleting a probe row, so the account is left exactly as it was.
WRITES = [
    ("tickets", "POST", "tickets", {"subject": "Parity probe", "description": "probe", "priority": "low"}, "create"),
    ("tickets", "POST", "tickets/<new>/messages", {"message": "parity probe note", "kind": "note"}, "internal note"),
    ("tickets", "PATCH", "tickets/<new>", {"status": "pending"}, "status change"),
    ("tickets", "DELETE", "tickets/<new>", None, "delete the probe"),
]

SKIPPED = [
    ("inbox", "POST", "private-email/send", "would send real mail"),
    ("inbox", "POST", "private-email/keys", "would mint a mailbox credential"),
    ("inbox", "POST", "private-email/boxes", "would change live mailbox config"),
    ("inbox", "PATCH", "private-email/boxes/<id>", "would change live mailbox config"),
    ("inbox", "POST", "private-email/domains", "would change live domain config"),
    ("inbox", "POST", "private-email/auto-replies", "would change live auto-reply behaviour"),
    ("inbox", "POST", "private-email/admin/purge/run", "would delete mail"),
    ("calendar", "PATCH", "booking/calendars/<cal>", "would change a live customer calendar"),
]


def main():
    rows = []

    # Discover real ids once, so the templates exercise the real scoping path.
    _, cals = door("GET", "booking/calendars")
    cal_id = cals[0]["id"] if isinstance(cals, list) and cals and isinstance(cals[0], dict) else ""
    _, inbox = door("GET", "private-email/messages?limit=1")
    msg_id = ""
    if isinstance(inbox, dict):
        msgs = inbox.get("messages") or []
        msg_id = msgs[0]["id"] if msgs and isinstance(msgs[0], dict) else ""
    _, tickets = door("GET", "tickets")
    ticket_id = ""
    if isinstance(tickets, dict):
        tl = tickets.get("tickets") or []
        ticket_id = tl[0]["id"] if tl and isinstance(tl[0], dict) else ""

    subs = {"<cal>": cal_id, "<msg>": msg_id, "<ticket>": ticket_id}

    for area, method, tmpl, note in READS:
        path = tmpl
        for k, v in subs.items():
            path = path.replace(k, v or str(uuid.uuid4()))
        status, body = door(method, path)
        rows.append(
            {
                "area": area,
                "method": method,
                "path": tmpl,
                "status": status,
                "verdict": classify(status),
                "note": note,
            }
        )

    new_id = ""
    for area, method, tmpl, body, note in WRITES:
        path = tmpl.replace("<new>", new_id or "00000000-0000-0000-0000-000000000000")
        status, out = door(method, path, body)
        if tmpl == "tickets" and method == "POST" and isinstance(out, dict):
            new_id = out.get("id", "")
        rows.append(
            {
                "area": area,
                "method": method,
                "path": tmpl,
                "status": status,
                "verdict": classify(status, str(out)[:120]),
                "note": note,
            }
        )

    for area, method, path, why in SKIPPED:
        rows.append(
            {
                "area": area,
                "method": method,
                "path": path,
                "status": "skipped",
                "verdict": "not exercised",
                "note": why,
            }
        )

    out_path = os.path.join(os.path.dirname(__file__), "parity-agent-door.json")
    with open(out_path, "w") as fh:
        json.dump({"door": DOOR, "rows": rows}, fh, indent=2)

    by_verdict = {}
    for r in rows:
        by_verdict[r["verdict"]] = by_verdict.get(r["verdict"], 0) + 1
    print(json.dumps({"results": by_verdict, "total": len(rows)}, indent=2))
    gaps = [r for r in rows if r["verdict"] in ("GAP", "unexpected")]
    for g in gaps:
        print(f"  {g['area']:9} {g['method']:6} {g['path']:44} -> {g['status']}")
    return 1 if gaps else 0


if __name__ == "__main__":
    sys.exit(main())
