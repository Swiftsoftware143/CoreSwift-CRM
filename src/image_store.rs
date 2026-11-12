//! `image_store` — the ONE accept-store-serve path for a customer-supplied image.
//!
//! Kanban t_feab8aff (porting t_c06a32eb from FunnelSwift). This app holds one customer-supplied
//! image: the email-branding logo (`tenant_logos`). The capability is built ONCE — "accept an image,
//! store it, serve it" — so the three pieces of it live here and nowhere else:
//!
//! * [`read_uploaded_image`] — the multipart read (first part carrying a filename, size cap,
//!   MAGIC-BYTE sniff). The upload handler calls this and then persists what it returns.
//! * [`sniff`] — recognise an image by its bytes, never by the caller's `Content-Type`.
//! * [`image_response`] — serve stored bytes back with the sniffed type pinned and caching that
//!   never invites a shared cache to keep one workspace's logo.
//!
//! The BYTES themselves are stored in the database, for one measured reason: this container binds
//! only its release binary and `migrations/` (`docker inspect coreswiftcrm`), so a file written at
//! run time lives inside the container and dies with the next `docker restart`, and no host webroot
//! could serve it either.

use axum::{
    body::Body,
    extract::Multipart,
    http::{header, StatusCode},
    response::Response,
};

use crate::errors::{ApiResult, AppError};

/// The platform's request-body ceiling for these uploads: axum refuses a body over 2 MB by default.
///
/// The per-route `DefaultBodyLimit` does NOT raise it, and that is measured, not assumed: the body
/// is read by `body_deadline::body_read_deadline_middleware`, which runs OUTSIDE the route layer and
/// therefore reads with axum's own default (measured on FunnelSwift 2026-10-08, kanban t_c06a32eb: a
/// 2 MB + 4 KB multipart body answers `413 Failed to buffer the request body: length limit exceeded`
/// before any handler runs). So the ENVELOPE — image plus multipart framing — is what has to fit in
/// 2 MB.
pub const ENVELOPE_BYTES: usize = 2 * 1024 * 1024;

/// Multipart framing reserve: boundary lines, the part headers and a real filename. Kept out of the
/// envelope so a file the picker accepts can never be refused by the envelope instead of by the
/// handler (which is the difference between the friendly message and a bare 413).
pub const ENVELOPE_HEADROOM: usize = 64 * 1024;

/// The most image bytes an upload may carry.
///
/// `ENVELOPE_BYTES - ENVELOPE_HEADROOM`, because the framing has to fit in the same 2 MB envelope.
/// This is also the number the picker in the tenant console checks before uploading, so the client
/// and the server refuse the SAME set of files. The handler still checks the decoded length as the
/// authoritative backstop.
pub const MAX_IMAGE_BYTES: usize = ENVELOPE_BYTES - ENVELOPE_HEADROOM;

/// The `Content-Type` an upload is served back as, recognised from the MAGIC BYTES.
///
/// These bytes are stored and later served from our own origin with the type this returns, so
/// trusting the client's header would let a caller upload HTML or JavaScript labelled `image/png`
/// and have the app serve it as an image on a CoreSwift URL. SVG is deliberately NOT accepted:
/// it is a scriptable document rather than a bitmap, and this surface carries no HTML sanitiser.
pub fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// Read the first multipart part that carries a filename, cap it, and sniff it.
///
/// Returns `(content_type, bytes)`. Every refusal is a 4xx: no part with a filename, an empty file,
/// one over [`MAX_IMAGE_BYTES`], or bytes that are not a PNG/JPEG/GIF/WebP. A form carrying extra
/// text parts still works — a part with no filename is skipped, not refused.
pub async fn read_uploaded_image(multipart: &mut Multipart) -> ApiResult<(String, Vec<u8>)> {
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| AppError::BadRequest(format!("Multipart error: {e}")))?
    {
        // Only a part that carries a filename is the image; a form with extra text parts still works.
        if field.file_name().is_none() {
            continue;
        }
        let data = field
            .bytes()
            .await
            .map_err(|e| AppError::BadRequest(format!("Failed to read the image: {e}")))?;
        if data.is_empty() {
            return Err(AppError::BadRequest("The image file is empty".into()));
        }
        if data.len() > MAX_IMAGE_BYTES {
            return Err(AppError::BadRequest(format!(
                "The image must be {} bytes or smaller (2 MB minus the upload envelope)",
                MAX_IMAGE_BYTES
            )));
        }
        let kind = sniff(&data)
            .ok_or_else(|| AppError::BadRequest("Upload a PNG, JPEG, GIF or WebP image".into()))?;
        return Ok((kind.to_string(), data.to_vec()));
    }
    Err(AppError::BadRequest("No image was uploaded".into()))
}

// ── Auto-sizing (kanban t_b671a0f7) ────────────────────────────────────────────────────────────
//
// David's requirement: the customer uploads a logo and "never touches a setting" — the app handles
// resizing and sharpness. So the ONE place a logo is stored does the work, at upload:
//
//   * fit inside [`LOGO_STORE_W`] x [`LOGO_STORE_H`] — that is 2x the size it is DISPLAYED at
//     ([`LOGO_DISPLAY_W`] x [`LOGO_DISPLAY_H`]), so the image is still sharp on a retina phone and a
//     4K monitor instead of being resampled up by the client;
//   * never upscale (a 120px logo stays 120px — enlarging it would only blur it);
//   * re-encode as PNG. This is the compatibility decision, not a preference: Outlook (Windows) does
//     not render WebP at all and GIF is a poor fit for a still logo, while PNG has an alpha channel
//     and is drawn by Gmail, Outlook and Apple Mail without a fallback. A JPEG logo becomes PNG too —
//     a flat logo compresses smaller and keeps its edges crisp.
//
// A resized copy is what gets stored, so every mail path (signature, branded header) inherits it.

/// The size the logo is drawn at in an email, in CSS pixels.
pub const LOGO_DISPLAY_W: u32 = 220;
pub const LOGO_DISPLAY_H: u32 = 88;

/// The size actually STORED: 2x the display size, for sharpness on retina screens.
pub const LOGO_STORE_W: u32 = LOGO_DISPLAY_W * 2;
pub const LOGO_STORE_H: u32 = LOGO_DISPLAY_H * 2;

/// A decompression ceiling. A 2 MB PNG can decode to hundreds of megabytes; without a limit one
/// upload could exhaust the container. 64 MB covers a 4000x4000 RGBA image, far more than any logo.
const LOGO_MAX_ALLOC: u64 = 64 * 1024 * 1024;

/// Decode an uploaded image, fit it to the mail-safe stored size, and return `(png_bytes, w, h)`.
///
/// CPU-bound on purpose: callers run it inside `spawn_blocking` so a big decode cannot stall the
/// async reactor that is also serving mail for every other account.
pub fn process_logo_for_email(bytes: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    use image::{ImageFormat, ImageReader};
    use std::io::Cursor;

    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("could not read that image: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(LOGO_MAX_ALLOC);
    reader.limits(limits);

    let img = reader
        .decode()
        .map_err(|e| format!("could not read that image: {e}"))?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err("that image has no pixels".into());
    }

    // Fit inside the box, preserving aspect ratio, and never upscale.
    let scale = f64::min(
        LOGO_STORE_W as f64 / w as f64,
        LOGO_STORE_H as f64 / h as f64,
    );
    let out_img = if scale < 1.0 {
        let nw = ((w as f64) * scale).round().max(1.0) as u32;
        let nh = ((h as f64) * scale).round().max(1.0) as u32;
        img.resize_exact(nw, nh, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let (out_w, out_h) = (out_img.width(), out_img.height());

    let mut png = Vec::new();
    out_img
        .to_rgba8()
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .map_err(|e| format!("could not re-encode that image: {e}"))?;
    if png.is_empty() {
        return Err("could not re-encode that image".into());
    }
    Ok((png, out_w, out_h))
}

/// Serve stored image bytes back.
///
/// `private, max-age=300`: the browser may reuse it for the session; no shared cache is invited to
/// keep one workspace's logo. `nosniff` pins the type to the one sniffed at upload time.
pub fn image_response(content_type: String, bytes: Vec<u8>) -> ApiResult<Response> {
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "private, max-age=300")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .body(Body::from(bytes))
        .map_err(|e| AppError::Internal(format!("Could not build the image response: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_recognises_the_four_allowed_formats_by_magic_bytes() {
        let png = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
        assert_eq!(sniff(&png), Some("image/png"));
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(sniff(b"GIF89a...."), Some("image/gif"));
        assert_eq!(sniff(b"GIF87a...."), Some("image/gif"));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(sniff(&webp), Some("image/webp"));
    }

    #[test]
    fn sniff_refuses_scriptable_and_unknown_payloads() {
        // SVG is a document, not a bitmap — and the label is not consulted either way.
        assert_eq!(
            sniff(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>"),
            None
        );
        assert_eq!(sniff(b"<html><script>alert(1)</script></html>"), None);
        assert_eq!(sniff(b"RIFF1234WAVE"), None);
        assert_eq!(sniff(b""), None);
    }
}
