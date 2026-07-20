use std::{
    collections::BTreeMap,
    ffi::CString,
    fs,
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;
use tokio::time;

use crate::process;

const PROC_STAT: &str = "/proc/stat";
const PROC_MEMINFO: &str = "/proc/meminfo";
const PROC_NET_DEV: &str = "/proc/net/dev";
const STORAGE_PATH: &str = "/data";
const THERMAL_ROOT: &str = "/sys/class/thermal";
const THERMAL_ZONES: [(&str, &str); 4] = [
    ("soc-thmzone", "SoC"),
    ("board-thmzone", "板载"),
    ("nr-thmzone", "5G"),
    ("lte-thmzone", "4G"),
];
const SAMPLE_DELAY: Duration = Duration::from_millis(250);
const MIHOMO_API: &str = "http://192.168.0.1:14193";
const MIHOMO_API_TIMEOUT: Duration = Duration::from_secs(1);
const MIHOMO_HTTP_PROXY: &str = "http://192.168.0.1:14191";
const MIHOMO_HTTP_PROXY_ADDR: &str = "192.168.0.1:14191";
const MIHOMO_GROUPS: [&str; 4] = ["Proxies", "HKGroup", "AI", "Download"];
// 真机海外节点首包实测可达 5s；4 站点并行，6s 在避免误报的同时限制最坏等待。
const PROXY_CHECK_TIMEOUT: Duration = Duration::from_secs(6);
const PROXY_TEST_SITES: &[ProxyTestSite] = &[
    ProxyTestSite {
        name: "百度",
        region: "国内",
        url: "http://www.baidu.com/",
    },
    ProxyTestSite {
        name: "腾讯",
        region: "国内",
        url: "http://www.qq.com/",
    },
    ProxyTestSite {
        name: "Google",
        region: "国外",
        url: "http://www.gstatic.com/generate_204",
    },
    ProxyTestSite {
        name: "Cloudflare",
        region: "国外",
        url: "http://www.cloudflare.com/cdn-cgi/trace",
    },
];
const ZTE_GOFORM_API: &str = "http://192.168.0.1:8080";
const ZTE_GOFORM_TIMEOUT: Duration = Duration::from_secs(1);
const ZTE_BASE_CMDS: &str = "network_type,network_provider,wifi_access_sta_num,network_signalbar,rssi,cr_version,ipv6_wan_ipaddr,lan_ipaddr,mac_address,realtime_rx_thrpt,realtime_tx_thrpt,monthly_rx_bytes,monthly_tx_bytes,monthly_time,ppp_status";
const ZTE_RADIO_CMDS: &str = "network_information,Lte_ca_status,lte_rssi,nr_rssi,Z5g_rsrp,nr_rsrp,Nr_snr,nr_rsrq,Nr_bands,Nr_bands_widths,Nr_fcn,Nr_pci,Nr_cell_id,lte_rsrp,Lte_snr,lte_rsrq,Lte_bands,Lte_bands_widths,Lte_fcn,Lte_pci,Lte_cell_id";

#[derive(Debug, Serialize)]
pub struct SystemStatus {
    pub version: &'static str,
    pub daemon: ServiceStatus,
    pub mihomo: ServiceStatus,
    pub easytier: ServiceStatus,
    pub easytier_enabled: bool,
    pub cellular_monthly: Option<CellularMonthly>,
    pub server_processing_ms: u64,
    pub clients: Vec<ConnectedClient>,
    pub cpu: Option<CpuStatus>,
    pub temperatures: Vec<TemperatureReading>,
    pub memory: Option<MemoryStatus>,
    pub traffic: Option<TrafficStatus>,
    pub storage: Option<StorageStatus>,
    pub mihomo_api: MihomoApiStatus,
    pub mihomo_auto_start: bool,
    /// watchdog 重启计数（R4 可观测化）：判断 mihomo 是否在反复重启
    pub mihomo_watchdog: process::mihomo::WatchdogSnapshot,
    pub mihomo_update: crate::update::UpdateSnapshot,
    pub proxy_health: ProxyHealthStatus,
    pub network_diagnosis: NetworkDiagnosis,
    pub device: DeviceStatus,
}

#[derive(Debug, Serialize)]
pub struct ConnectedClient {
    pub ip: String,
    pub mac: String,
    pub hostname: Option<String>,
    /// `wifi`（无线接入）/ `lan`（USB/有线）
    pub link: &'static str,
}

#[derive(Debug, Serialize)]
pub struct CellularMonthly {
    pub period: String,
    /// 设备视角下行（4G 接收）
    pub rx_bytes: u64,
    /// 设备视角上行（4G 发送）
    pub tx_bytes: u64,
    pub source: &'static str,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CpuStatus {
    pub usage_percent: f64,
}

#[derive(Debug, Serialize)]
pub struct TemperatureReading {
    pub label: &'static str,
    pub celsius: f64,
}

#[derive(Debug, Serialize)]
pub struct MemoryStatus {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_percent: f64,
}

#[derive(Debug, Serialize)]
pub struct TrafficStatus {
    pub rx_bytes_per_sec: u64,
    pub tx_bytes_per_sec: u64,
    pub rx_total_bytes: u64,
    pub tx_total_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct StorageStatus {
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub used_percent: f64,
}

#[derive(Debug, Serialize)]
pub struct MihomoApiStatus {
    pub available: bool,
    pub controller: &'static str,
    pub version: Option<String>,
    pub mode: Option<String>,
    pub bind_address: Option<String>,
    pub mixed_port: Option<u16>,
    pub http_port: Option<u16>,
    pub socks_port: Option<u16>,
    pub redir_port: Option<u16>,
    pub tproxy_port: Option<u16>,
    pub connection_count: Option<usize>,
    pub upload_total_bytes: Option<u64>,
    pub download_total_bytes: Option<u64>,
    pub upload_bytes_per_sec: Option<u64>,
    pub download_bytes_per_sec: Option<u64>,
    pub memory_bytes: Option<u64>,
    pub groups: Vec<MihomoGroupStatus>,
    pub paths: Vec<MihomoPathStatus>,
    pub sources: Vec<MihomoSourceStatus>,
    pub rules: Vec<MihomoRuleStatus>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MihomoGroupStatus {
    pub name: String,
    pub selected: Option<String>,
    pub proxy_type: Option<String>,
    pub alive: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct MihomoPathStatus {
    pub source: String,
    pub host: String,
    pub chains: Vec<String>,
    pub upload_bytes: u64,
    pub download_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct MihomoSourceStatus {
    pub source: String,
    pub connection_count: usize,
    pub upload_bytes: u64,
    pub download_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct MihomoRuleStatus {
    pub name: String,
    pub target: String,
    pub hit_count: u64,
}

#[derive(Debug, Serialize)]
pub struct ProxyHealthStatus {
    pub available: bool,
    pub summary: String,
    pub domestic_ok: bool,
    pub foreign_ok: bool,
    pub sites: Vec<ProxySiteStatus>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProxySiteStatus {
    pub name: &'static str,
    pub region: &'static str,
    pub url: &'static str,
    pub ok: bool,
    pub latency_ms: Option<u64>,
    pub status: Option<u16>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct NetworkDiagnosis {
    pub code: &'static str,
    pub level: &'static str,
    pub summary: &'static str,
    pub action: &'static str,
    pub ppp_ok: Option<bool>,
    pub dns_ok: bool,
    pub direct_ok: bool,
    pub domestic_ok: bool,
    pub foreign_ok: bool,
    pub tproxy_active: bool,
    pub direct_latency_ms: Option<u64>,
    pub error: Option<String>,
}

struct DirectNetworkStatus {
    dns_ok: bool,
    internet_ok: bool,
    latency_ms: Option<u64>,
    error: Option<String>,
}
struct DirectProbeResult {
    dns_ok: bool,
    internet_ok: bool,
    latency_ms: Option<u64>,
    error: Option<String>,
}

#[derive(Debug)]
struct TrackingResolver {
    resolved: Arc<AtomicBool>,
}

impl ureq::unversioned::resolver::Resolver for TrackingResolver {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> std::result::Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let result = ureq::unversioned::resolver::Resolver::resolve(
            &ureq::unversioned::resolver::DefaultResolver::default(),
            uri,
            config,
            timeout,
        );
        if result.is_ok() {
            self.resolved.store(true, Ordering::Relaxed);
        }
        result
    }
}

#[derive(Debug, Serialize)]
pub struct DeviceStatus {
    pub available: bool,
    pub source: &'static str,
    pub network_type: Option<String>,
    pub network_provider: Option<String>,
    pub ppp_status: Option<String>,
    pub wifi_clients: Option<u64>,
    pub signal_bars: Option<i64>,
    pub rsrp: Option<i64>,
    pub sinr: Option<i64>,
    pub rsrq: Option<i64>,
    pub band: Option<String>,
    pub frequency: Option<String>,
    pub pci: Option<String>,
    pub cell_id: Option<String>,
    pub firmware: Option<String>,
    pub ipv6_wan_ip: Option<String>,
    pub lan_ip: Option<String>,
    pub mac_address: Option<String>,
    pub rx_bytes_per_sec: Option<u64>,
    pub tx_bytes_per_sec: Option<u64>,
    pub monthly_rx_bytes: Option<u64>,
    pub monthly_tx_bytes: Option<u64>,
    pub error: Option<String>,
}

#[derive(Clone, Copy)]
struct ProxyTestSite {
    name: &'static str,
    region: &'static str,
    url: &'static str,
}

#[derive(Clone, Copy)]
struct CpuSample {
    idle: u64,
    total: u64,
}

#[derive(Clone, Copy)]
struct TrafficSample {
    rx: u64,
    tx: u64,
}

pub async fn collect() -> SystemStatus {
    let started = std::time::Instant::now();
    let cpu_before = read_cpu_sample(PROC_STAT).ok();
    let traffic_before = read_traffic_sample(PROC_NET_DEV).ok();
    let mihomo_api = tokio::task::spawn_blocking(collect_mihomo_api);
    let proxy_health = tokio::task::spawn_blocking(collect_proxy_health);
    let direct_network = tokio::task::spawn_blocking(collect_direct_network);
    let device = tokio::task::spawn_blocking(collect_device_status);
    let cellular_monthly = tokio::task::spawn_blocking(cellular_monthly_status);
    // collect_clients 内部是同步 ureq HTTP（ZTE goform ×2，最坏 ~2s），
    // 必须与其他采集一样隔离到阻塞线程，否则每次 /api/status 轮询都占死一个 worker。
    let clients = tokio::task::spawn_blocking(collect_clients);
    time::sleep(SAMPLE_DELAY).await;

    let mihomo_api = mihomo_api
        .await
        .unwrap_or_else(|err| mihomo_api_error(format!("读取 Mihomo API 任务失败: {err}")));
    let proxy_health = proxy_health
        .await
        .unwrap_or_else(|err| proxy_health_error(format!("代理健康检查任务失败: {err}")));
    let direct_network = direct_network
        .await
        .unwrap_or_else(|err| DirectNetworkStatus {
            dns_ok: false,
            internet_ok: false,
            latency_ms: None,
            error: Some(format!("直连网络诊断任务失败: {err}")),
        });
    let device = device
        .await
        .unwrap_or_else(|err| device_error(format!("读取设备信息任务失败: {err}")));
    let tproxy_active = crate::iptables::rules::is_active().unwrap_or(false);
    let mut network_diagnosis = diagnose_network(
        device.ppp_status.as_deref(),
        direct_network.dns_ok,
        direct_network.internet_ok,
        proxy_health.domestic_ok,
        proxy_health.foreign_ok,
        tproxy_active,
    );
    network_diagnosis.direct_latency_ms = direct_network.latency_ms;
    network_diagnosis.error = direct_network.error;

    SystemStatus {
        version: env!("F50MGR_VERSION"),
        daemon: ServiceStatus {
            running: true,
            pid: Some(std::process::id()),
            error: None,
        },
        mihomo: mihomo_status(),
        easytier: easytier_status(),
        easytier_enabled: process::easytier::auto_start_enabled().unwrap_or(false),
        cellular_monthly: Some(cellular_monthly.await.unwrap_or_else(|err| CellularMonthly {
            period: String::new(),
            rx_bytes: 0,
            tx_bytes: 0,
            source: "/proc/net/dev sipa_eth0",
            error: Some(format!("月流量采集任务失败: {err}")),
        })),
        // collect 末尾再覆盖（包含本次采集的全部耗时，扣掉客户端→服务端 LAN 段
        // 即可推算 LAN RTT；用于诊断"客户端慢 vs 服务端慢"）
        server_processing_ms: started.elapsed().as_millis() as u64,
        clients: clients
            .await
            .map(|result| result.unwrap_or_default())
            .unwrap_or_default(),
        cpu: cpu_before
            .zip(read_cpu_sample(PROC_STAT).ok())
            .and_then(|(before, after)| cpu_status(before, after)),
        temperatures: collect_temperatures(THERMAL_ROOT),
        memory: read_memory_status(PROC_MEMINFO).ok(),
        traffic: traffic_before
            .zip(read_traffic_sample(PROC_NET_DEV).ok())
            .map(|(before, after)| traffic_status(before, after, SAMPLE_DELAY)),
        storage: storage_status(STORAGE_PATH).ok(),
        mihomo_api,
        mihomo_auto_start: process::mihomo::auto_start_enabled().unwrap_or(true),
        mihomo_watchdog: process::mihomo::watchdog_snapshot(),
        mihomo_update: crate::update::update_snapshot(),
        proxy_health,
        network_diagnosis,
        device,
    }
    .finalize_processing_time(started)
}

impl SystemStatus {
    fn finalize_processing_time(mut self, started: std::time::Instant) -> Self {
        self.server_processing_ms = started.elapsed().as_millis() as u64;
        self
    }
}

fn cellular_monthly_status() -> CellularMonthly {
    match crate::traffic::sample() {
        Ok(s) => CellularMonthly {
            period: s.period,
            rx_bytes: s.rx_bytes,
            tx_bytes: s.tx_bytes,
            source: "/proc/net/dev sipa_eth0",
            error: None,
        },
        Err(err) => CellularMonthly {
            period: String::new(),
            rx_bytes: 0,
            tx_bytes: 0,
            source: "/proc/net/dev sipa_eth0",
            error: Some(err.to_string()),
        },
    }
}

fn easytier_status() -> ServiceStatus {
    match process::easytier::status() {
        Ok(status) => ServiceStatus {
            running: status.running,
            pid: status.pid,
            error: None,
        },
        Err(err) => ServiceStatus {
            running: false,
            pid: None,
            error: Some(err.to_string()),
        },
    }
}

fn mihomo_status() -> ServiceStatus {
    match process::mihomo::status() {
        Ok(status) => ServiceStatus {
            running: status.running,
            pid: status.pid,
            error: None,
        },
        Err(err) => ServiceStatus {
            running: false,
            pid: None,
            error: Some(err.to_string()),
        },
    }
}

fn collect_mihomo_api() -> MihomoApiStatus {
    match collect_mihomo_api_inner() {
        Ok(status) => status,
        Err(err) => mihomo_api_error(err.to_string()),
    }
}

fn collect_mihomo_api_inner() -> Result<MihomoApiStatus> {
    let agent = mihomo_agent();
    let version = mihomo_get_json(&agent, "/version")?;
    let configs = mihomo_get_json(&agent, "/configs")?;
    let connections_before = mihomo_get_json(&agent, "/connections")?;
    thread::sleep(SAMPLE_DELAY);
    let connections = mihomo_get_json(&agent, "/connections")?;
    let rules = mihomo_get_json(&agent, "/rules").unwrap_or(Value::Null);
    let mut groups = Vec::with_capacity(MIHOMO_GROUPS.len());

    for group in MIHOMO_GROUPS {
        if let Ok(value) = mihomo_get_json(&agent, &format!("/proxies/{group}")) {
            groups.push(MihomoGroupStatus {
                name: group.to_owned(),
                selected: string_field(&value, "now"),
                proxy_type: string_field(&value, "type"),
                alive: value.get("alive").and_then(Value::as_bool),
            });
        }
    }

    let (upload_before, download_before) = connection_totals(&connections_before);
    let (upload_total, download_total) = connection_totals(&connections);

    Ok(MihomoApiStatus {
        available: true,
        controller: MIHOMO_API,
        version: string_field(&version, "version"),
        mode: string_field(&configs, "mode"),
        bind_address: string_field(&configs, "bind-address"),
        mixed_port: u16_field(&configs, "mixed-port"),
        http_port: u16_field(&configs, "port"),
        socks_port: u16_field(&configs, "socks-port"),
        redir_port: u16_field(&configs, "redir-port"),
        tproxy_port: u16_field(&configs, "tproxy-port"),
        connection_count: connection_count(&connections),
        upload_total_bytes: upload_total,
        download_total_bytes: download_total,
        upload_bytes_per_sec: upload_total
            .zip(upload_before)
            .map(|(after, before)| rate(after.saturating_sub(before), SAMPLE_DELAY.as_secs_f64())),
        download_bytes_per_sec: download_total
            .zip(download_before)
            .map(|(after, before)| rate(after.saturating_sub(before), SAMPLE_DELAY.as_secs_f64())),
        memory_bytes: connections.get("memory").and_then(Value::as_u64),
        groups,
        paths: connection_paths(&connections),
        sources: source_stats(&connections),
        rules: rule_stats(&rules),
        error: None,
    })
}

fn mihomo_api_error(error: String) -> MihomoApiStatus {
    MihomoApiStatus {
        available: false,
        controller: MIHOMO_API,
        version: None,
        mode: None,
        bind_address: None,
        mixed_port: None,
        http_port: None,
        socks_port: None,
        redir_port: None,
        tproxy_port: None,
        connection_count: None,
        upload_total_bytes: None,
        download_total_bytes: None,
        upload_bytes_per_sec: None,
        download_bytes_per_sec: None,
        memory_bytes: None,
        groups: Vec::new(),
        paths: Vec::new(),
        sources: Vec::new(),
        rules: Vec::new(),
        error: Some(error),
    }
}

fn mihomo_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(MIHOMO_API_TIMEOUT))
        .build();
    config.into()
}

fn mihomo_get_json(agent: &ureq::Agent, path: &str) -> Result<Value> {
    let secret = crate::web::auth_store::read_management_key()
        .context("读取 Mihomo Controller 管理密钥失败")?;
    let body = agent
        .get(&format!("{MIHOMO_API}{path}"))
        .header("Authorization", mihomo_authorization(&secret))
        .call()
        .with_context(|| format!("请求 Mihomo API 失败: {path}"))?
        .body_mut()
        .with_config()
        .limit(256 * 1024)
        .read_to_string()
        .with_context(|| format!("读取 Mihomo API 响应失败: {path}"))?;
    serde_json::from_str(&body).with_context(|| format!("解析 Mihomo API JSON 失败: {path}"))
}

fn mihomo_authorization(secret: &str) -> String {
    format!("Bearer {secret}")
}

fn collect_proxy_health() -> ProxyHealthStatus {
    match collect_proxy_health_inner() {
        Ok(status) => status,
        Err(err) => proxy_health_error(err.to_string()),
    }
}

fn collect_proxy_health_inner() -> Result<ProxyHealthStatus> {
    let mihomo = process::mihomo::status()?;
    if !mihomo.running {
        return Ok(proxy_health_error("Mihomo 未运行".to_owned()));
    }

    let probes = PROXY_TEST_SITES
        .iter()
        .map(|site| thread::spawn(move || probe_proxy_site(*site)))
        .collect::<Vec<_>>();
    let sites = probes
        .into_iter()
        .enumerate()
        .map(|(index, probe)| {
            probe.join().unwrap_or_else(|_| {
                proxy_site_error(PROXY_TEST_SITES[index], "代理探测线程异常".to_owned())
            })
        })
        .collect::<Vec<_>>();

    let self_proxy_probe_limited = sites.iter().all(|site| site.status == Some(502));
    let mihomo_groups_alive = mihomo_groups_alive();
    let (domestic_ok, foreign_ok, summary, error) =
        if self_proxy_probe_limited && mihomo_groups_alive {
            (
                true,
                true,
                "Mihomo 正常，自探测受限".to_owned(),
                Some("路由器本机 HTTP 代理自探测返回 502，已按 Mihomo 分组存活状态判断".to_owned()),
            )
        } else {
            let domestic_ok = region_ok(&sites, "国内");
            let foreign_ok = region_ok(&sites, "国外");
            let summary = match (domestic_ok, foreign_ok) {
                (true, true) => "国内外正常",
                (true, false) => "国内正常，国外异常",
                (false, true) => "国内异常，国外正常",
                (false, false) => "国内外异常",
            }
            .to_owned();
            (domestic_ok, foreign_ok, summary, None)
        };

    Ok(ProxyHealthStatus {
        available: domestic_ok || foreign_ok,
        summary,
        domestic_ok,
        foreign_ok,
        sites,
        error,
    })
}

fn proxy_health_error(error: String) -> ProxyHealthStatus {
    ProxyHealthStatus {
        available: false,
        summary: "不可用".to_owned(),
        domestic_ok: false,
        foreign_ok: false,
        sites: Vec::new(),
        error: Some(error),
    }
}

fn collect_direct_network() -> DirectNetworkStatus {
    let results = thread::scope(|scope| {
        crate::CONNECTIVITY_PROBE_URLS
            .iter()
            .copied()
            .map(|url| scope.spawn(move || probe_direct_network(url)))
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect::<Vec<_>>()
    });
    let dns_ok = results.iter().any(|result| result.dns_ok);
    if let Some(result) = results.iter().find(|result| result.internet_ok) {
        return DirectNetworkStatus {
            dns_ok: true,
            internet_ok: true,
            latency_ms: result.latency_ms,
            error: None,
        };
    }
    let errors = results
        .into_iter()
        .filter_map(|result| result.error)
        .collect::<Vec<_>>();

    DirectNetworkStatus {
        dns_ok,
        internet_ok: false,
        latency_ms: None,
        error: Some(if errors.is_empty() {
            "公网诊断线程异常退出".to_owned()
        } else {
            errors.join("; ")
        }),
    }
}

fn probe_direct_network(url: &'static str) -> DirectProbeResult {
    let resolved = Arc::new(AtomicBool::new(false));
    let agent = ureq::Agent::with_parts(
        crate::connectivity_probe_config(),
        ureq::unversioned::transport::DefaultConnector::default(),
        TrackingResolver {
            resolved: Arc::clone(&resolved),
        },
    );
    let started = Instant::now();
    match agent.get(url).call() {
        Ok(response) => DirectProbeResult {
            dns_ok: true,
            internet_ok: crate::connectivity_probe_succeeded(Some(response.status().as_u16())),
            latency_ms: Some(started.elapsed().as_millis() as u64),
            error: (!crate::connectivity_probe_succeeded(Some(response.status().as_u16())))
                .then(|| format!("{url} 返回 HTTP {}", response.status())),
        },
        Err(err) => DirectProbeResult {
            dns_ok: resolved.load(Ordering::Relaxed),
            internet_ok: false,
            latency_ms: None,
            error: Some(format!("{url}: {err}")),
        },
    }
}

fn diagnose_network(
    ppp_status: Option<&str>,
    dns_ok: bool,
    direct_ok: bool,
    domestic_ok: bool,
    foreign_ok: bool,
    tproxy_active: bool,
) -> NetworkDiagnosis {
    let ppp_ok = ppp_connected(ppp_status);
    let (code, level, summary, action) = if !direct_ok {
        if ppp_ok == Some(false) {
            (
                "cellular_disconnected",
                "bad",
                "蜂窝未联网",
                "检查 SIM、信号和 APN，重新拨号后再测试",
            )
        } else if !dns_ok {
            (
                "dns_unavailable",
                "bad",
                "DNS 解析失败",
                "检查蜂窝拨号和 DNS 配置",
            )
        } else {
            (
                "carrier_restricted",
                "bad",
                "运营商公网受限",
                "DNS 正常但公网不可达，请检查欠费、停机、套餐或运营商风控",
            )
        }
    } else if !domestic_ok && !foreign_ok {
        (
            "proxy_unavailable",
            "bad",
            "代理出口不可用",
            "蜂窝直连正常，请检查 Mihomo 配置、订阅和代理节点",
        )
    } else if !domestic_ok || !foreign_ok {
        (
            "proxy_degraded",
            "warn",
            "代理链路部分异常",
            "查看国内外探测结果并切换异常分组的节点",
        )
    } else if !tproxy_active {
        (
            "tproxy_inactive",
            "warn",
            "公网正常，透明代理未启用",
            "等待 watchdog 自动恢复；若持续未启用，请检查 f50mgr 日志",
        )
    } else {
        (
            "healthy",
            "ok",
            "网络正常",
            "蜂窝、DNS、公网和代理链路均已通过",
        )
    };

    NetworkDiagnosis {
        code,
        level,
        summary,
        action,
        ppp_ok,
        dns_ok,
        direct_ok,
        domestic_ok,
        foreign_ok,
        tproxy_active,
        direct_latency_ms: None,
        error: None,
    }
}

fn ppp_connected(status: Option<&str>) -> Option<bool> {
    let status = status?.trim().to_ascii_lowercase();
    if status.is_empty() {
        return None;
    }
    match status.as_str() {
        "connected"
        | "ppp_connected"
        | "ipv4_connected"
        | "ipv6_connected"
        | "ipv4_ipv6_connected"
        | "online"
        | "up"
        | "1" => Some(true),
        "disconnected" | "ppp_disconnected" | "connecting" | "ppp_connecting" | "disconnecting"
        | "ppp_disconnecting" | "offline" | "failed" | "down" | "0" => Some(false),
        _ => None,
    }
}

fn region_ok(sites: &[ProxySiteStatus], region: &str) -> bool {
    sites.iter().any(|site| site.region == region && site.ok)
}

fn mihomo_groups_alive() -> bool {
    let agent = mihomo_agent();
    MIHOMO_GROUPS.iter().any(|group| {
        mihomo_get_json(&agent, &format!("/proxies/{group}"))
            .ok()
            .and_then(|value| value.get("alive").and_then(Value::as_bool))
            .unwrap_or(false)
    })
}

fn probe_proxy_site(site: ProxyTestSite) -> ProxySiteStatus {
    match proxy_http_probe(site.url) {
        Ok((status, latency_ms)) => ProxySiteStatus {
            name: site.name,
            region: site.region,
            url: site.url,
            ok: (200..400).contains(&status),
            latency_ms: Some(latency_ms),
            status: Some(status),
            error: None,
        },
        Err(err) => proxy_site_error(site, format!("{err:#}")),
    }
}

fn proxy_site_error(site: ProxyTestSite, error: String) -> ProxySiteStatus {
    ProxySiteStatus {
        name: site.name,
        region: site.region,
        url: site.url,
        ok: false,
        latency_ms: None,
        status: None,
        error: Some(error),
    }
}

fn proxy_http_probe(url: &str) -> Result<(u16, u64)> {
    let (host, _path) = parse_http_url(url)?;
    let started = Instant::now();

    let mut stream = TcpStream::connect(MIHOMO_HTTP_PROXY_ADDR)
        .with_context(|| format!("连接 Mihomo HTTP 代理失败: {MIHOMO_HTTP_PROXY}"))?;
    stream.set_read_timeout(Some(PROXY_CHECK_TIMEOUT))?;
    stream.set_write_timeout(Some(PROXY_CHECK_TIMEOUT))?;
    write!(
        stream,
        "GET {url} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: f50mgr/{}\r\nAccept: */*\r\nProxy-Connection: close\r\nConnection: close\r\n\r\n",
        env!("F50MGR_VERSION")
    )?;
    stream.flush()?;

    let mut status_line = String::new();
    BufReader::new(stream)
        .read_line(&mut status_line)
        .with_context(|| format!("读取 Mihomo 代理响应失败: {url}"))?;
    let status = parse_http_status(&status_line)?;
    Ok((status, started.elapsed().as_millis() as u64))
}

fn parse_http_url(url: &str) -> Result<(&str, &str)> {
    let rest = url
        .strip_prefix("http://")
        .context("代理健康检查只支持 http:// URL")?;
    let Some((host, path)) = rest.split_once('/') else {
        return Ok((rest, "/"));
    };
    Ok((host, &url[(url.len() - path.len() - 1)..]))
}

fn parse_http_status(status_line: &str) -> Result<u16> {
    let mut parts = status_line.split_whitespace();
    let version = parts.next().context("HTTP 响应缺少版本")?;
    if !version.starts_with("HTTP/") {
        bail!("HTTP 响应版本异常: {status_line:?}");
    }
    let status = parts
        .next()
        .context("HTTP 响应缺少状态码")?
        .parse::<u16>()
        .context("HTTP 状态码不是数字")?;
    Ok(status)
}

/// 从 ZTE goform 拉 station_list（WiFi 客户端）+ lan_station_list（USB/有线）合并。
fn collect_clients() -> Result<Vec<ConnectedClient>> {
    let agent = zte_goform_agent();
    let mut clients = Vec::new();
    if let Ok(v) = zte_goform_get_json(&agent, "station_list") {
        clients.extend(parse_station_array(&v, "station_list", "wifi"));
    }
    if let Ok(v) = zte_goform_get_json(&agent, "lan_station_list") {
        clients.extend(parse_station_array(&v, "lan_station_list", "lan"));
    }
    Ok(clients)
}

fn parse_station_array(value: &Value, field: &str, link: &'static str) -> Vec<ConnectedClient> {
    let Some(arr) = value.get(field).and_then(Value::as_array) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|entry| {
            let ip = string_field(entry, "ip_addr")?;
            let mac = string_field(entry, "mac_addr").unwrap_or_default();
            let hostname = string_field(entry, "hostname").filter(|s| !s.is_empty());
            Some(ConnectedClient {
                ip,
                mac,
                hostname,
                link,
            })
        })
        .collect()
}

fn collect_device_status() -> DeviceStatus {
    match collect_device_status_inner() {
        Ok(status) => status,
        Err(err) => device_error(err.to_string()),
    }
}

fn collect_device_status_inner() -> Result<DeviceStatus> {
    let agent = zte_goform_agent();
    let mut value = zte_goform_get_json(&agent, ZTE_BASE_CMDS)?;
    if let Ok(radio) = zte_goform_get_json(&agent, ZTE_RADIO_CMDS) {
        merge_json_objects(&mut value, radio);
    }
    Ok(DeviceStatus {
        available: true,
        source: ZTE_GOFORM_API,
        network_type: value_string(&value, "network_type"),
        network_provider: value_string(&value, "network_provider"),
        ppp_status: value_string(&value, "ppp_status"),
        wifi_clients: u64_field(&value, "wifi_access_sta_num"),
        signal_bars: i64_field(&value, "rssi").or_else(|| i64_field(&value, "network_signalbar")),
        rsrp: i64_field(&value, "Z5g_rsrp")
            .or_else(|| i64_field(&value, "nr_rsrp"))
            .or_else(|| i64_field(&value, "lte_rsrp")),
        sinr: i64_field(&value, "Nr_snr").or_else(|| i64_field(&value, "Lte_snr")),
        rsrq: i64_field(&value, "nr_rsrq").or_else(|| i64_field(&value, "lte_rsrq")),
        band: first_value_string(&value, &["Nr_bands", "Lte_bands"]),
        frequency: first_value_string(&value, &["Nr_fcn", "Lte_fcn"]),
        pci: first_value_string(&value, &["Nr_pci", "Lte_pci"]),
        cell_id: first_value_string(&value, &["Nr_cell_id", "Lte_cell_id"]),
        firmware: value_string(&value, "cr_version"),
        ipv6_wan_ip: value_string(&value, "ipv6_wan_ipaddr"),
        lan_ip: value_string(&value, "lan_ipaddr"),
        mac_address: value_string(&value, "mac_address"),
        rx_bytes_per_sec: u64_field(&value, "realtime_rx_thrpt"),
        tx_bytes_per_sec: u64_field(&value, "realtime_tx_thrpt"),
        // ZTE F50 goform 命名 quirk：realtime_rx/tx 按设备视角（设备 rx = 用户下载），
        // 但 monthly_rx/tx 按基站视角（基站 rx = 用户上传）。这里翻转字段映射使
        // monthly_rx_bytes 始终表示"用户下载"，与 realtime 字段视角统一。
        // 实测验证 (2026-05-14)：sipa_eth0 设备视角 rx > tx，goform realtime rx > tx，
        // 但 goform monthly_tx (142GB) >> monthly_rx (25GB)，说明字段反置。
        monthly_rx_bytes: u64_field(&value, "monthly_tx_bytes"),
        monthly_tx_bytes: u64_field(&value, "monthly_rx_bytes"),
        error: None,
    })
}

fn merge_json_objects(target: &mut Value, source: Value) {
    let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) else {
        return;
    };
    for (key, value) in source {
        if !value.is_null() {
            target.insert(key.clone(), value.clone());
        }
    }
}

fn device_error(error: String) -> DeviceStatus {
    DeviceStatus {
        available: false,
        source: ZTE_GOFORM_API,
        network_type: None,
        network_provider: None,
        ppp_status: None,
        wifi_clients: None,
        signal_bars: None,
        rsrp: None,
        sinr: None,
        rsrq: None,
        band: None,
        frequency: None,
        pci: None,
        cell_id: None,
        firmware: None,
        ipv6_wan_ip: None,
        lan_ip: None,
        mac_address: None,
        rx_bytes_per_sec: None,
        tx_bytes_per_sec: None,
        monthly_rx_bytes: None,
        monthly_tx_bytes: None,
        error: Some(error),
    }
}

fn zte_goform_agent() -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(ZTE_GOFORM_TIMEOUT))
        .build();
    config.into()
}

fn zte_goform_get_json(agent: &ureq::Agent, commands: &str) -> Result<Value> {
    let url = format!(
        "{ZTE_GOFORM_API}/goform/goform_get_cmd_process?multi_data=1&isTest=false&cmd={commands}"
    );
    let body = agent
        .get(&url)
        .header("Referer", "http://192.168.0.1/index.html")
        .header("Origin", "http://192.168.0.1")
        .call()
        .context("请求 ZTE goform 状态失败")?
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()
        .context("读取 ZTE goform 响应失败")?;
    serde_json::from_str(&body).context("解析 ZTE goform JSON 失败")
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn value_string(value: &Value, key: &str) -> Option<String> {
    match value.get(key)? {
        Value::String(text) if !text.is_empty() => Some(text.to_owned()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn first_value_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| value_string(value, key))
}

fn i64_field(value: &Value, key: &str) -> Option<i64> {
    match value.get(key)? {
        Value::Number(number) => number.as_i64(),
        Value::String(text) if !text.is_empty() => text.parse::<i64>().ok(),
        _ => None,
    }
}

fn u64_field(value: &Value, key: &str) -> Option<u64> {
    match value.get(key)? {
        Value::Number(number) => number.as_u64(),
        Value::String(text) if !text.is_empty() => text.parse::<u64>().ok(),
        _ => None,
    }
}

fn u16_field(value: &Value, key: &str) -> Option<u16> {
    value.get(key)?.as_u64()?.try_into().ok()
}

fn connection_count(value: &Value) -> Option<usize> {
    value
        .get("connections")
        .and_then(Value::as_array)
        .map(Vec::len)
}

fn connection_totals(value: &Value) -> (Option<u64>, Option<u64>) {
    (
        value.get("uploadTotal").and_then(Value::as_u64),
        value.get("downloadTotal").and_then(Value::as_u64),
    )
}

fn connection_paths(value: &Value) -> Vec<MihomoPathStatus> {
    let Some(connections) = value.get("connections").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut paths = connections
        .iter()
        .map(|connection| {
            let metadata = connection.get("metadata").unwrap_or(&Value::Null);
            MihomoPathStatus {
                source: source_name(metadata),
                host: string_field(metadata, "host")
                    .or_else(|| string_field(metadata, "remoteDestination"))
                    .unwrap_or_else(|| "--".to_owned()),
                chains: string_array(connection, "chains"),
                upload_bytes: connection
                    .get("upload")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
                download_bytes: connection
                    .get("download")
                    .and_then(Value::as_u64)
                    .unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();
    paths.sort_by_key(|path| std::cmp::Reverse(path.upload_bytes + path.download_bytes));
    paths.truncate(5);
    paths
}

fn source_stats(value: &Value) -> Vec<MihomoSourceStatus> {
    let Some(connections) = value.get("connections").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut stats = BTreeMap::<String, MihomoSourceStatus>::new();
    for connection in connections {
        let metadata = connection.get("metadata").unwrap_or(&Value::Null);
        let source = source_name(metadata);
        let entry = stats.entry(source.clone()).or_insert(MihomoSourceStatus {
            source,
            connection_count: 0,
            upload_bytes: 0,
            download_bytes: 0,
        });
        entry.connection_count += 1;
        entry.upload_bytes += connection
            .get("upload")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        entry.download_bytes += connection
            .get("download")
            .and_then(Value::as_u64)
            .unwrap_or_default();
    }
    let mut values = stats.into_values().collect::<Vec<_>>();
    values.sort_by_key(|item| std::cmp::Reverse(item.upload_bytes + item.download_bytes));
    values.truncate(5);
    values
}

fn rule_stats(value: &Value) -> Vec<MihomoRuleStatus> {
    let Some(rules) = value.get("rules").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut stats = rules
        .iter()
        .filter_map(|rule| {
            let hit_count = rule
                .get("extra")
                .and_then(|extra| extra.get("hitCount"))
                .and_then(Value::as_u64)
                .unwrap_or_default();
            if hit_count == 0 {
                return None;
            }
            Some(MihomoRuleStatus {
                name: format!(
                    "{} {}",
                    string_field(rule, "type").unwrap_or_else(|| "Rule".to_owned()),
                    string_field(rule, "payload").unwrap_or_default()
                )
                .trim()
                .to_owned(),
                target: string_field(rule, "proxy").unwrap_or_else(|| "--".to_owned()),
                hit_count,
            })
        })
        .collect::<Vec<_>>();
    stats.sort_by_key(|rule| std::cmp::Reverse(rule.hit_count));
    stats.truncate(8);
    stats
}

fn source_name(metadata: &Value) -> String {
    string_field(metadata, "sourceIP")
        .filter(|value| !value.is_empty())
        .or_else(|| string_field(metadata, "type").filter(|value| !value.is_empty()))
        .or_else(|| string_field(metadata, "inboundName").filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn string_array(value: &Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn read_cpu_sample(path: &str) -> Result<CpuSample> {
    let content = fs::read_to_string(path).with_context(|| format!("读取 CPU 状态失败: {path}"))?;
    parse_cpu_sample(&content)
}

fn parse_cpu_sample(content: &str) -> Result<CpuSample> {
    let line = content.lines().next().context("缺少 CPU 汇总行")?;
    let fields = line
        .strip_prefix("cpu ")
        .context("CPU 汇总行格式无效")?
        .split_whitespace()
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if fields.len() < 5 {
        bail!("CPU 汇总字段不足");
    }
    let idle = fields[3] + fields[4];
    let total = fields.iter().sum();
    Ok(CpuSample { idle, total })
}

fn cpu_status(before: CpuSample, after: CpuSample) -> Option<CpuStatus> {
    let total = after.total.checked_sub(before.total)?;
    let idle = after.idle.checked_sub(before.idle)?;
    if total == 0 {
        return None;
    }
    Some(CpuStatus {
        usage_percent: rounded_percent(total - idle, total),
    })
}

fn read_memory_status(path: &str) -> Result<MemoryStatus> {
    let content = fs::read_to_string(path).with_context(|| format!("读取内存状态失败: {path}"))?;
    parse_memory_status(&content)
}

fn collect_temperatures(root: &str) -> Vec<TemperatureReading> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut readings = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(zone_type) = fs::read_to_string(path.join("type")) else {
            continue;
        };
        let Some((order, (_, label))) = THERMAL_ZONES
            .iter()
            .enumerate()
            .find(|(_, (name, _))| *name == zone_type.trim())
        else {
            continue;
        };
        let Some(celsius) = fs::read_to_string(path.join("temp"))
            .ok()
            .and_then(|raw| temperature_celsius(&raw))
        else {
            continue;
        };
        readings.push((order, TemperatureReading { label, celsius }));
    }
    readings.sort_by_key(|(order, _)| *order);
    readings.into_iter().map(|(_, reading)| reading).collect()
}

fn temperature_celsius(raw: &str) -> Option<f64> {
    let millidegrees = raw.trim().parse::<f64>().ok()?;
    (-40_000.0..=150_000.0)
        .contains(&millidegrees)
        .then(|| (millidegrees / 100.0).round() / 10.0)
}

fn parse_memory_status(content: &str) -> Result<MemoryStatus> {
    let mut total = None;
    let mut available = None;
    for line in content.lines() {
        if let Some(value) = line.strip_prefix("MemTotal:") {
            total = Some(parse_kib(value)?);
        } else if let Some(value) = line.strip_prefix("MemAvailable:") {
            available = Some(parse_kib(value)?);
        }
    }

    let total = total.context("缺少 MemTotal")?;
    let available = available.context("缺少 MemAvailable")?;
    Ok(MemoryStatus {
        total_bytes: total,
        available_bytes: available,
        used_percent: rounded_percent(total.saturating_sub(available), total),
    })
}

fn parse_kib(value: &str) -> Result<u64> {
    Ok(value
        .split_whitespace()
        .next()
        .context("缺少 KiB 数值")?
        .parse::<u64>()?
        * 1024)
}

fn read_traffic_sample(path: &str) -> Result<TrafficSample> {
    let content = fs::read_to_string(path).with_context(|| format!("读取流量状态失败: {path}"))?;
    parse_traffic_sample(&content)
}

fn parse_traffic_sample(content: &str) -> Result<TrafficSample> {
    let mut sample = TrafficSample { rx: 0, tx: 0 };
    for line in content.lines().skip(2) {
        let Some((name, values)) = line.split_once(':') else {
            continue;
        };
        if name.trim() == "lo" {
            continue;
        }
        let fields = values.split_whitespace().collect::<Vec<_>>();
        if fields.len() < 16 {
            continue;
        }
        sample.rx += fields[0].parse::<u64>()?;
        sample.tx += fields[8].parse::<u64>()?;
    }
    Ok(sample)
}

fn traffic_status(before: TrafficSample, after: TrafficSample, elapsed: Duration) -> TrafficStatus {
    let seconds = elapsed.as_secs_f64();
    TrafficStatus {
        rx_bytes_per_sec: rate(after.rx.saturating_sub(before.rx), seconds),
        tx_bytes_per_sec: rate(after.tx.saturating_sub(before.tx), seconds),
        rx_total_bytes: after.rx,
        tx_total_bytes: after.tx,
    }
}

fn storage_status(path: &str) -> Result<StorageStatus> {
    let path = CString::new(path)?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // libc::statvfs 只写入提供的缓冲区；返回值检查后才读取。
    let result = unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("读取存储状态失败");
    }
    let stat = unsafe { stat.assume_init() };
    let block_size = stat_field_to_u64(stat.f_frsize);
    let total = stat_field_to_u64(stat.f_blocks);
    let available = stat_field_to_u64(stat.f_bavail);
    let total = total * block_size;
    let available = available * block_size;
    Ok(StorageStatus {
        total_bytes: total,
        available_bytes: available,
        used_percent: rounded_percent(total.saturating_sub(available), total),
    })
}

fn rounded_percent(value: u64, total: u64) -> f64 {
    if total == 0 {
        return 0.0;
    }
    (((value as f64 / total as f64) * 100.0) * 10.0).round() / 10.0
}

fn stat_field_to_u64<T: TryInto<u64>>(value: T) -> u64 {
    value.try_into().ok().unwrap_or(0)
}

fn rate(bytes: u64, seconds: f64) -> u64 {
    if seconds <= 0.0 {
        return 0;
    }
    (bytes as f64 / seconds).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_usage_from_proc_stat_samples() -> Result<()> {
        let before = parse_cpu_sample("cpu  100 0 100 800 0 0 0 0\n")?;
        let after = parse_cpu_sample("cpu  150 0 150 900 0 0 0 0\n")?;
        let status = cpu_status(before, after).context("缺少 CPU 状态")?;
        assert_eq!(status.usage_percent, 50.0);
        Ok(())
    }

    #[test]
    fn parses_memory_status_from_meminfo() -> Result<()> {
        let status = parse_memory_status("MemTotal: 1000 kB\nMemAvailable: 250 kB\n")?;
        assert_eq!(status.total_bytes, 1024000);
        assert_eq!(status.used_percent, 75.0);
        Ok(())
    }

    #[test]
    fn parses_traffic_without_loopback() -> Result<()> {
        let sample = parse_traffic_sample(
            "Inter-| Receive | Transmit\n face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier compressed\n  lo: 10 0 0 0 0 0 0 0 20 0 0 0 0 0 0 0\n br0: 100 0 0 0 0 0 0 0 300 0 0 0 0 0 0 0\n",
        )?;
        assert_eq!(sample.rx, 100);
        assert_eq!(sample.tx, 300);
        Ok(())
    }

    #[test]
    fn region_is_ok_when_any_probe_passes() {
        let sites = vec![
            ProxySiteStatus {
                name: "A",
                region: "国内",
                url: "https://a.example",
                ok: false,
                latency_ms: None,
                status: None,
                error: Some("timeout".to_owned()),
            },
            ProxySiteStatus {
                name: "B",
                region: "国内",
                url: "https://b.example",
                ok: true,
                latency_ms: Some(120),
                status: Some(200),
                error: None,
            },
        ];
        assert!(region_ok(&sites, "国内"));
    }

    #[test]
    fn proxy_probe_timeout_covers_slow_but_reachable_foreign_sites() {
        assert!(
            PROXY_CHECK_TIMEOUT >= Duration::from_secs(6),
            "真机海外站点首包约 5 秒，超时阈值过短会产生误报"
        );
    }

    #[test]
    fn mihomo_authorization_header_uses_management_secret() {
        assert_eq!(
            mihomo_authorization("test-secret"),
            "Bearer test-secret"
        );
    }

    #[test]
    fn device_status_does_not_expose_subscriber_identifiers() -> Result<()> {
        let value = serde_json::to_value(device_error("test".to_owned()))?;

        for field in ["iccid", "imei", "imsi", "phone_number"] {
            assert!(value.get(field).is_none(), "状态 API 不应暴露 {field}");
        }
        Ok(())
    }

    #[test]
    fn diagnoses_carrier_restriction_when_dns_works_but_http_is_blocked() {
        let diagnosis = diagnose_network(Some("ppp_connected"), true, false, false, false, false);

        assert_eq!(diagnosis.code, "carrier_restricted");
        assert!(diagnosis.action.contains("欠费"));
    }

    #[test]
    fn diagnoses_proxy_failure_when_direct_network_works() {
        let diagnosis = diagnose_network(Some("ppp_connected"), true, true, false, false, true);

        assert_eq!(diagnosis.code, "proxy_unavailable");
        assert!(diagnosis.action.contains("代理"));
    }

    #[test]
    fn reports_healthy_only_when_direct_and_proxy_paths_pass() {
        let diagnosis = diagnose_network(Some("ppp_connected"), true, true, true, true, true);

        assert_eq!(diagnosis.code, "healthy");
        assert_eq!(diagnosis.level, "ok");
    }

    #[test]
    fn treats_connecting_ppp_state_as_disconnected() {
        assert_eq!(ppp_connected(Some("ppp_connecting")), Some(false));
        assert_eq!(ppp_connected(Some("connecting")), Some(false));
        assert_eq!(ppp_connected(Some("ipv4_ipv6_connected")), Some(true));
    }

    #[test]
    fn parses_thermal_zone_millidegrees() {
        assert_eq!(temperature_celsius("53674\n"), Some(53.7));
        assert_eq!(temperature_celsius("invalid"), None);
        assert_eq!(temperature_celsius("200000"), None);
    }
}
