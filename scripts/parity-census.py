#!/usr/bin/env python3
"""PARITY CENSUS — does every route have (a) a console screen and (b) an issued-key/MCP door?

David's rule (2026-11-27): "the MCP and the API should touch every endpoint and every endpoint should
touch the front end UI."

Authoritative input is `/tmp/mounted-routes.txt`, dumped by the `dump_mounted_routes` test in
`src/auth/route_policy.rs` — the SAME walk the pinned route count uses. Do NOT regex the source: a
regex sees a fraction of the mounted set (measured: 24 of 480).

    cargo test --bin crm-swift dump_mounted_routes      # refresh the list
    python3 scripts/parity-census.py                    # report gaps

Both columns are LOWER BOUNDS on coverage, for a reason worth keeping in mind before quoting them:
  * UI: a path built at run time ('/api/booking/calendars/' + id) counts as covered by its prefix
    match, so a route can look covered when only its prefix is used. Treat the "no screen" count as
    the reliable number; a "has screen" verdict means "some string like this appears in the console".
  * KEY SURFACE: a route is matched by its own literal or its parent prefix appearing in
    route_policy.rs. A route served only through the generic `coreswift_api` MCP tool is REACHABLE by a
    key without appearing on the enumeration, because that tool forwards any path.
"""
import glob
import os
import subprocess
import sys

BASE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ROUTES = "/tmp/mounted-routes.txt"


def main() -> int:
    if not os.path.exists(ROUTES):
        subprocess.run(
            ["cargo", "test", "--bin", "crm-swift", "dump_mounted_routes", "-q"],
            cwd=BASE,
            check=False,
        )
    if not os.path.exists(ROUTES):
        print("INCOMPLETE — no route dump; run: cargo test --bin crm-swift dump_mounted_routes")
        return 1

    routes = [l.strip() for l in open(ROUTES) if l.strip()]
    api = sorted({r for r in routes if r.startswith("/api")})

    ui = ""
    patterns = ["www-app/coreswift/*", "www-admin/*.js", "public/*.html"]
    for pat in patterns:
        for f in glob.glob(os.path.join(BASE, pat)):
            try:
                ui += open(f, errors="replace").read()
            except OSError:
                continue
    pol = open(os.path.join(BASE, "src/auth/route_policy.rs"), errors="replace").read()

    def in_ui(path: str) -> bool:
        parent = "/".join(path.split("/")[:-1]) + "/"
        return any(c and c in ui for c in (path, path.replace("/api", "", 1), parent))

    def on_key_surface(path: str) -> bool:
        parent = "/".join(path.split("/")[:-1])
        return path in pol or (len(parent) > 6 and (parent + '"') in pol)

    no_ui = [r for r in api if not in_ui(r)]
    no_key = [r for r in api if not on_key_surface(r)]

    print(f"mounted API routes: {len(api)}")
    print(f"  console-referenced: {len(api) - len(no_ui)}    no screen: {len(no_ui)}")
    print(f"  on the key surface: {len(api) - len(no_key)}    key-less:  {len(no_key)}")
    if no_ui:
        print("\nno screen:")
        for r in no_ui:
            print("  ", r)
    if no_key:
        print("\nno issued-key door:")
        for r in no_key:
            print("  ", r)
    return 0


if __name__ == "__main__":
    sys.exit(main())
