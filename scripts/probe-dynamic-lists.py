#!/usr/bin/env python3
"""Prove a rule-driven list maintains itself, on throwaway data, through the agent door.

Follows `rule-driven-segment-membership` §4: create the segments, watch membership arrive from a RULE,
watch it leave when the record stops matching, and assert the refusals too — a probe that only walks the
happy path leaves the silent-zero shape (a rule the engine cannot run returning "nobody qualifies") in
place, which is exactly the shape that made the console look broken.

Everything it creates, it deletes. Run against the live binary after a deploy:

    python3 scripts/probe-dynamic-lists.py
"""
import json
import os
import sys
import urllib.error
import urllib.request
import uuid

DOOR = os.environ.get("DOOR", "http://127.0.0.1:8084/api/mcp")
KEY_FILE = os.environ.get("KEY_FILE", "/tmp/wp_key")
TAG = uuid.uuid4().hex[:6]
EMAIL = f"segment-probe-{TAG}@example.invalid"

results = []


def api(method, path, body=None):
    args = {"method": method, "path": path}
    if body is not None:
        args["body"] = body
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "coreswift_api", "arguments": args},
    }
    with open(KEY_FILE) as fh:
        token = fh.read().strip()
    req = urllib.request.Request(
        DOOR,
        data=json.dumps(payload).encode(),
        method="POST",
        headers={
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
            "Authorization": "Bearer " + token,
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=40) as resp:
            envelope = json.loads(resp.read().decode())
    except urllib.error.HTTPError as err:
        return err.code, {"transport": err.read().decode()[:160]}
    result = envelope.get("result", {})
    text = (result.get("content") or [{}])[0].get("text", "")
    if result.get("isError"):
        for code in (400, 401, 403, 404, 422, 500):
            if f"answered {code}" in text:
                return code, {"app": text[:220]}
        return "error", {"app": text[:220]}
    try:
        parsed = json.loads(text)
        return parsed.get("status", 200), parsed.get("body")
    except Exception:
        return "?", {"raw": text[:160]}


def check(label, ok, detail=""):
    results.append({"check": label, "ok": bool(ok), "detail": str(detail)[:180]})
    print(("PASS " if ok else "FAIL ") + label + ("" if ok else f"  <- {detail}"))


def main():
    contact = list_id = None
    try:
        st, body = api(
            "POST",
            "contacts",
            {
                "first_name": "Segment",
                "last_name": "Probe",
                "email": EMAIL,
                "city": "Palm Bay",
                "job_title": "Owner",
            },
        )
        contact = (body or {}).get("id") if isinstance(body, dict) else None
        check("a contact exists with a city", st == 201 and bool(contact), f"{st} {body}")

        st, body = api("POST", "lists", {"name": f"Segment probe {TAG}", "list_type": "dynamic"})
        list_id = (body or {}).get("id") if isinstance(body, dict) else None
        check("a dynamic list is created", st == 201 and bool(list_id), f"{st} {body}")

        rule = {"field": "contact_field", "operator": "equals", "value": {"column": "city", "equals": "Palm Bay"}}
        st, body = api("PATCH", f"lists/{list_id}", {"rules": [rule]})
        check("a contact-field rule saves", st == 200, f"{st} {body}")

        st, body = api("POST", f"lists/{list_id}/evaluate", {})
        matched = (body or {}).get("total_matched", 0) if isinstance(body, dict) else 0
        added = (body or {}).get("added", 0) if isinstance(body, dict) else 0
        check("the RULE puts the contact on the list", st == 200 and matched >= 1 and added >= 1, f"{st} {body}")

        st, body = api("GET", f"lists/{list_id}")
        count = (body or {}).get("member_count") if isinstance(body, dict) else None
        check("the stored count agrees with the membership", count is not None and count >= 1, f"{st} count={count}")

        st, body = api("GET", f"lists/{list_id}/members")
        ids = set()
        if isinstance(body, dict):
            for m in body.get("members") or body.get("contacts") or []:
                if isinstance(m, dict):
                    ids.add(str(m.get("contact_id") or m.get("id")))
        elif isinstance(body, list):
            for m in body:
                if isinstance(m, dict):
                    ids.add(str(m.get("contact_id") or m.get("id")))
        check("the probe contact is one of the members", str(contact) in ids, f"{st} members={len(ids)}")

        st, _ = api("PATCH", f"contacts/{contact}", {"city": "Melbourne"})
        check("the contact's city can change", st == 200, st)
        st, body = api("POST", f"lists/{list_id}/evaluate", {})
        removed = (body or {}).get("removed", 0) if isinstance(body, dict) else 0
        check("it leaves when it stops matching", st == 200 and removed >= 1, f"{st} {body}")

        # Refusals: a rule the engine cannot run must be a 4xx that names the vocabulary.
        st, body = api("PATCH", f"lists/{list_id}", {"rules": [{"field": "city", "operator": "equals", "value": "x"}]})
        check("an unknown field is refused by name", st in (400, 422), f"{st} {body}")
        st, body = api(
            "PATCH",
            f"lists/{list_id}",
            {"rules": [{"field": "contact_field", "operator": "gte", "value": {"column": "city", "equals": "x"}}]},
        )
        check("an operator that field cannot run is refused", st in (400, 422), f"{st} {body}")
        st, body = api(
            "PATCH",
            f"lists/{list_id}",
            {"rules": [{"field": "contact_field", "operator": "equals", "value": {"column": "city", "equals": ""}}]},
        )
        check("a rule with no value is refused", st in (400, 422), f"{st} {body}")
        st, body = api(
            "PATCH",
            f"lists/{list_id}",
            {"rules": [{"field": "contact_field", "operator": "equals", "value": {"column": "salary", "equals": "x"}}]},
        )
        check("a column that is not whitelisted is refused", st in (400, 422), f"{st} {body}")
    finally:
        if list_id:
            api("DELETE", f"lists/{list_id}")
        if contact:
            api("DELETE", f"contacts/{contact}")

    out = os.path.join(os.path.dirname(__file__), "probe-dynamic-lists.json")
    with open(out, "w") as fh:
        json.dump({"tag": TAG, "checks": results}, fh, indent=2)
    passed = sum(1 for r in results if r["ok"])
    print(f"\n{passed}/{len(results)} checks passed")
    return 0 if passed == len(results) else 1


if __name__ == "__main__":
    sys.exit(main())
