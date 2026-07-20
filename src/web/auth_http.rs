use std::{net::SocketAddr, sync::Arc};

use anyhow::{Context, Result};
use axum::{
    extract::{ConnectInfo, Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware,
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use super::{auth::AuthCore, auth_store};

pub const MANAGEMENT_HOST: &str = "192.168.0.1:14180";
pub const MANAGEMENT_ORIGIN: &str = "http://192.168.0.1:14180";
pub const CSRF_HEADER: &str = "x-f50-csrf";
const SESSION_COOKIE: &str = "f50_session";

#[derive(Clone)]
pub struct AuthState {
    core: Arc<AuthCore>,
}

impl AuthState {
    pub fn load() -> Result<Self> {
        let key = auth_store::read_management_key().context("管理认证不可用")?;
        Self::new(&key).map_err(|_| anyhow::anyhow!("管理密钥无效"))
    }

    pub fn new(key: &str) -> std::result::Result<Self, super::auth::AuthError> {
        Ok(Self {
            core: Arc::new(AuthCore::new(key.as_bytes())?),
        })
    }
}

pub fn auth_routes(auth: AuthState) -> Router<AuthState> {
    let protected_logout = Router::new()
        .route("/api/auth/logout", post(logout))
        .route_layer(middleware::from_fn_with_state(auth, require_auth));
    Router::new()
        .route("/api/auth/login", post(login))
        .route("/api/auth/session", get(session))
        .merge(protected_logout)
}

#[derive(Deserialize)]
struct LoginRequest {
    key: String,
}

#[derive(Serialize)]
struct SessionResponse {
    authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    csrf: Option<String>,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: &'static str,
    code: &'static str,
}

async fn login(
    State(state): State<AuthState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(payload): Json<LoginRequest>,
) -> Response {
    if !trusted_browser_origin(&headers) {
        return error_response(StatusCode::FORBIDDEN, "请求来源不受信任", "forbidden");
    }
    match state.core.login(peer.ip(), payload.key.as_bytes()) {
        Ok(login) => {
            let cookie = format!(
                "{SESSION_COOKIE}={}; HttpOnly; SameSite=Strict; Path=/; Max-Age=43200",
                login.session_id
            );
            let mut response = Json(SessionResponse {
                authenticated: true,
                csrf: Some(login.csrf_nonce),
            })
            .into_response();
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_str(&cookie).expect("随机十六进制 Cookie 必须是合法 header"),
            );
            response
        }
        Err(super::auth::AuthError::RateLimited { retry_after_secs }) => {
            let mut response = error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "认证尝试过于频繁",
                "rate_limited",
            );
            response.headers_mut().insert(
                header::RETRY_AFTER,
                HeaderValue::from_str(&retry_after_secs.to_string())
                    .expect("重试秒数必须是合法 header"),
            );
            response
        }
        _ => error_response(StatusCode::UNAUTHORIZED, "认证失败", "unauthorized"),
    }
}

async fn session(State(state): State<AuthState>, headers: HeaderMap) -> Json<SessionResponse> {
    let authenticated = session_id(&headers).and_then(|id| state.core.validate_session(id));
    Json(SessionResponse {
        authenticated: authenticated.is_some(),
        csrf: authenticated.map(|session| session.csrf_nonce),
    })
}

async fn logout(State(state): State<AuthState>, headers: HeaderMap) -> Response {
    if let Some(id) = session_id(&headers) {
        state.core.logout(id);
    }
    let mut response = Json(SessionResponse {
        authenticated: false,
        csrf: None,
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("f50_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
    );
    response
}

pub async fn require_auth(
    State(state): State<AuthState>,
    request: Request,
    next: Next,
) -> Response {
    let Some(id) = session_id(request.headers()) else {
        return error_response(StatusCode::UNAUTHORIZED, "需要登录", "unauthorized");
    };
    if matches!(*request.method(), Method::GET | Method::HEAD) {
        if state.core.validate_session(id).is_none() {
            return error_response(StatusCode::UNAUTHORIZED, "会话已失效", "unauthorized");
        }
    } else {
        if !trusted_browser_origin(request.headers()) {
            return error_response(StatusCode::FORBIDDEN, "请求来源不受信任", "forbidden");
        }
        let csrf = request
            .headers()
            .get(CSRF_HEADER)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !state.core.validate_csrf(id, csrf) {
            let status = if state.core.has_session(id) {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::UNAUTHORIZED
            };
            return error_response(status, "认证校验失败", "unauthorized");
        }
    }
    next.run(request).await
}

pub async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert("x-frame-options", HeaderValue::from_static("DENY"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self' https://ipinfo.io http://myip.ipip.net; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
        ),
    );
    response
}

fn trusted_browser_origin(headers: &HeaderMap) -> bool {
    header_eq(headers, header::HOST, MANAGEMENT_HOST)
        && header_eq(headers, header::ORIGIN, MANAGEMENT_ORIGIN)
}

fn header_eq(headers: &HeaderMap, name: header::HeaderName, expected: &str) -> bool {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == expected)
}

fn session_id(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .map(str::trim)
        .find_map(|cookie| cookie.strip_prefix("f50_session="))
        .filter(|value| !value.is_empty())
}

fn error_response(status: StatusCode, error: &'static str, code: &'static str) -> Response {
    (status, Json(ErrorResponse { error, code })).into_response()
}

#[cfg(test)]
mod tests;
