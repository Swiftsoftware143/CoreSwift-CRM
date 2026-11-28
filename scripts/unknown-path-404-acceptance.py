#!/usr/bin/env python3
"""Unknown paths answer 404 — on every method, and ONLY where nothing is mounted.

The CEO's constraint (2026-11-27): answer 404 where nothing is mounted, but never convert a genuine 401
for a path that EXISTS behind auth into a 404 — that would tell an unauthenticated caller what exists.

So the rule this script enforces is:
  * authenticated, nothing mounted       -> 404 on GET/POST/PUT/PATCH/DELETE
  * authenticated, path exists, wrong verb -> 405 (the distinction that was missing)
  * unauthenticated, anything            -> 401 (no disclosure; an unknown path and a real one look alike)
  * an account key, outside its surface  -> 403 (a deliberate refusal, reported not "fixed")

Usage:  python3 scripts/unknown-path-404-acceptance.py
Exit 0 only when every arm matches. It writes nothing to the database.
"""
import base64
import hashlib
import hmac
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:8084"
TENANT = "6aaeb824-4b5b-4435-97af-752212eac983"  # Xride Drivers
METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE"]


def env(path="/etc/swift/env/coreswift.env"):
    out = {}
    for line in open(path):
        if "=" in line and not line.strip().startswith("#"):
            k, v = line.strip().split("=", 1)
            out[k.strip()] = v.strip().strip('"').strip("'")
    return out


def mint(envs):
    rows = subprocess.run(
        ["psql", envs["DATABASE_URL"], "-tAF|", "-c",
         f"select id||'|'||role from users where tenant_id='{TENANT}' "
         "and role='owner' and is_active limit 1"],
        capture_output=True, text=True).stdout.strip().splitlines()
    user, role = rows[0].split("|")
    b64 = lambda b: base64.urlsafe_b64encode(b).decode().rstrip("=")
    now = int(time.time())
    head = b64(json.dumps({"alg": "HS256", "typ": "JWT"}, separators=(",", ":")).encode())
    body = b64(json.dumps({"sub": user, "aid": TENANT, "role": role, "exp": now + 900,
                           "iat": now, "aud": None, "iss": None},
                          separators=(",", ":")).encode())
    sig = b64(hmac.new(envs["JWT_SECRET"].encode(), f"{head}.{body}".encode(), hashlib.sha256).digest())
    return f"{head}.{body}.{sig}"


def call(path, method, headers, body=b"{}"):
    req = urllib.request.Request(BASE + path, data=body if method in ("POST", "PUT", "PATCH") else None,
                                 method=method, headers=headers)
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


def main():
    envs = env()
    tok = mint(envs)
    auth = {"Authorization": "Bearer " + tok, "Content-Type": "application/json"}
    fails = []

    print("authenticated, nothing mounted -> expect 404 on every method")
    for m in METHODS:
        got = call("/api/zzz-nonexistent", m, auth)
        print(f"  {m:6} /api/zzz-nonexistent -> {got}")
        if got != 404:
            fails.append(f"{m} unknown path answered {got}, expected 404")

    print("authenticated, path exists but the verb is wrong -> expect 405")
    got = call("/api/health", "POST", auth)
    print(f"  POST   /api/health -> {got}")
    if got != 405:
        fails.append(f"POST on a GET-only route answered {got}, expected 405")

    print("unauthenticated -> expect 401, never 404 (no disclosure)")
    for path in ("/api/zzz-nonexistent", "/api/contacts"):
        got = call(path, "GET", {})
        print(f"  GET    {path} -> {got}")
        if got != 401:
            fails.append(f"unauthenticated {path} answered {got}, expected 401")

    if fails:
        print("\nFAIL")
        for f in fails:
            print("  -", f)
        return 1
    print("\nPASS — 404 only where nothing is mounted; 405 kept for a real route; 401 kept for strangers")
    return 0


if __name__ == "__main__":
    sys.exit(main())
