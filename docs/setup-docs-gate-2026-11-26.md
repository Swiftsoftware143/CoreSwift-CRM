# Gating the setup documentation behind the login — what was tried, what holds, what is next

David, 2026-11-26: *"make sure none of that is before the login."*

## The problem

`/guide.html`, `/onboarding.html` and `/help.html` are **static documents** served by nginx. A browser
navigation to a static file carries no `Authorization` header, so the app never sees the session and anyone
who guesses the URL can read the whole setup walk-through.

## What holds (shipped)

- `GET /api/auth/session/check` — answers a fact only: valid signature, unexpired claim → **204**, otherwise
  **401**. Public by design: the answer *is* the check. It names nobody and reveals nothing.
  Verified live: with `Authorization: Bearer <session>` → **204**; with nothing → **401**.
- The console mirrors its session token into a `cs_token` cookie wherever it sets a token (7 sites) and
  clears it on logout, so the cookie can never outlive the session by more than one request.
- The inline guide is served `Cache-Control: no-store` (it is fetched on every page load, and Cloudflare was
  handing out a version whose origin bytes no longer existed).
- Route census moved to 597 mounted / 583 on `/api` by this change, and the constant says so.

## What does NOT hold — and was reverted

An nginx `auth_request /_session` gate in front of the three documents. **It does not work with a cookie:**

- `proxy_set_header Authorization "Bearer $cookie_cs_token"` → the docs still answered **302** for a
  signed-in reader.
- A temporary probe (`/_cookiecheck`, since removed) proved the cookie *is* visible at the parent request:
  it returned `cookie=eyJhbG… auth=Bearer eyJhbG…`.
- With the token sent as a real `Authorization` header instead, the same location answered **200**.

So nginx's `auth_request` subrequest carries the parent's `$http_*` variables but **not** `$cookie_*`. A gate
that sends the signed-in owner to the login page is worse than no gate, so it was removed the same hour
rather than left half-working.

## The fix to build

Serve the three documents **from the app**, where the session is already resolved:

1. `GET /api/docs/<name>` — validates the session (bearer or the `cs_token` cookie), reads the document's
   bytes, and returns them with `text/html`. Documents ship with the binary (`include_str!`), so there is no
   filesystem dependency inside the container.
2. nginx: `location ~ ^/(guide|onboarding|help)\.html$ { internal; }` so the raw paths stop being public, and
   the console's links point at `/api/docs/…`.
3. Prove three ways: no session → 401/redirect; live session cookie → 200 and the bytes match the repo copy;
   `Authorization: Bearer` → 200.

## Why the app-side cookie parse needs its own test first

`session_check` already reads `cs_token` from the `Cookie` header, and a direct call to it with a cookie
answered the app's own **coded 401** (not the boundary's), i.e. the handler ran and did not accept the
cookie. That is a small, testable defect (`cargo test` over the header extraction), and it must be fixed
before the `/api/docs` route depends on it — otherwise the new route inherits the same 401.
