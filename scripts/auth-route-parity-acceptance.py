#!/usr/bin/env python3
"""auth-route-parity-acceptance — kanban t_7d182634 (David, 2026-11-26).

Proves on a RUNNING deployment that CoreSwift answers the fleet-standard auth path
`/api/v1/auth/*` with the SAME router as its native `/api/auth/*`, and that the native path stays as
a permanent alias:

  A  POST /api/v1/auth/login   + the real vault credential      -> 200 with a non-empty access_token
  B  POST /api/v1/auth/login   + a wrong password               -> 401 "Invalid credentials"
     POST /api/auth/login      + the same wrong password        -> 401, SAME body shape (the app's
                                                                  credential refusal, not the
                                                                  boundary's "Authentication required")
  C  POST /api/auth/login      + the real credential            -> 200 (the alias is intact)
  D  POST /api/v1/nonexistent-xyz (anonymous)                   -> 401 "Authentication required"
                                                                  (the boundary was not widened)
  E  the auth bucket is the SAME ONE on both prefixes: six wrong-password posts from one synthetic
     client IP to /api/v1/auth/login answer 401 x5 then 429, the OLD path from that same IP then
     answers 429, and a different IP still answers 401.

The password and the tokens are never printed — only lengths and status codes.

    python3 scripts/auth-route-parity-acceptance.py                     # origin (127.0.0.1:8084)
    python3 scripts/auth-route-parity-acceptance.py --base https://app.coreswiftcrm.com --edge
"""
from __future__ import annotations

import argparse
import json
import random
import sys
import urllib.error
import urllib.request

CRED_ENV = "/opt/swift/secure/cred.env"
VAULT_ENTRY = "coreswift-login-giraudy-capital"
VAULT_URL = "http://127.0.0.1:8100/vault/" + VAULT_ENTRY
ORIGIN = "http://127.0.0.1:8084"
BROWSER_UA = (
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/131.0.0.0 Safari/537.36"
)

PASS: list[str] = []
FAIL: list[str] = []


def env(name: str) -> str:
    for line in open(CRED_ENV):
        line = line.strip()
        if line.startswith(name + "="):
            return line.split("=", 1)[1].strip().strip('"').strip("'")
    raise SystemExit(f"{name} not in {CRED_ENV}")


def vault_credential() -> tuple[str, str]:
    req = urllib.request.Request(
        VAULT_URL, headers={"Authorization": "Bearer " + env("CRED_TOKEN")}
    )
    with urllib.request.urlopen(req, timeout=15) as r:
        doc = json.loads(r.read())
    user = doc.get("username") or doc.get("email")
    pw = doc.get("password")
    if not user or not pw:
        raise SystemExit("vault entry has no username/password")
    return user, pw


def post(base: str, path: str, body: dict, ip: str | None = None, edge: bool = False):
    data = json.dumps(body).encode()
    headers = {"Content-Type": "application/json", "User-Agent": BROWSER_UA}
    if ip:
        # On the origin the real client IP comes from X-Real-IP (nginx sets it in production), so a
        # synthetic address exercises the limiter deterministically without touching a real bucket.
        headers["X-Real-IP"] = ip
    req = urllib.request.Request(base + path, data=data, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(req, timeout=20) as r:
            return r.status, r.read().decode()
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode()


def check(name: str, ok: bool, detail: str) -> None:
    (PASS if ok else FAIL).append(f"{name}: {detail}")
    print(("PASS " if ok else "FAIL ") + name + ": " + detail)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--base", default=ORIGIN)
    ap.add_argument("--edge", action="store_true", help="rate-limit legs go light (real source IP)")
    a = ap.parse_args()
    base = a.base.rstrip("/")
    user, pw = vault_credential()
    wrong = {"email": user, "password": "not-the-password-" + str(len(pw))}

    # A — the fleet-standard path, real credential.
    st, body = post(base, "/api/v1/auth/login", {"email": user, "password": pw}, ip="203.0.113.11")
    doc = json.loads(body) if body else {}
    tok = doc.get("access_token") or ""
    check(
        "A /api/v1/auth/login real credential",
        st == 200 and bool(tok),
        f"status={st} access_token_len={len(tok)} keys={sorted(k for k in doc if 'token' in k or k in ('team_member','platform_admin'))}",
    )

    # C — the alias, real credential.
    st_c, body_c = post(base, "/api/auth/login", {"email": user, "password": pw}, ip="203.0.113.12")
    doc_c = json.loads(body_c) if body_c else {}
    tok_c = doc_c.get("access_token") or ""
    check(
        "C /api/auth/login alias real credential",
        st_c == 200 and bool(tok_c),
        f"status={st_c} access_token_len={len(tok_c)}",
    )

    # B — the handler, not the boundary: wrong password, both paths, same body shape.
    st_b, body_b = post(base, "/api/v1/auth/login", wrong, ip="203.0.113.13")
    st_o, body_o = post(base, "/api/auth/login", wrong, ip="203.0.113.14")
    db_ = json.loads(body_b) if body_b else {}
    do_ = json.loads(body_o) if body_o else {}
    same_keys = sorted(db_.keys()) == sorted(do_.keys())
    msg = json.dumps(db_)
    check(
        "B /api/v1/auth/login wrong password",
        st_b == 401 and "Invalid credentials" in msg and "Authentication required" not in msg,
        f"status={st_b} body_keys={sorted(db_.keys())} says_invalid_credentials={'Invalid credentials' in msg}",
    )
    check(
        "B parity with /api/auth/login wrong password",
        st_o == 401 and same_keys and json.dumps(do_.get("message", do_)) == json.dumps(db_.get("message", db_)),
        f"old_status={st_o} same_body_keys={same_keys} identical_message={json.dumps(do_.get('message', do_)) == json.dumps(db_.get('message', db_))}",
    )

    # D — the guard was not widened.
    st_d, body_d = post(base, "/api/v1/nonexistent-xyz", {"probe": True}, ip="203.0.113.15")
    dd = json.loads(body_d) if body_d else {}
    check(
        "D /api/v1/nonexistent-xyz still refused by the boundary",
        st_d == 401 and dd.get("error") == "Authentication required",
        f"status={st_d} error={dd.get('error')}",
    )

    # E — one auth bucket on both prefixes (5/min per client IP).
    # A FRESH bucket per run: the governor is a 5-per-minute token bucket keyed by client IP, so a
    # fixed address would inherit the previous run's tokens and make this leg flaky (measured: a
    # re-run inside the refill window answered [401, 429, 429, 429, 429, 429]). TEST-NET-2/3
    # addresses are documentation-only, so no real client can share this bucket.
    ip = f"198.51.100.{random.randint(10, 250)}"
    codes = []
    for i in range(6):
        st, _ = post(base, "/api/v1/auth/login", wrong, ip=ip)
        codes.append(st)
    check(
        "E /api/v1/auth/login is rate-limited",
        codes[:5].count(401) == 5 and codes[5] == 429,
        f"six wrong-password posts from {ip} -> {codes}",
    )
    st_old, _ = post(base, "/api/auth/login", wrong, ip=ip)
    check(
        "E the OLD path shares that same bucket",
        st_old == 429,
        f"old path from the exhausted IP -> {st_old}",
    )
    st_fresh, _ = post(base, "/api/auth/login", wrong, ip=f"198.51.101.{random.randint(10, 250)}")
    check(
        "E the bucket is per client IP, not global",
        st_fresh == 401,
        f"old path from a fresh IP -> {st_fresh}",
    )

    print(f"\n{len(PASS)} passed, {len(FAIL)} failed")
    for f in FAIL:
        print("  FAILED " + f)
    return 0 if not FAIL else 1


if __name__ == "__main__":
    sys.exit(main())
