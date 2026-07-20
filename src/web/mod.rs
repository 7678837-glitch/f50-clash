pub mod auth;
pub mod auth_http;
pub mod auth_store;

use std::net::SocketAddr;

use anyhow::{bail, Context, Result};
use axum::{
    body::to_bytes, extract::Request, middleware, response::Html, routing::get, routing::post,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tokio::{net::TcpListener, sync::watch, task, time};
use tracing::{info, warn};

use crate::{process, status, subscription};

const BIND_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

pub async fn run(addr: SocketAddr, mut shutdown: watch::Receiver<bool>) -> Result<()> {
    let auth = auth_http::AuthState::load()?;
    let app = app_router(auth);
    let listener = bind_with_retry(addr, &mut shutdown).await?;
    info!(%addr, "Web 管理界面已启动");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        while !*shutdown.borrow() {
            if shutdown.changed().await.is_err() {
                break;
            }
        }
    })
    .await
    .context("Web 管理界面异常退出")
}

fn app_router(auth: auth_http::AuthState) -> Router {
    let protected: Router<auth_http::AuthState> = Router::new()
        .route("/api/status", get(api_status))
        .route("/api/proxy/speedtest", post(api_proxy_speedtest))
        .route("/api/mihomo/start", post(api_mihomo_start))
        .route("/api/mihomo/stop", post(api_mihomo_stop))
        .route("/api/subscription/import", post(api_subscription_import))
        .route("/api/subscription/upload", post(api_subscription_upload))
        .route("/api/subscription/config", get(api_subscription_config))
        .route("/api/subscription/refresh", post(api_subscription_refresh))
        .route("/api/easytier/start", post(api_easytier_start))
        .route("/api/easytier/stop", post(api_easytier_stop))
        .route("/api/easytier/status", get(api_easytier_status))
        .route("/api/easytier/preview", get(api_easytier_preview))
        .route(
            "/api/easytier/secrets",
            get(api_easytier_secrets_get).post(api_easytier_secrets_set),
        )
        .route("/api/diag/splitflow", post(api_splitflow))
        .route_layer(middleware::from_fn_with_state(
            auth.clone(),
            auth_http::require_auth,
        ));

    Router::new()
        .route("/", get(index))
        .route("/assets/app.js", get(app_javascript))
        .route("/health", get(api_health))
        .route("/api/ping", get(api_ping))
        .merge(auth_http::auth_routes(auth.clone()))
        .merge(protected)
        .layer(middleware::from_fn(auth_http::security_headers))
        .with_state(auth)
}

async fn bind_with_retry(
    addr: SocketAddr,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<TcpListener> {
    loop {
        match TcpListener::bind(addr).await {
            Ok(listener) => return Ok(listener),
            Err(err) => {
                warn!(%addr, error = %err, "绑定 Web 管理端口失败，等待重试");
                tokio::select! {
                    _ = time::sleep(BIND_RETRY_DELAY) => {}
                    changed = shutdown.changed() => {
                        if changed.is_err() || *shutdown.borrow() {
                            anyhow::bail!("Web 管理界面在绑定端口前收到退出信号");
                        }
                    }
                }
            }
        }
    }
}

async fn api_status() -> Json<status::SystemStatus> {
    Json(status::collect().await)
}

async fn api_health() -> Json<HealthResponse> {
    Json(health_response())
}

fn health_response() -> HealthResponse {
    HealthResponse {
        version: env!("F50MGR_VERSION"),
        git_commit: env!("F50MGR_GIT_COMMIT"),
        schema_version: env!("F50MGR_SCHEMA_VERSION"),
    }
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    version: &'static str,
    git_commit: &'static str,
    schema_version: &'static str,
}

/// 触发一次代理出口实测速（F50 经 mihomo HTTP 14191 下载测速文件，统计带宽）
async fn api_proxy_speedtest() -> Json<SpeedtestResponse> {
    let result = task::spawn_blocking(run_proxy_speedtest)
        .await
        .context("代理测速任务失败")
        .and_then(|inner| inner);
    match result {
        Ok(s) => Json(s),
        Err(err) => Json(SpeedtestResponse {
            ok: false,
            bytes: 0,
            elapsed_ms: 0,
            mbps: 0.0,
            target: SPEEDTEST_TARGET,
            error: Some(err.to_string()),
        }),
    }
}

const SPEEDTEST_TARGET: &str = "http://speed.cloudflare.com/__down?bytes=10000000";
const SPEEDTEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const MIHOMO_HTTP_PROXY: &str = "http://192.168.0.1:14191";

fn run_proxy_speedtest() -> Result<SpeedtestResponse> {
    use std::io::Read as _;
    let proxy = ureq::Proxy::new(MIHOMO_HTTP_PROXY).context("构造 mihomo HTTP 代理失败")?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(Some(proxy))
        .timeout_global(Some(SPEEDTEST_TIMEOUT))
        .build()
        .into();
    let started = std::time::Instant::now();
    let mut response = agent
        .get(SPEEDTEST_TARGET)
        .call()
        .with_context(|| format!("代理测速请求失败: {SPEEDTEST_TARGET}"))?;
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .reader();
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => total += n as u64,
            Err(e) => return Err(anyhow::anyhow!("读取测速数据失败: {e}")),
        }
    }
    let elapsed = started.elapsed();
    let mbps = if elapsed.as_secs_f64() > 0.0 {
        (total as f64 * 8.0) / 1024.0 / 1024.0 / elapsed.as_secs_f64()
    } else {
        0.0
    };
    Ok(SpeedtestResponse {
        ok: true,
        bytes: total,
        elapsed_ms: elapsed.as_millis() as u64,
        mbps: (mbps * 100.0).round() / 100.0,
        target: SPEEDTEST_TARGET,
        error: None,
    })
}

#[derive(Debug, Serialize)]
struct SpeedtestResponse {
    ok: bool,
    bytes: u64,
    elapsed_ms: u64,
    mbps: f64,
    target: &'static str,
    error: Option<String>,
}

/// 轻量端点用于客户端测真实 LAN RTT（不做任何采集，应答应在 1-2ms）
async fn api_ping() -> Json<PingResponse> {
    Json(PingResponse {
        ok: true,
        ts_unix_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
    })
}

#[derive(Debug, Serialize)]
struct PingResponse {
    ok: bool,
    ts_unix_ms: u64,
}

#[derive(Debug, Serialize)]
struct ActionResponse {
    ok: bool,
    message: String,
}

#[derive(Debug, Deserialize)]
struct SubscriptionImportRequest {
    url: String,
}

async fn api_mihomo_start() -> Json<ActionResponse> {
    action_response(
        process::mihomo::set_auto_start(true).map(|()| "已启用 Mihomo 开机启动并请求启动"),
    )
}

async fn api_mihomo_stop() -> Json<ActionResponse> {
    let result = process::mihomo::set_auto_start(false)
        .and_then(|()| process::mihomo::stop_running())
        .map(|()| "已停止 Mihomo，并关闭开机启动");
    action_response(result)
}

async fn api_subscription_import(
    Json(payload): Json<SubscriptionImportRequest>,
) -> Json<ActionResponse> {
    let url = payload.url.trim().to_owned();
    if url.is_empty() {
        return action_response(Err(anyhow::anyhow!("订阅 URL 不能为空")));
    }
    let result = task::spawn_blocking(move || {
        subscription::import_from_url(&url)?;
        reload_mihomo_after_import("订阅已导入，Mihomo 正在重新加载配置")
    })
    .await
    .context("订阅导入任务失败")
    .and_then(|result| result);

    action_response(result)
}

async fn api_subscription_upload(request: Request) -> Json<ActionResponse> {
    action_response(import_uploaded_subscription(request).await)
}

async fn import_uploaded_subscription(request: Request) -> Result<&'static str> {
    let bytes = to_bytes(request.into_body(), subscription::MAX_IMPORT_BYTES)
        .await
        .map_err(|err| anyhow::anyhow!("读取订阅文件失败（文件不能超过 2MB）: {err}"))?;
    if bytes.is_empty() {
        bail!("请选择 YAML 订阅文件");
    }
    task::spawn_blocking(move || {
        let subscription =
            String::from_utf8(bytes.to_vec()).context("订阅 YAML 必须是 UTF-8 文本")?;
        subscription::import_from_yaml(&subscription)?;
        reload_mihomo_after_import("订阅文件已导入，Mihomo 正在重新加载配置")
    })
    .await
    .context("订阅文件导入任务失败")?
}

#[derive(Debug, Serialize)]
struct SubscriptionConfigResponse {
    configured: bool,
    display: Option<String>,
}

async fn api_subscription_config() -> Json<SubscriptionConfigResponse> {
    let url = task::spawn_blocking(subscription::read_subscription_url)
        .await
        .context("读取订阅配置任务失败")
        .and_then(|inner| inner)
        .unwrap_or(None);
    Json(subscription_config_response(url))
}

fn subscription_config_response(url: Option<String>) -> SubscriptionConfigResponse {
    let display = url.as_deref().map(mask_subscription_url);
    SubscriptionConfigResponse {
        configured: display.is_some(),
        display,
    }
}

fn mask_subscription_url(url: &str) -> String {
    let without_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let host = without_scheme
        .split(['/', '?', '#'])
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("已配置");
    let suffix = url.chars().rev().take(4).collect::<String>();
    let suffix = suffix.chars().rev().collect::<String>();
    format!("{host}/…{suffix}")
}

async fn api_subscription_refresh() -> Json<ActionResponse> {
    let result = task::spawn_blocking(|| {
        let url = subscription::read_subscription_url()
            .context("读取订阅 URL 失败")?
            .context("尚未保存订阅 URL，请先通过「导入 URL」完成首次导入")?;
        subscription::import_from_url(&url)?;
        reload_mihomo_after_import("订阅已刷新，Mihomo 正在重新加载配置")
    })
    .await
    .context("订阅刷新任务失败")
    .and_then(|result| result);
    action_response(result)
}

fn reload_mihomo_after_import(message: &'static str) -> Result<&'static str> {
    process::mihomo::set_auto_start(true).context("订阅已导入，但启用 Mihomo 开机启动失败")?;
    process::mihomo::stop_running().context("订阅已导入，但重启 Mihomo 失败")?;
    Ok(message)
}

async fn api_easytier_start() -> Json<ActionResponse> {
    action_response(
        process::easytier::set_auto_start(true).map(|()| "已启用 EasyTier 开机启动并请求启动"),
    )
}

async fn api_easytier_stop() -> Json<ActionResponse> {
    let result = process::easytier::set_auto_start(false)
        .and_then(|()| process::easytier::stop_running())
        .map(|()| "已停止 EasyTier，并关闭开机启动");
    action_response(result)
}

async fn api_easytier_status() -> Json<process::easytier::NodeStatus> {
    Json(process::easytier::node_status().await)
}

async fn api_easytier_preview() -> Json<EasyTierPreviewResponse> {
    // 用脱敏版：14180 对 LAN 无认证开放，不得回显 network_secret
    let result = task::spawn_blocking(subscription::easytier::preview_redacted)
        .await
        .context("EasyTier 配置预览任务失败")
        .and_then(|inner| inner);
    match result {
        Ok(content) => Json(EasyTierPreviewResponse {
            ok: true,
            config: Some(content),
            error: None,
        }),
        Err(err) => Json(EasyTierPreviewResponse {
            ok: false,
            config: None,
            error: Some(err.to_string()),
        }),
    }
}

#[derive(Debug, Serialize)]
struct EasyTierPreviewResponse {
    ok: bool,
    config: Option<String>,
    error: Option<String>,
}

async fn api_easytier_secrets_get() -> Json<EasyTierSecretsGetResponse> {
    let result = task::spawn_blocking(subscription::easytier::read_secrets_safe)
        .await
        .context("EasyTier secrets 读取任务失败")
        .and_then(|inner| inner);
    match result {
        Ok(view) => Json(EasyTierSecretsGetResponse {
            ok: true,
            configured: view.is_some(),
            secrets: view,
            error: None,
        }),
        Err(err) => Json(EasyTierSecretsGetResponse {
            ok: false,
            configured: false,
            secrets: None,
            error: Some(err.to_string()),
        }),
    }
}

async fn api_easytier_secrets_set(
    Json(payload): Json<subscription::easytier::SecretsInput>,
) -> Json<ActionResponse> {
    let result = task::spawn_blocking(move || subscription::easytier::write_secrets(payload))
        .await
        .context("EasyTier secrets 写入任务失败")
        .and_then(|inner| inner)
        .map(|()| "EasyTier 配置已保存");
    action_response(result)
}

#[derive(Debug, Serialize)]
struct EasyTierSecretsGetResponse {
    ok: bool,
    configured: bool,
    secrets: Option<subscription::easytier::SecretsSafeView>,
    error: Option<String>,
}

fn action_response(result: Result<&'static str>) -> Json<ActionResponse> {
    match result {
        Ok(message) => Json(ActionResponse {
            ok: true,
            message: message.to_owned(),
        }),
        Err(err) => Json(ActionResponse {
            ok: false,
            message: err.to_string(),
        }),
    }
}

/// ip-api.com 单次返回 IP + geo，纯 HTTP 避免 F50 TLS 问题
const SPLITFLOW_IP_GEO_API: &str =
    "http://ip-api.com/json/?fields=query,country,city,isp&lang=zh-CN";
const SPLITFLOW_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

#[derive(Debug, Serialize)]
struct SplitFlowResponse {
    ok: bool,
    direct: Option<IpProbeResult>,
    proxy: Option<IpProbeResult>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct IpProbeResult {
    ip: String,
    geo: String,
}

async fn api_splitflow() -> Json<SplitFlowResponse> {
    let result = task::spawn_blocking(run_splitflow_probe)
        .await
        .context("分流探测任务失败")
        .and_then(|inner| inner);
    match result {
        Ok((direct, proxy)) => Json(SplitFlowResponse {
            ok: true,
            direct: Some(direct),
            proxy: Some(proxy),
            error: None,
        }),
        Err(err) => Json(SplitFlowResponse {
            ok: false,
            direct: None,
            proxy: None,
            error: Some(err.to_string()),
        }),
    }
}

fn run_splitflow_probe() -> Result<(IpProbeResult, IpProbeResult)> {
    let direct_agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(SPLITFLOW_TIMEOUT))
        .build()
        .into();
    let direct = probe_ip_with_geo(&direct_agent).context("直连出口 IP 探测失败")?;

    let proxy = ureq::Proxy::new(MIHOMO_HTTP_PROXY).context("构造 Mihomo HTTP 代理失败")?;
    let proxy_agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(Some(proxy))
        .timeout_global(Some(SPLITFLOW_TIMEOUT))
        .build()
        .into();
    let proxied = probe_ip_with_geo(&proxy_agent).context("代理出口 IP 探测失败")?;

    Ok((direct, proxied))
}

fn probe_ip_with_geo(agent: &ureq::Agent) -> Result<IpProbeResult> {
    let mut resp = agent
        .get(SPLITFLOW_IP_GEO_API)
        .call()
        .context("出口 IP 探测请求失败")?;
    let body = resp
        .body_mut()
        .with_config()
        .limit(512)
        .read_to_string()
        .context("读取出口 IP 响应失败")?;
    let json: serde_json::Value = serde_json::from_str(&body).context("解析出口 IP JSON 失败")?;
    let ip = json
        .get("query")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .context("响应缺少 query 字段")?;
    let geo = [
        json.get("country").and_then(serde_json::Value::as_str),
        json.get("city").and_then(serde_json::Value::as_str),
        json.get("isp").and_then(serde_json::Value::as_str),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" · ");
    Ok(IpProbeResult { ip, geo })
}

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn app_javascript() -> ([(axum::http::HeaderName, &'static str); 1], &'static str) {
    ([
        (
            axum::http::header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        ),
    ], APP_JS)
}

const INDEX_HTML: &str = include_str!(concat!(env!("OUT_DIR"), "/index.html"));
const APP_JS: &str = include_str!(concat!(env!("OUT_DIR"), "/app.js"));

#[cfg(test)]
mod web_asset_tests {
    use super::{app_router, auth_http::AuthState, health_response, APP_JS, INDEX_HTML};
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    fn inline_script(html: &str) -> &str {
        html.split_once("<script>")
            .and_then(|(_, rest)| rest.split_once("</script>"))
            .map(|(script, _)| script)
            .expect("页面应包含内联脚本")
    }

    #[test]
    fn release_page_uses_five_offline_sparkline_components() {
        assert_eq!(INDEX_HTML.matches("data-sparkline").count(), 5);
        assert!(INDEX_HTML.contains("<script src=\"/assets/app.js\" defer></script>"));
        assert!(!INDEX_HTML.contains("<script>"));
        assert!(!INDEX_HTML.contains("<link rel=\"stylesheet\""));
        assert!(!APP_JS.contains("import("));
    }

    #[test]
    fn release_page_uses_theme_aware_chart_colors() {
        assert!(INDEX_HTML.contains("--chart-line:"));
        assert!(INDEX_HTML.contains("--chart-average:"));
        assert!(!APP_JS.contains("#1b1f24"));
        assert!(!APP_JS.contains("#b0b7c0"));
    }

    #[test]
    fn release_page_contains_easytier_peer_table() {
        assert!(INDEX_HTML.contains("id=\"easytierPeers\""));
        for heading in [
            "IPv4 地址",
            "主机名",
            "路由",
            "延迟",
            "丢包率",
            "下载",
            "上传",
            "协议",
            "NAT 类型",
            "内核版本",
        ] {
            assert!(
                INDEX_HTML.contains(heading),
                "缺少 EasyTier 表头: {heading}"
            );
        }
    }

    #[test]
    fn release_page_javascript_is_minified() {
        let source = include_str!("index.html");
        let source_script = inline_script(source);
        assert!(APP_JS.lines().count() <= 2);
        assert!(!APP_JS.lines().any(|line| line.starts_with("  ")));
        assert!(APP_JS.len() * 5 <= source_script.len() * 4);
    }

    #[test]
    fn release_page_has_local_login_gate_and_csrf_client() {
        assert!(INDEX_HTML.contains("id=\"loginGate\""));
        assert!(APP_JS.contains("/api/auth/session"));
        assert!(APP_JS.contains("/api/auth/login"));
        assert!(APP_JS.contains("X-F50-CSRF"));
    }

    #[test]
    fn release_page_exposes_mihomo_update_state_without_credentials() {
        assert!(APP_JS.contains("mihomo_update"));
        assert!(INDEX_HTML.contains("自动更新"));
        assert!(!APP_JS.contains("company_base_url"));
    }

    #[test]
    fn release_page_has_no_device_reboot_control() {
        assert!(!INDEX_HTML.contains("rebootDevice"));
        assert!(!INDEX_HTML.contains("/api/reboot"));
    }

    #[test]
    fn source_page_uploads_yaml_as_size_checked_raw_text() {
        let source = include_str!("index.html");
        assert!(source.contains("file.size>2*1024*1024"));
        assert!(source.contains("await file.text()"));
        assert!(source.contains("'Content-Type':'application/yaml; charset=utf-8'"));
        assert!(!source.contains("new FormData()"));
    }

    #[test]
    fn health_contract_exposes_release_identity() {
        let health = health_response();
        assert_eq!(health.version, env!("F50MGR_VERSION"));
        assert!(!health.git_commit.is_empty());
        assert_eq!(health.schema_version, "1");
    }

    #[tokio::test]
    async fn all_management_routes_reject_unauthenticated_requests() {
        let auth =
            AuthState::new("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
                .unwrap();
        let app = app_router(auth);
        let routes = [
            ("GET", "/api/status"),
            ("POST", "/api/proxy/speedtest"),
            ("POST", "/api/mihomo/start"),
            ("POST", "/api/mihomo/stop"),
            ("POST", "/api/subscription/import"),
            ("POST", "/api/subscription/upload"),
            ("GET", "/api/subscription/config"),
            ("POST", "/api/subscription/refresh"),
            ("POST", "/api/easytier/start"),
            ("POST", "/api/easytier/stop"),
            ("GET", "/api/easytier/status"),
            ("GET", "/api/easytier/preview"),
            ("GET", "/api/easytier/secrets"),
            ("POST", "/api/easytier/secrets"),
            ("POST", "/api/diag/splitflow"),
        ];
        for (method, path) in routes {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), 401, "{method} {path}");
        }
    }

    #[test]
    fn subscription_config_response_never_exposes_full_url() {
        let response = super::subscription_config_response(Some(
            "https://example.com/subscription?token=canary-secret".to_owned(),
        ));
        let json = serde_json::to_value(response).unwrap();
        let encoded = json.to_string();
        assert_eq!(json["configured"], true);
        assert!(json["display"].as_str().unwrap().contains("example.com"));
        assert!(!encoded.contains("canary-secret"));
        assert!(json.get("url").is_none());
    }
}
