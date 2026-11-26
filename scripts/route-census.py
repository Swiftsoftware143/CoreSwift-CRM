#!/usr/bin/env python3
"""EXPLORATORY ONLY — these totals are a LOWER BOUND, never a number to pin.

The authoritative route count is the Rust side: `cargo test --bin crm-swift auth::route_policy` walks the
router with `mounted_routes()`, and the `left` value it reports is the number to record in the module docs.

This script is a regex parser over source text and cannot see everything the compiler sees: on 2026-11-25 it
read 553 mounts against the test's 576. When it meets something it cannot follow it prints INCOMPLETE, prints
NO totals, and exits 1 — because a number printed here once got pinned into route_policy.rs, and a census
that is short by any amount turns the unreachable-screen guard OFF while looking green.

    python3 scripts/route-census.py            # census this repo (src/ next to this script)
    python3 scripts/route-census.py <repo-root>

KNOWN CONCRETE MISS — fix against this route, not against a number: the operator's
`/api/booking/bookings/:id/cancel` is mounted from `booking::admin_router`, which `main.rs` nests with a
`.layer(from_fn(...))` chain, and that is the shape this walk skips. A count can be made to agree without
seeing one more mount, so a fixer must be able to show this path in the output.

It follows `.route(`, `.nest(`, `.nest_service(` and `.merge(`, and it follows a MERGED nest argument
(`kb::router(s).merge(canned::router(s))` is three routers, not one). A call that builds a router is
recognised by its name: `router` or `*_router`; anything else in a nest argument is a layer, not a gap.
"""
import os
import re
import sys

ROOT = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src") if os.path.isdir(os.path.join(ROOT, "src")) else ROOT

TOK_RE = re.compile(r'\.(route|nest|nest_service)\(\s*"([^"]*)"')
# `.merge(other::router())` adds routes at the SAME prefix. Missing it under-counted the walk (536 against the
# test's 576, measured 2026-11-25).
MERGE_RE = re.compile(r'\.merge\(\s*([A-Za-z0-9_:]+?)\s*::\s*([a-z_0-9]+)\s*\(')
NEST_TARGET_RE = re.compile(r'\s*,?\s*([A-Za-z0-9_:]+?)\s*::\s*([a-z_0-9]+)\s*\(')
# Every `mod::fn(` in a nest's argument, so a MERGED router is followed rather than half-read.
# `main.rs` nests `kb::router(s).merge(canned::router(s)).merge(reports::router(s))`; matching only the
# first name made the script miss 56 mounts and then print totals that looked authoritative (measured
# 2026-11-25: script said 520, the test pins 576). "Cannot see" must never read as "clean".
NEST_CHAIN_RE = re.compile(r'([A-Za-z0-9_:]+?)\s*::\s*([a-z_0-9]+)\s*\(')
VERBS = ("get", "post", "put", "patch", "delete")

# Anything the walk could not follow. If this is non-empty the totals are WRONG, and printing them anyway is
# how a 56-short count got pinned (2026-11-25). main() exits non-zero instead.
problems = []


def read(path):
    try:
        with open(path) as fh:
            return fh.read()
    except OSError:
        return ""


def find_file(modpath):
    parts = modpath.split("::")
    for cand in (os.path.join(SRC, *parts) + ".rs", os.path.join(SRC, *parts, "mod.rs")):
        if os.path.isfile(cand):
            return cand
    return None


def fn_body(text, fn):
    """The body of `fn <fn>`, assuming rustfmt puts the closing brace at column 0."""
    m = re.search(r'\n(?:pub )?(?:async )?fn ' + re.escape(fn) + r'\s*[(<]', text)
    if not m:
        return ""
    start = text.index("{", m.end() - 1)
    end = text.find("\n}", start)
    return text[start:] if end < 0 else text[start:end]


def scan_router(text, prefix, out, seen, depth=0):
    # Merged routers first: they contribute at this same prefix and have no token of their own.
    for mod, fn in MERGE_RE.findall(text):
        if not (fn == "router" or fn.endswith("_router")):
            continue
        if mod.startswith("crate::"):
            mod = mod[len("crate::"):]
        if mod.split("::")[0] in ("axum", "std", "tower", "tower_http"):
            continue
        key = (prefix, mod, fn)
        if key in seen or depth > 6:
            continue
        seen.add(key)
        nf = find_file(mod)
        if not nf:
            print("# UNRESOLVED-FILE %s %s::%s" % (prefix or "/", mod, fn), file=sys.stderr)
            problems.append("%s::%s" % (mod, fn))
            continue
        body = fn_body(read(nf), fn)
        if not body:
            print("# NOBODY %s %s::%s (%s)" % (prefix or "/", mod, fn, nf), file=sys.stderr)
            problems.append("%s::%s" % (mod, fn))
            continue
        scan_router(body, prefix, out, seen, depth + 1)

    toks = list(TOK_RE.finditer(text))
    for i, m in enumerate(toks):
        nxt = toks[i + 1].start() if i + 1 < len(toks) else len(text)
        chunk = text[m.end():nxt]
        kind, what = m.group(1), m.group(2)
        if kind == "route":
            # axum nests a router's `/` route at the nest PREFIX itself, not at `prefix/`
            # (verified live 2026-10-06: /api/contacts -> 401, /api/contacts/ -> 404).
            path = "" if what == "/" else what
            verbs = [v.upper() for v in VERBS if re.search(r'\b' + v + r'\s*\(', chunk)]
            out.append((path, prefix, ",".join(verbs) or "?"))
        elif kind == "nest":
            # Take the WHOLE nest argument (up to the matching `)`) so a MERGED chain is visible:
            # `kb::router(s).merge(canned::router(s)).merge(reports::router(s))` is three routers, and
            # following only the first made the walk miss 56 mounts here (measured 2026-11-25: the script
            # said 520 while the test pins 576). "Cannot see" must never read as "clean".
            arg = chunk
            depth_paren, cut = 0, len(arg)
            for idx, ch in enumerate(arg):
                if ch == "(":
                    depth_paren += 1
                elif ch == ")":
                    if depth_paren == 0:
                        cut = idx
                        break
                    depth_paren -= 1
            chain = NEST_CHAIN_RE.findall(arg[:cut])
            if not chain:
                print("# UNPARSED-NEST %s%s %r" % (prefix, what, chunk.strip()[:60]), file=sys.stderr)
                problems.append("nest %s%s" % (prefix, what))
                out.append(("@" + "NEST-TARGET:" + chunk.strip()[:40], prefix + what, ""))
                continue
            if depth > 6:
                continue
            for mod, fn in chain:
                # A nest's argument also carries LAYERS (`axum::middleware::from_fn_with_state(...)`). Only
                # functions that build a router are routers; anything else is skipped rather than reported as
                # a failure to follow.
                if not (fn == "router" or fn.endswith("_router")):
                    continue
                if mod.startswith("crate::"):
                    mod = mod[len("crate::"):]
                if mod.split("::")[0] in ("axum", "std", "tower", "tower_http"):
                    continue
                key = (prefix + what, mod, fn)
                if key in seen:
                    continue
                seen.add(key)
                nf = find_file(mod)
                if not nf:
                    print("# UNRESOLVED-FILE %s %s::%s" % (prefix + what, mod, fn), file=sys.stderr)
                    problems.append("%s::%s" % (mod, fn))
                    out.append(("@" + "UNRESOLVED:" + mod, prefix + what, fn))
                    continue
                body = fn_body(read(nf), fn)
                if not body:
                    print("# NOBODY %s %s::%s (%s)" % (prefix + what, mod, fn, nf), file=sys.stderr)
                    problems.append("%s::%s" % (mod, fn))
                    out.append(("@" + "NOBODY:" + mod + "::" + fn, prefix + what, ""))
                    continue
                scan_router(body, prefix + what, out, seen, depth + 1)
        else:  # nest_service
            out.append((what + "*", prefix, "SERVED"))


def main():
    main_rs = read(os.path.join(SRC, "main.rs"))
    start = main_rs.find("let app = Router::new()")
    end = main_rs.find(".with_state(state.clone())", start)
    if start < 0 or end < 0:
        print("route-census: cannot find the router build in %s/main.rs" % SRC, file=sys.stderr)
        return 2
    out = []
    scan_router(main_rs[start:end], "", out, set())
    full = sorted((prefix + path, verbs) for path, prefix, verbs in out)
    for path, verbs in full:
        print("{}\t{}".format(verbs, path))
    if problems:
        # INCOMPLETE. Say so, name what was missed, and print NO totals: a number printed here is a number
        # somebody pins into route_policy.rs, and a short census turns the unreachable-screen guard OFF.
        print(
            "# INCOMPLETE — %d nest(s) could not be followed: %s"
            % (len(problems), ", ".join(sorted(set(problems))[:8])),
            file=sys.stderr,
        )
        print(
            "# no totals printed. The authoritative count is the Rust side: "
            "`cargo test --bin crm-swift auth::route_policy` (mounted_routes() walks the router itself).",
            file=sys.stderr,
        )
        return 1
    print("# total mounts: %d; unique paths: %d" % (len(full), len({p for p, _ in full})), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
