#!/usr/bin/env python3
"""route-census — resolve every `.route(..)` this app mounts to a concrete request path.

    python3 scripts/route-census.py            # census this repo (src/ next to this script)
    python3 scripts/route-census.py <repo-root>

Prints `VERBS<TAB>PATH` per mount on stdout and a `# total mounts: N; unique paths: M` summary on
stderr. The counts it prints are the ones `src/auth/route_policy.rs`'s `the_census_shape_is_what_the_docs_say`
test pins, so a route added or removed anywhere in the tree fails that test until the module docs and
this census agree again.

WHY A TOKENIZER AND NOT A REGEX PER CALL
    The first cut matched a whole `.route(path, handler)` in one regex. Every MULTI-LINE call —
        .route(
            "/",
            post(internal_create),
        )
    — was therefore invisible, and the first census silently omitted `/api/internal/contacts` and
    `/api/internal/lists` (measured 2026-10-06). Here each `.route(` / `.nest(` / `.nest_service(`
    occurrence is a token and the text between one token and the next belongs to it, so a call can
    span any number of lines.

Every `.nest(..)` in this app lives in `src/main.rs` (52 of them, measured) and no module router
nests another, so resolving `main.rs` + one level of `nest` targets is the whole tree. The script
fails loudly (`UNRESOLVED-FILE` / `NOBODY`) rather than dropping a nest it cannot follow.
"""
import os
import re
import sys

ROOT = os.path.abspath(sys.argv[1]) if len(sys.argv) > 1 else os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src") if os.path.isdir(os.path.join(ROOT, "src")) else ROOT

TOK_RE = re.compile(r'\.(route|nest|nest_service)\(\s*"([^"]*)"')
NEST_TARGET_RE = re.compile(r'\s*,?\s*([A-Za-z0-9_:]+?)\s*::\s*([a-z_0-9]+)\s*\(')
VERBS = ("get", "post", "put", "patch", "delete")


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
            tgt = NEST_TARGET_RE.match(chunk)
            if not tgt:
                print("# UNPARSED-NEST %s%s %r" % (prefix, what, chunk.strip()[:60]), file=sys.stderr)
                out.append(("@NEST-TARGET:" + chunk.strip()[:40], prefix + what, ""))
                continue
            mod, fn = tgt.group(1), tgt.group(2)
            if depth > 6:
                continue
            key = (prefix + what, mod, fn)
            if key in seen:
                continue
            seen.add(key)
            nf = find_file(mod)
            if not nf:
                print("# UNRESOLVED-FILE %s %s::%s" % (prefix + what, mod, fn), file=sys.stderr)
                out.append(("@UNRESOLVED:" + mod, prefix + what, fn))
                continue
            body = fn_body(read(nf), fn)
            if not body:
                print("# NOBODY %s %s::%s (%s)" % (prefix + what, mod, fn, nf), file=sys.stderr)
                out.append(("@NOBODY:" + mod + "::" + fn, prefix + what, ""))
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
    print("# total mounts: %d; unique paths: %d" % (len(full), len({p for p, _ in full})), file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
