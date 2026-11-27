//! The setup documentation, served behind the session (David, 2026-11-26: "make sure none of that is
//! before the login").
//!
//! `/guide.html`, `/onboarding.html` and `/help.html` are static files, and a browser navigation to a
//! static file carries no `Authorization` header — so the web server could not tell a signed-in reader from
//! a stranger and anyone who guessed the URL could read exactly how the account is wired.
//!
//! They are served from here instead, where the session is already resolved by the same middleware as
//! every other private route. The documents ship INSIDE the binary (`include_str!`), so the container needs
//! no copy of the front-end tree and the bytes cannot drift from what was built.

use axum::extract::Path;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

/// The three documents, by the name the console asks for.
const GUIDE: &str = include_str!("../public/guide.html");
const ONBOARDING: &str = include_str!("../public/onboarding.html");
const HELP: &str = include_str!("../public/help.html");

fn doc(name: &str) -> Option<&'static str> {
    match name.trim().trim_end_matches(".html") {
        "guide" => Some(GUIDE),
        "onboarding" => Some(ONBOARDING),
        "help" => Some(HELP),
        _ => None,
    }
}

/// GET /api/docs/:name — the document's own bytes, to a caller holding a live session.
///
/// Reached only through the authenticated API, so a stranger gets the ordinary 401 with no body. The name is
/// matched against a fixed list; nothing here touches the filesystem, so no path can be walked.
pub async fn get_doc(Path(name): Path<String>) -> Response {
    match doc(&name) {
        Some(body) => Html(body).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            "no such document: guide, onboarding or help",
        )
            .into_response(),
    }
}
