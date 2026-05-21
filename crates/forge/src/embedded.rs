//! Built-in embedded frontend handler.
//!
//! Provides `serve_embedded_assets` which creates an SPA-aware handler from
//! any type implementing `rust_embed::Embed`. Users just derive the embed
//! struct and pass it as a type parameter.
//!
//! # Example
//!
//! ```ignore
//! #[derive(rust_embed::Embed)]
//! #[folder = "frontend/dist"]
//! struct Assets;
//!
//! let builder = builder.frontend_handler(forge::serve_embedded_assets::<Assets>);
//! ```

use std::future::Future;
use std::pin::Pin;

use axum::body::Body;
use axum::http::Request;
use axum::response::Response;

/// Serve embedded assets from a `rust_embed::Embed` type with SPA fallback.
///
/// Resolution order:
/// 1. Exact path match (e.g. `_app/immutable/...`, `sitemap.xml`)
/// 2. `{path}.html` for flat pre-rendered SvelteKit output (e.g. `/about` → `about.html`)
/// 3. `{path}/index.html` for nested pre-rendered output (e.g. `/about` → `about/index.html`)
/// 4. `200.html` SPA fallback for client-side routes (SvelteKit adapter-static convention)
/// 5. `index.html` fallback for backwards compatibility
///
/// Use as a type-parameterized function pointer with `ForgeBuilder::frontend_handler()`.
pub fn serve_embedded_assets<E: rust_embed::Embed + 'static>(
    req: Request<Body>,
) -> Pin<Box<dyn Future<Output = Response> + Send>> {
    Box::pin(async move {
        use axum::http::{StatusCode, header};
        use axum::response::IntoResponse;

        let path = req.uri().path().trim_start_matches('/');
        let path = if path.is_empty() { "index.html" } else { path };

        if let Some(content) = E::get(path) {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            return ([(header::CONTENT_TYPE, mime.as_ref())], content.data).into_response();
        }

        let flat = format!("{}.html", path);
        if let Some(content) = E::get(&flat) {
            return ([(header::CONTENT_TYPE, "text/html")], content.data).into_response();
        }

        let nested = format!("{}/index.html", path);
        if let Some(content) = E::get(&nested) {
            return ([(header::CONTENT_TYPE, "text/html")], content.data).into_response();
        }

        let fallback = E::get("200.html").or_else(|| E::get("index.html"));
        match fallback {
            Some(content) => ([(header::CONTENT_TYPE, "text/html")], content.data).into_response(),
            None => (StatusCode::NOT_FOUND, "not found").into_response(),
        }
    })
}
