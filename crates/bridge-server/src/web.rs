//! Serves the built management UI, so a deployment needs only this process behind
//! whatever reverse proxy terminates TLS.
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::path::{Component, Path, PathBuf};

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'";

pub fn router(root: PathBuf) -> Router {
    Router::new().fallback(serve).with_state(root)
}

async fn serve(State(root): State<PathBuf>, request: Request) -> Response {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let Some(path) = resolve(&root, request.uri().path()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(bytes) = tokio::fs::read(&path).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut response = if request.method() == Method::HEAD {
        Response::new(Body::empty())
    } else {
        Response::new(Body::from(bytes))
    };
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime(&path)));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CSP),
    );
    response
}

/// Only plain file names below the UI directory; anything that could leave it is not found.
fn resolve(root: &Path, uri_path: &str) -> Option<PathBuf> {
    let relative = uri_path.trim_start_matches('/');
    let relative = if relative.is_empty() || relative.ends_with('/') {
        format!("{relative}index.html")
    } else {
        relative.to_owned()
    };
    let relative = Path::new(&relative);
    if !relative
        .components()
        .all(|part| matches!(part, Component::Normal(_)))
    {
        return None;
    }
    let path = root.join(relative);
    path.is_file().then_some(path)
}

fn mime(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "webmanifest") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    async fn get(root: &Path, method: Method, uri: &str) -> Response {
        router(root.to_path_buf())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn serves_the_ui_with_its_headers_and_nothing_outside_it() {
        let temp = std::env::temp_dir().join(format!("bridge-ui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(temp.join("ui/assets")).unwrap();
        std::fs::write(temp.join("ui/index.html"), "<html></html>").unwrap();
        std::fs::write(temp.join("ui/assets/app.js"), "ok").unwrap();
        std::fs::write(temp.join("secret.txt"), "private").unwrap();
        let root = temp.join("ui");

        let index = get(&root, Method::GET, "/").await;
        assert_eq!(index.status(), StatusCode::OK);
        assert_eq!(
            index.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        assert!(
            index.headers()[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        let script = get(&root, Method::GET, "/assets/app.js").await;
        assert_eq!(script.status(), StatusCode::OK);
        assert_eq!(
            script.headers()[header::CONTENT_TYPE],
            "text/javascript; charset=utf-8"
        );
        for escape in [
            "/../secret.txt",
            "/assets/../../secret.txt",
            "/%2e%2e/secret.txt",
        ] {
            assert_eq!(
                get(&root, Method::GET, escape).await.status(),
                StatusCode::NOT_FOUND
            );
        }
        assert_eq!(
            get(&root, Method::GET, "/missing.js").await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            get(&root, Method::POST, "/").await.status(),
            StatusCode::METHOD_NOT_ALLOWED
        );
        std::fs::remove_dir_all(temp).unwrap();
    }
}
