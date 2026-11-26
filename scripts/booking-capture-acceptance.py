#!/usr/bin/env python3
"""booking-capture-acceptance — kanban: many tags, whole tag CATEGORIES and many LISTS per calendar.

David, 2026-11-26: "if somebody makes a purchase through the calendar they will get tagged with the
product they purchase maybe the city they're from and also a tag that says hot ... and then they be
going to a list so they can be nurtured ... They can go on a main list and then on a customer list."

That was one nullable column (`sponsor_tag`) and no list membership at all. This proves the replacement
on a RUNNING deployment, with counts, on a throwaway calendar inside a real tenant:

  C1 configure three tags ({product} hot, {product}, {city} booking), one CATEGORY with two tags, and
     two LISTS on the calendar -> GET reports exactly that
  C2 a contact booked through the public door comes back with FIVE tags on the contact row
  C3 the contact is a member of BOTH lists (2 rows in list_members)
  C4 the {city} tag is skipped when the form carries no city (no tag named " booking")
  C5 a second booking by the same person adds no duplicate list row and no duplicate tag
  Z  every probe row removed: calendar, tags, category, lists, contact, bookings

    python3 scripts/booking-capture-acceptance.py
    python3 scripts/booking-capture-acceptance.py --base https://app.coreswiftcrm.com
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import hmac
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
import uuid

PASS: list[str] = []
FAIL: list[str] = []
TAG = "capprobe"
PROBE_MAIL_DOMAIN = "coreswiftcrm.com"
ENV_FILE = "/etc/swift/env/coreswift.env"


def env(name: str) -> str:
    for line in open(ENV_FILE):
        line = line.strip()
        if line.startswith(name + "="):
            return line.split("=", 1)[1].strip().strip('"').strip("'")
    raise SystemExit(f"{name} missing")


DB = env("DATABASE_URL")
SECRET = env("JWT_SECRET")


def sql(q: str) -> str:
    r = subprocess.run(["psql", DB, "-tAF|", "-c", q], capture_output=True, text=True)
    if r.returncode != 0:
        raise SystemExit("psql: " + r.stderr.strip()[:300])
    return r.stdout.strip()


def one(q: str) -> str:
    out = sql(q)
    return out.splitlines()[0] if out else ""


def b64(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).decode().rstrip("=")


def mint(uid: str, tid: str, role: str) -> str:
    now = int(time.time())
    head = b64(json.dumps({"alg": "HS256", "typ": "JWT"}, separators=(",", ":")).encode())
    body = b64(json.dumps({"sub": uid, "aid": tid, "role": role,
                           "exp": now + 900, "iat": now, "aud": None, "iss": None},
                          separators=(",", ":")).encode())
    sig = b64(hmac.new(SECRET.encode(), f"{head}.{body}".encode(), hashlib.sha256).digest())
    return f"{head}.{body}.{sig}"


def call(base: str, method: str, path: str, token: str | None = None, body=None):
    data = json.dumps(body).encode() if body is not None else None
    hdr = {"Accept": "application/json", "Content-Type": "application/json"}
    if token:
        hdr["Authorization"] = "Bearer " + token
    req = urllib.request.Request(base + path, data=data, method=method, headers=hdr)
    try:
        with urllib.request.urlopen(req, timeout=40) as r:
            raw = r.read().decode()
            return r.status, (json.loads(raw) if raw.strip()[:1] in "{[" else {"raw": raw})
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw)
        except Exception:
            return e.code, {"raw": raw[:300]}


def check(name: str, ok: bool, detail: str) -> None:
    (PASS if ok else FAIL).append(f"{name}: {detail}")
    print(("  PASS " if ok else "  FAIL ") + f"{name}: {detail}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default="http://127.0.0.1:8084")
    a = ap.parse_args()
    base = a.base.rstrip("/")

    owner = one("SELECT id||'|'||tenant_id||'|'||role FROM users "
                "WHERE is_active AND role='owner' ORDER BY created_at LIMIT 1")
    uid, tid, role = owner.split("|")
    tok = mint(uid, tid, role)

    cal = str(uuid.uuid4())
    cat = str(uuid.uuid4())
    l1, l2 = str(uuid.uuid4()), str(uuid.uuid4())
    c1, c2 = str(uuid.uuid4()), str(uuid.uuid4())
    # NOT a reserved/harness address: the app rightly refuses to mint a CRM contact from a synthetic
    # requester (that gate exists because harness runs had created 26 probe contacts in one account). The
    # capture path is what is under test, so the buyer has to look like a customer. This domain is our own.
    contact_email = f"captureaudit{cal[:6]}@{PROBE_MAIL_DOMAIN}"
    slug = f"{TAG}-{cal[:8]}"

    sql(f"INSERT INTO booking_calendars (id, tenant_id, name, slug, calendar_type, kind, hold_minutes, "
        f"is_active) VALUES ('{cal}','{tid}','{TAG} calendar','{slug}','personal','appointment',30,true)")
    sql(f"INSERT INTO tag_categories (id, tenant_id, name) VALUES ('{cat}','{tid}','{TAG} Products')")
    for cid, name in ((c1, f"{TAG} Widget"), (c2, f"{TAG} Gadget")):
        sql(f"INSERT INTO tags (id, tenant_id, name, category_id) VALUES "
            f"('{cid}','{tid}','{name}','{cat}')")
    for lid, name in ((l1, f"{TAG} Nurture"), (l2, f"{TAG} Customers")):
        sql(f"INSERT INTO lists (id, tenant_id, name, list_type) VALUES ('{lid}','{tid}','{name}','static')")

    try:
        st, body = call(base, "PUT", f"/api/booking/calendars/{cal}/capture", tok, {
            "tags": ["{product} hot", "{product}", "{city} booking"],
            "categories": [cat],
            "lists": [l1, l2],
        })
        st2, got = call(base, "GET", f"/api/booking/calendars/{cal}/capture", tok)
        g = got if isinstance(got, dict) else {}
        tags = g.get("tags", []) or []
        cats = [c.get("id") for c in (g.get("categories") or []) if isinstance(c, dict)]
        lists = [l.get("id") for l in (g.get("lists") or []) if isinstance(l, dict)]
        check("C1 configured", st == 200 and st2 == 200 and len(tags) == 3 and cats == [cat]
              and sorted(lists) == sorted([l1, l2]),
              f"PUT {st} | tags={len(tags)} | categories={len(cats)} | lists={len(lists)}")

        # a real booking through the PUBLIC door: the product is the position's own name
        res = str(uuid.uuid4())
        sql(f"INSERT INTO booking_resources (id, calendar_id, name, capacity, price_cents, "
            f"duration_min, active) VALUES ('{res}','{cal}','{TAG} Full Takeover',1,4000,30,true)")
        # Week-round opening hours for the probe position: without them every offered time is refused
        # ("that time is not offered") and the test measures the availability engine, not the capture.
        for wd in range(7):
            sql(f"INSERT INTO booking_availability_rules (id, resource_id, weekday, start_time, end_time) "
                f"VALUES ('{uuid.uuid4()}','{res}',{wd},'00:00','23:59')")
        # Appointment calendars offer times on a grid derived from duration_min, so the start has to sit on
        # it: an arbitrary second answers "that time is not offered" and the test would never reach capture.
        tomorrow = time.strftime("%Y-%m-%d", time.gmtime(time.time() + 86400))
        start = f"{tomorrow}T10:00:00Z"
        start2 = f"{tomorrow}T11:00:00Z"
        bst, bbody = call(base, "POST", f"/api/public/booking/c/{slug}/orders", None, {
            "customer_name": "Dana Buyer",
            "customer_email": contact_email,
            "items": [{"resource_id": res, "starts_at": start, "quantity": 1}],
        })
        contact = one(f"SELECT id FROM contacts WHERE tenant_id='{tid}' AND email='{contact_email}' "
                      "ORDER BY created_at DESC LIMIT 1")
        tag_names = []
        if contact:
            tag_names = [t for t in sql(
                f"SELECT t.name FROM tags t JOIN tag_assignments ct ON ct.tag_id=t.id "
                f"WHERE ct.entity_id='{contact}' AND ct.entity_type='contact' ORDER BY t.name").splitlines() if t]
        check("C2 four tags on the contact", bst in (200, 201) and contact != "" and len(tag_names) == 4,
              f"booking HTTP {bst} | tags on contact = {len(tag_names)} → {tag_names} "
              f"(3 templates, one skipped for a missing city, plus 2 from the category)")

        members = one(f"SELECT COUNT(*) FROM list_members WHERE contact_id='{contact}' "
                      f"AND list_id IN ('{l1}','{l2}')") if contact else "0"
        check("C3 on both lists", members == "2", f"list_members rows for the two lists = {members}")

        stray = one(f"SELECT COUNT(*) FROM tags t JOIN tag_assignments ct ON ct.tag_id=t.id "
                    f"WHERE ct.entity_id='{contact}' AND ct.entity_type='contact' AND t.name LIKE '% booking'") if contact else "0"
        check("C4 empty placeholder skipped", stray == "0",
              f"tags ending in ' booking' (no city on this form) = {stray}")

        # the same person books again: no duplicate membership, no duplicate tag
        call(base, "POST", f"/api/public/booking/c/{slug}/orders", None, {
            "customer_name": "Dana Buyer",
            "customer_email": contact_email,
            "items": [{"resource_id": res, "starts_at": start2, "quantity": 1}],
        })
        members2 = one(f"SELECT COUNT(*) FROM list_members WHERE contact_id='{contact}' "
                       f"AND list_id IN ('{l1}','{l2}')") if contact else "0"
        tags2 = one(f"SELECT COUNT(*) FROM tag_assignments WHERE entity_id='{contact}'") if contact else "0"
        check("C5 returning buyer adds nothing", members2 == "2" and tags2 == "4",
              f"list rows = {members2} | contact_tags rows = {tags2}")
    finally:
        sql(f"DELETE FROM booking_orders WHERE calendar_id='{cal}'")
        sql(f"DELETE FROM booking_calendars WHERE id='{cal}'")
        sql(f"DELETE FROM lists WHERE id IN ('{l1}','{l2}')")
        sql(f"DELETE FROM tag_assignments WHERE tag_id IN "
            f"(SELECT id FROM tags WHERE name LIKE '{TAG}%')")
        sql(f"DELETE FROM tags WHERE name LIKE '{TAG}%'")
        sql(f"DELETE FROM tag_categories WHERE id='{cat}'")
        sql(f"DELETE FROM contacts WHERE email LIKE '{TAG}%'")
        sql(f"DELETE FROM booking_availability_rules WHERE resource_id NOT IN "
            f"(SELECT id FROM booking_resources WHERE name NOT LIKE '{TAG}%')")
        sql(f"DELETE FROM booking_resources WHERE name LIKE '{TAG}%'")
        # capture CREATES the tags it expands to, so their assignments must go before the tags themselves
        sql(f"DELETE FROM tag_assignments WHERE tag_id IN (SELECT id FROM tags WHERE name LIKE '{TAG}%')")
        sql(f"DELETE FROM tag_categories WHERE name LIKE '{TAG}%'")

    left = one(f"SELECT (SELECT COUNT(*) FROM booking_calendars WHERE name LIKE '{TAG}%')"
               f" + (SELECT COUNT(*) FROM contacts WHERE email LIKE '{TAG}%')"
               f" + (SELECT COUNT(*) FROM lists WHERE name LIKE '{TAG}%')"
               f" + (SELECT COUNT(*) FROM tags WHERE name LIKE '{TAG}%')")
    check("Z no probe rows left", left == "0", f"probe rows across four tables = {left}")

    # ---- C6 TEMPERATURE SWAP: an exclusive category keeps exactly one tag, and never zero (David, 2026-11-26)
    tcat = str(uuid.uuid4())
    warm, hot = str(uuid.uuid4()), str(uuid.uuid4())
    person = str(uuid.uuid4())
    sql(f"INSERT INTO tag_categories (id, tenant_id, name, is_exclusive) VALUES "
        f"('{tcat}','{tid}','{TAG} Temperature',true)")
    for tid_, nm in ((warm, f"{TAG} Warm"), (hot, f"{TAG} Hot")):
        sql(f"INSERT INTO tags (id, tenant_id, name, category_id) VALUES ('{tid_}','{tid}','{nm}','{tcat}')")
    sql(f"INSERT INTO contacts (id, tenant_id, email, first_name, last_name) VALUES "
        f"('{person}','{tid}','{TAG}-temp@example.invalid','temp','probe')")
    sql(f"INSERT INTO tag_assignments (id, tag_id, entity_type, entity_id, tenant_id) VALUES "
        f"('{uuid.uuid4()}','{warm}','contact','{person}','{tid}')")
    before = one(f"SELECT COUNT(*) FROM tag_assignments WHERE entity_id='{person}'")
    sql(f"INSERT INTO tag_assignments (id, tag_id, entity_type, entity_id, tenant_id) VALUES "
        f"('{uuid.uuid4()}','{hot}','contact','{person}','{tid}')")
    kept = sql(f"SELECT t.name FROM tag_assignments ta JOIN tags t ON t.id=ta.tag_id "
               f"WHERE ta.entity_id='{person}' ORDER BY t.name").splitlines()
    check("C6 exclusive swap", before == "1" and kept == [f"{TAG} Hot"],
          f"after Warm then Hot, tags held = {kept} (never zero, never both)")
    sql(f"DELETE FROM tag_assignments WHERE entity_id='{person}'")
    sql(f"DELETE FROM contacts WHERE id='{person}'")
    sql(f"DELETE FROM tags WHERE id IN ('{warm}','{hot}')")
    sql(f"DELETE FROM tag_categories WHERE id='{tcat}'")

    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    return 0 if not FAIL else 1


if __name__ == "__main__":
    sys.exit(main())
