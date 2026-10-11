use axum::body::Body;
use axum::extract::Request;
use axum::http::{Method, StatusCode, header};
use axum::response::{IntoResponse, Response};

pub(super) async fn serve(request: Request) -> Response {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let (kind, bytes): (&str, &'static [u8]) = match request.uri().path() {
        "/" => (
            "text/html; charset=utf-8",
            include_bytes!("assets/index.html"),
        ),
        "/app.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("assets/app.js"),
        ),
        "/appearance.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("assets/appearance.js"),
        ),
        "/themes.css" => (
            "text/css; charset=utf-8",
            include_bytes!("assets/themes.css"),
        ),
        "/web.css" => ("text/css; charset=utf-8", include_bytes!("assets/web.css")),
        "/bitmap.css" => (
            "text/css; charset=utf-8",
            include_bytes!("../../docs/previews/web/bitmap.css"),
        ),
        "/bitmap-motion.js" => (
            "text/javascript; charset=utf-8",
            include_bytes!("../../docs/previews/web/bitmap-motion.js"),
        ),
        "/favicon.svg" => (
            "image/svg+xml",
            include_bytes!("../../docs/previews/web/favicon.svg"),
        ),
        "/fonts/geist-sans.woff2" => (
            "font/woff2",
            include_bytes!("../../docs/previews/web/fonts/geist-sans.woff2"),
        ),
        "/fonts/inter-latin.woff2" => (
            "font/woff2",
            include_bytes!("../../docs/previews/web/fonts/inter-latin.woff2"),
        ),
        "/fonts/inter-latin-ext.woff2" => (
            "font/woff2",
            include_bytes!("../../docs/previews/web/fonts/inter-latin-ext.woff2"),
        ),
        "/fonts/LICENSE-inter.txt" => (
            "text/plain; charset=utf-8",
            include_bytes!("../../docs/previews/web/fonts/LICENSE-inter.txt"),
        ),
        "/fonts/geist-mono.woff2" => (
            "font/woff2",
            include_bytes!("../../docs/previews/web/fonts/geist-mono.woff2"),
        ),
        "/fonts/geist-pixel-circle.woff2" => (
            "font/woff2",
            include_bytes!("../../docs/previews/web/fonts/geist-pixel-circle.woff2"),
        ),
        "/fonts/geist-pixel-square.woff2" => (
            "font/woff2",
            include_bytes!("../../docs/previews/web/fonts/geist-pixel-square.woff2"),
        ),
        "/fonts/source-han-sans-cn.woff2" => (
            "font/woff2",
            include_bytes!("../../docs/previews/web/fonts/source-han-sans-cn.woff2"),
        ),
        "/fonts/LICENSE-geist.txt" => (
            "text/plain; charset=utf-8",
            include_bytes!("../../docs/previews/web/fonts/LICENSE-geist.txt"),
        ),
        "/fonts/LICENSE-source-han.txt" => (
            "text/plain; charset=utf-8",
            include_bytes!("../../docs/previews/web/fonts/LICENSE-source-han.txt"),
        ),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let body = if *request.method() == Method::HEAD {
        Body::empty()
    } else {
        Body::from(bytes)
    };
    ([(header::CONTENT_TYPE, kind)], body).into_response()
}
