use super::*;
use axum::{
    body::{to_bytes, Body},
    extract::ConnectInfo,
    http::{header, Method, Request, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use tower::ServiceExt;

const KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn state() -> AuthState {
    AuthState::new(KEY).unwrap()
}

fn with_peer(mut request: Request<Body>) -> Request<Body> {
    request.extensions_mut().insert(ConnectInfo(
        "192.168.0.2:40000".parse::<SocketAddr>().unwrap(),
    ));
    request
}

async fn login(app: &Router) -> (String, String) {
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/auth/login")
        .header(header::HOST, MANAGEMENT_HOST)
        .header(header::ORIGIN, MANAGEMENT_ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!(r#"{{"key":"{KEY}"}}"#)))
        .unwrap();
    let response = app.clone().oneshot(with_peer(request)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let set_cookie = response.headers()[header::SET_COOKIE].to_str().unwrap();
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    assert!(set_cookie.contains("Path=/"));
    assert!(!set_cookie.contains("Domain="));
    let cookie = set_cookie.split(';').next().unwrap().to_owned();
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    (cookie, json["csrf"].as_str().unwrap().to_owned())
}

fn test_app() -> Router {
    let auth = state();
    let protected: Router<AuthState> = Router::new()
        .route("/protected", get(|| async { "ok" }))
        .route(
            "/echo",
            post(|body: String| async move { body.into_response() }),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            auth.clone(),
            require_auth,
        ));
    Router::new()
        .merge(auth_routes(auth.clone()))
        .merge(protected)
        .layer(axum::middleware::from_fn(security_headers))
        .with_state(auth)
}

#[tokio::test]
async fn rejects_protected_get_without_session() {
    let response = test_app()
        .oneshot(
            Request::builder()
                .uri("/protected")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn rejects_protected_post_without_session_before_handler_runs() {
    let response = test_app()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/echo")
                .header(header::HOST, MANAGEMENT_HOST)
                .header(header::ORIGIN, MANAGEMENT_ORIGIN)
                .body(Body::from("must-not-run"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_sets_strict_http_only_cookie_and_returns_csrf() {
    let app = test_app();
    let (cookie, csrf) = login(&app).await;
    assert!(cookie.starts_with("f50_session="));
    assert_eq!(csrf.len(), 64);

    let response = app
        .oneshot(
            Request::builder()
                .uri("/protected")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn authenticated_post_requires_origin_host_and_csrf_without_consuming_body() {
    let app = test_app();
    let (cookie, csrf) = login(&app).await;
    let request = Request::builder()
        .method(Method::POST)
        .uri("/echo")
        .header(header::COOKIE, &cookie)
        .header(header::HOST, MANAGEMENT_HOST)
        .header(header::ORIGIN, MANAGEMENT_ORIGIN)
        .header(CSRF_HEADER, &csrf)
        .body(Body::from("body-must-survive"))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap(),
        "body-must-survive"
    );

    let rejected = Request::builder()
        .method(Method::POST)
        .uri("/echo")
        .header(header::COOKIE, cookie)
        .header(header::HOST, MANAGEMENT_HOST)
        .header(header::ORIGIN, "http://evil.example")
        .header(CSRF_HEADER, csrf)
        .body(Body::from("must-not-run"))
        .unwrap();
    assert_eq!(
        app.oneshot(rejected).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn logout_requires_authenticated_origin_and_csrf() {
    let app = test_app();
    let (cookie, csrf) = login(&app).await;

    let rejected = Request::builder()
        .method(Method::POST)
        .uri("/api/auth/logout")
        .header(header::COOKIE, &cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(rejected).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );

    let accepted = Request::builder()
        .method(Method::POST)
        .uri("/api/auth/logout")
        .header(header::COOKIE, &cookie)
        .header(header::HOST, MANAGEMENT_HOST)
        .header(header::ORIGIN, MANAGEMENT_ORIGIN)
        .header(CSRF_HEADER, csrf)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(accepted).await.unwrap().status(),
        StatusCode::OK
    );

    let protected = Request::builder()
        .uri("/protected")
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.oneshot(protected).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn security_headers_are_present_on_error_responses() {
    let response = test_app()
        .oneshot(
            Request::builder()
                .uri("/protected")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.headers()["x-frame-options"], "DENY");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
}

#[tokio::test]
async fn security_headers_allow_only_required_network_probes() {
    let response = test_app()
        .oneshot(
            Request::builder()
                .uri("/protected")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let csp = response.headers()["content-security-policy"]
        .to_str()
        .unwrap();

    assert!(csp.contains(
        "connect-src 'self' https://ipinfo.io http://myip.ipip.net;"
    ));
    assert!(!csp.contains('*'));
}
