use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    process::Stdio,
    sync::OnceLock,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    sync::{watch, Mutex},
    time,
};
use tracing::{info, warn};

const EASYTIER_BIN: &str = "/data/adb/f50mgr/bin/easytier";
const EASYTIER_CLI_BIN: &str = "/data/adb/f50mgr/bin/easytier-cli";
const CONFIG_DIR: &str = "/data/adb/f50mgr/config/easytier";
const CONFIG_FILE: &str = "/data/adb/f50mgr/config/easytier/config.toml";
const F50MGR_CONFIG_FILE: &str = "/data/adb/f50mgr/config/f50mgr.toml";
const RUN_DIR: &str = "/data/adb/f50mgr/run";
const PID_FILE: &str = "/data/adb/f50mgr/run/easytier.pid";
const RESTART_DELAY: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
const STABLE_RUN: Duration = Duration::from_secs(60);
const MAX_FAILURES: u32 = 3;
/// 连续失败达 MAX_FAILURES 后的长退避。EasyTier 是可选模块，watchdog 返回 Err 会
/// 连带打死整个 daemon（含 mihomo/TPROXY/Web 面板）—— 只能退避重试，不能放弃。
const FAILURE_BACKOFF: Duration = Duration::from_secs(60);
/// 运行中日志上界检查间隔。
const LOG_CHECK_INTERVAL: Duration = Duration::from_secs(60);
const EASYTIER_CLI_TIMEOUT: Duration = Duration::from_secs(2);
const EASYTIER_CLI_CACHE_TTL: Duration = Duration::from_secs(15);
const EASYTIER_CLI_OUTPUT_LIMIT: usize = 64 * 1024;

static NODE_STATUS_CACHE: OnceLock<Mutex<NodeStatusCache>> = OnceLock::new();

#[derive(Debug, Clone)]
struct Paths {
    bin: PathBuf,
    config_dir: PathBuf,
    config_file: PathBuf,
    run_dir: PathBuf,
    pid_file: PathBuf,
}

impl Paths {
    fn runtime() -> Self {
        Self {
            bin: PathBuf::from(EASYTIER_BIN),
            config_dir: PathBuf::from(CONFIG_DIR),
            config_file: PathBuf::from(CONFIG_FILE),
            run_dir: PathBuf::from(RUN_DIR),
            pid_file: PathBuf::from(PID_FILE),
        }
    }
}

#[derive(Debug)]
pub struct Status {
    pub pid: Option<u32>,
    pub running: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NodeStatus {
    pub available: bool,
    pub stale: bool,
    pub updated_at_unix_ms: Option<u64>,
    pub virtual_ip: Option<String>,
    pub hostname: Option<String>,
    pub peer_id: Option<String>,
    pub public_ip: Option<String>,
    pub udp_nat_type: Option<String>,
    pub version: Option<String>,
    pub listeners: Vec<String>,
    pub peers: Vec<PeerStatus>,
    pub peers_error: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeerStatus {
    pub cidr: String,
    pub hostname: String,
    pub route: String,
    pub latency_ms: Option<String>,
    pub loss_rate: Option<String>,
    pub rx_bytes: Option<String>,
    pub tx_bytes: Option<String>,
    pub tunnel_protocol: Option<String>,
    pub nat_type: String,
    pub version: String,
}

#[derive(Debug, Default)]
struct NodeStatusCache {
    attempted_at: Option<Instant>,
    response: Option<NodeStatus>,
}

impl NodeStatusCache {
    fn current(&self, now: Instant) -> Option<NodeStatus> {
        let attempted_at = self.attempted_at?;
        if now.saturating_duration_since(attempted_at) >= EASYTIER_CLI_CACHE_TTL {
            return None;
        }
        self.response.clone()
    }
}

#[derive(Debug, Deserialize)]
struct NodeCliOutput {
    peer_id: Option<u64>,
    ipv4_addr: Option<String>,
    hostname: Option<String>,
    stun_info: Option<NodeStunInfo>,
    #[serde(default)]
    listeners: Vec<String>,
    version: Option<String>,
}

#[derive(Debug, Deserialize)]
struct NodeStunInfo {
    udp_nat_type: Option<u8>,
    #[serde(default)]
    public_ip: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PeerCliOutput {
    #[serde(default)]
    cidr: String,
    #[serde(default)]
    hostname: String,
    #[serde(default)]
    cost: String,
    #[serde(default)]
    lat_ms: String,
    #[serde(default)]
    loss_rate: String,
    #[serde(default)]
    rx_bytes: String,
    #[serde(default)]
    tx_bytes: String,
    #[serde(default)]
    tunnel_proto: String,
    #[serde(default)]
    nat_type: String,
    #[serde(default)]
    version: String,
}

/// 独立于 `/api/status` 的 EasyTier 节点快照。互斥锁跨刷新持有，保证同一时刻
/// 最多一个 CLI 子进程；成功结果缓存 15 秒，刷新失败时返回旧快照并标记 stale。
pub async fn node_status() -> NodeStatus {
    let cache = NODE_STATUS_CACHE.get_or_init(|| Mutex::new(NodeStatusCache::default()));
    let mut cache = cache.lock().await;
    if let Some(status) = cache.current(Instant::now()) {
        return status;
    }

    let response = match collect_node_status().await {
        Ok(status) => status,
        Err(err) => match &cache.response {
            Some(previous) if previous.available => {
                let mut previous = previous.clone();
                previous.stale = true;
                previous.error = Some(err.to_string());
                previous
            }
            None => node_status_error(err.to_string()),
            Some(_) => node_status_error(err.to_string()),
        },
    };
    cache.attempted_at = Some(Instant::now());
    cache.response = Some(response.clone());
    response
}

async fn collect_node_status() -> Result<NodeStatus> {
    let (node, peers) = tokio::join!(
        run_cli_json(&["-o", "json", "node"]),
        run_cli_json(&["-o", "json", "peer"]),
    );
    let mut status = parse_node_json(&node?, unix_time_ms())?;
    match peers.and_then(|bytes| parse_peer_json(&bytes)) {
        Ok(peers) => status.peers = peers,
        Err(err) => status.peers_error = Some(err.to_string()),
    }
    Ok(status)
}

async fn run_cli_json(args: &[&str]) -> Result<Vec<u8>> {
    let mut child = Command::new(EASYTIER_CLI_BIN)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("启动 EasyTier CLI 失败: {EASYTIER_CLI_BIN}"))?;
    let stdout = child.stdout.take().context("EasyTier CLI 缺少 stdout")?;
    let stderr = child.stderr.take().context("EasyTier CLI 缺少 stderr")?;

    let completed = time::timeout(EASYTIER_CLI_TIMEOUT, async {
        let (status, stdout, stderr) =
            tokio::join!(child.wait(), read_limited(stdout), read_limited(stderr));
        (status, stdout, stderr)
    })
    .await;
    let (exit_status, stdout, _stderr) = match completed {
        Ok((status, stdout, stderr)) => (status?, stdout?, stderr?),
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            bail!("EasyTier CLI 超过 2 秒未响应");
        }
    };
    if !exit_status.success() {
        bail!("EasyTier CLI 退出异常: {exit_status}");
    }
    Ok(stdout)
}

async fn read_limited(reader: impl AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((EASYTIER_CLI_OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .context("读取 EasyTier CLI 输出失败")?;
    if bytes.len() > EASYTIER_CLI_OUTPUT_LIMIT {
        bail!("EasyTier CLI 输出超过 64 KiB");
    }
    Ok(bytes)
}

fn parse_node_json(bytes: &[u8], updated_at_unix_ms: u64) -> Result<NodeStatus> {
    if bytes.len() > EASYTIER_CLI_OUTPUT_LIMIT {
        bail!("EasyTier CLI 输出超过 64 KiB");
    }
    let raw: NodeCliOutput = serde_json::from_slice(bytes).context("解析 EasyTier 节点状态失败")?;
    let stun = raw.stun_info;
    Ok(NodeStatus {
        available: true,
        stale: false,
        updated_at_unix_ms: Some(updated_at_unix_ms),
        virtual_ip: raw.ipv4_addr,
        hostname: raw.hostname,
        peer_id: raw.peer_id.map(|id| id.to_string()),
        public_ip: stun.as_ref().and_then(|s| s.public_ip.first().cloned()),
        udp_nat_type: stun.and_then(|s| s.udp_nat_type).map(|value| match value {
            // EasyTier 2.4.5/2.6.4 真机输出已交叉验证：6 = Symmetric。
            6 => "Symmetric".to_owned(),
            other => format!("类型 {other}"),
        }),
        version: raw.version,
        listeners: raw
            .listeners
            .into_iter()
            .filter(|listener| !listener.starts_with("ring://"))
            .take(8)
            .collect(),
        peers: Vec::new(),
        peers_error: None,
        error: None,
    })
}

fn parse_peer_json(bytes: &[u8]) -> Result<Vec<PeerStatus>> {
    if bytes.len() > EASYTIER_CLI_OUTPUT_LIMIT {
        bail!("EasyTier CLI 输出超过 64 KiB");
    }
    let raw: Vec<PeerCliOutput> =
        serde_json::from_slice(bytes).context("解析 EasyTier Peer 状态失败")?;
    Ok(raw
        .into_iter()
        .take(128)
        .map(|peer| PeerStatus {
            cidr: peer.cidr,
            hostname: peer.hostname,
            route: peer.cost,
            latency_ms: peer_value(peer.lat_ms),
            loss_rate: peer_value(peer.loss_rate),
            rx_bytes: peer_value(peer.rx_bytes),
            tx_bytes: peer_value(peer.tx_bytes),
            tunnel_protocol: peer_value(peer.tunnel_proto),
            nat_type: peer.nat_type,
            version: peer.version,
        })
        .collect())
}

fn peer_value(value: String) -> Option<String> {
    if value.is_empty() || value == "-" {
        None
    } else {
        Some(value)
    }
}

fn node_status_error(error: String) -> NodeStatus {
    NodeStatus {
        available: false,
        stale: false,
        updated_at_unix_ms: None,
        virtual_ip: None,
        hostname: None,
        peer_id: None,
        public_ip: None,
        udp_nat_type: None,
        version: None,
        listeners: Vec::new(),
        peers: Vec::new(),
        peers_error: None,
        error: Some(error),
    }
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub async fn run_watchdog(mut shutdown: watch::Receiver<bool>) -> Result<()> {
    let paths = Paths::runtime();
    let mut failures = 0_u32;
    let mut last_missing: Option<PathBuf> = None;

    // 上一代 daemon 非正常退出（kill -9）会留下孤儿 EasyTier，先回收再进入主循环
    while !crate::process::reap_stale_process("easytier", &paths.pid_file, &paths.bin, STOP_TIMEOUT)
        .await
    {
        if wait_or_shutdown(&mut shutdown, RESTART_DELAY).await {
            return Ok(());
        }
    }

    'main: loop {
        if is_shutdown(&shutdown) {
            break;
        }
        // 配置读取/解析失败不打死 watchdog，按默认 false（可选模块）继续。
        let auto_start = auto_start_enabled().unwrap_or_else(|err| {
            warn!(error = %err, "读取 easytier.enabled 失败，按默认 false 处理");
            false
        });
        if !auto_start {
            remove_pid_file(&paths)?;
            if wait_or_shutdown(&mut shutdown, RESTART_DELAY).await {
                break;
            }
            continue;
        }

        // 每次拉起前都重新渲染，让 secrets 改动 / 代码升级（如新增 listeners）自动生效。
        // render 是纯文件 I/O，几毫秒；watchdog 重启间隔 5s+，开销可忽略。
        match crate::subscription::easytier::render_default(&paths.config_file) {
            Ok(()) => last_missing = None,
            Err(err) => {
                if last_missing.as_deref() != Some(&paths.config_file) {
                    warn!(error = %err, "EasyTier 配置渲染失败，等待 secrets");
                    last_missing = Some(paths.config_file.clone());
                }
                if wait_or_shutdown(&mut shutdown, RESTART_DELAY).await {
                    break;
                }
                continue;
            }
        }
        if !paths.bin.is_file() {
            if last_missing.as_deref() != Some(&paths.bin) {
                warn!(path = %paths.bin.display(), "EasyTier 二进制未就绪");
                last_missing = Some(paths.bin.clone());
            }
            if wait_or_shutdown(&mut shutdown, RESTART_DELAY).await {
                break;
            }
            continue;
        }
        last_missing = None;

        let started_at = Instant::now();
        let mut child = match spawn(&paths) {
            Ok(child) => child,
            Err(err) => {
                failures += 1;
                warn!(error = %err, failures, "启动 EasyTier 失败");
                if backoff_or_shutdown(&mut shutdown, &mut failures).await {
                    break;
                }
                continue;
            }
        };
        let pid = child.id().context("EasyTier 子进程缺少 PID")?;
        write_pid_file(&paths, pid)?;
        info!(pid, "EasyTier 已启动");

        loop {
            tokio::select! {
                status = child.wait() => {
                    remove_pid_file(&paths)?;
                    let status = status.context("等待 EasyTier 退出状态失败")?;
                    if is_shutdown(&shutdown) {
                        break 'main;
                    }
                    if started_at.elapsed() >= STABLE_RUN {
                        failures = 0;
                    }
                    failures += 1;
                    warn!(pid, ?status, failures, "EasyTier 异常退出，准备重启");
                    if backoff_or_shutdown(&mut shutdown, &mut failures).await {
                        break 'main;
                    }
                    break;
                }
                // 周期性日志上界兜底：peer 不可达时 WARN 级重连日志实测 ~70KB/min，
                // 单次运行无法靠 spawn 时轮转控制
                _ = time::sleep(LOG_CHECK_INTERVAL) => {
                    crate::process::truncate_oversized_log(
                        &paths.run_dir.join("easytier.log"),
                        crate::process::LOG_MAX_SIZE,
                    );
                }
                changed = shutdown.changed() => {
                    if changed.is_err() || is_shutdown(&shutdown) {
                        stop_child(&mut child, &paths).await?;
                        break 'main;
                    }
                }
            }
        }
    }

    remove_pid_file(&paths)?;
    Ok(())
}

pub fn status() -> Result<Status> {
    let paths = Paths::runtime();
    status_with_paths(&paths)
}

fn status_with_paths(paths: &Paths) -> Result<Status> {
    let Some(pid) = read_pid_file(paths)? else {
        return Ok(Status {
            pid: None,
            running: false,
        });
    };
    Ok(Status {
        pid: Some(pid),
        running: match crate::process::PidFd::open_verified(pid, &paths.bin) {
            Ok(Some(process)) => process.is_alive()?,
            Ok(None) => false,
            Err(err) => return Err(err).context("无法通过 pidfd 检查 EasyTier 进程"),
        },
    })
}

/// 默认 false（mesh VPN 是可选模块，员工按需启用）。
pub fn auto_start_enabled() -> Result<bool> {
    match fs::read_to_string(F50MGR_CONFIG_FILE) {
        Ok(config) => parse_easytier_enabled(&config),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("读取配置文件失败: {F50MGR_CONFIG_FILE}")),
    }
}

pub fn set_auto_start(enabled: bool) -> Result<()> {
    crate::process::mihomo::update_bool_config(F50MGR_CONFIG_FILE, "easytier", "enabled", enabled)
}

fn parse_easytier_enabled(config: &str) -> Result<bool> {
    let mut in_section = false;
    for line in config.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_section = line == "[easytier]";
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "enabled" {
            continue;
        }
        return match value.trim() {
            "true" => Ok(true),
            "false" => Ok(false),
            value => bail!("easytier.enabled 只能是 true/false，当前值: {value}"),
        };
    }
    Ok(false)
}

fn spawn(paths: &Paths) -> Result<Child> {
    fs::create_dir_all(&paths.run_dir)
        .with_context(|| format!("创建运行目录失败: {}", paths.run_dir.display()))?;
    fs::create_dir_all(&paths.config_dir)
        .with_context(|| format!("创建 EasyTier 配置目录失败: {}", paths.config_dir.display()))?;

    // stdout/stderr 落盘到 run/easytier.log（同 mihomo，防日志蒸发）
    let (stdout, stderr) = crate::process::log_stdio_or_inherit(&paths.run_dir.join("easytier.log"));
    Command::new(&paths.bin)
        .arg("-c")
        .arg(&paths.config_file)
        // console 输出即 run/easytier.log：INFO 级连接重试实测 64KB/min，
        // 压到 warn 从源头限量。config.toml 的 [console_logger] 段实测被忽略，
        // 只有该环境变量生效（easytier --help 标注 ET_CONSOLE_LOG_LEVEL）。
        .env("ET_CONSOLE_LOG_LEVEL", "warn")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("启动 EasyTier 失败: {}", paths.bin.display()))
}

async fn stop_child(child: &mut Child, paths: &Paths) -> Result<()> {
    #[cfg(any(target_os = "android", target_os = "linux"))]
    return stop_child_with_opener(
        child,
        paths,
        crate::process::PidFd::open_verified,
        |process, signal| process.send_signal(signal),
    )
    .await;

    #[cfg(not(any(target_os = "android", target_os = "linux")))]
    return stop_child_with_opener(
        child,
        paths,
        |pid, _| Ok(Some(pid)),
        |pid, signal| signal_process(*pid, signal),
    )
    .await;
}

async fn stop_child_with_opener<H>(
    child: &mut Child,
    paths: &Paths,
    open: impl Fn(u32, &Path) -> std::io::Result<Option<H>>,
    mut send_signal: impl FnMut(&H, libc::c_int) -> std::io::Result<()>,
) -> Result<()> {
    let Some(pid) = child.id() else {
        remove_pid_file(paths)?;
        return Ok(());
    };

    let Some(process) = open(pid, &paths.bin)
        .with_context(|| format!("无法通过 pidfd 固定 EasyTier 子进程: pid={pid}"))?
    else {
        return match child.try_wait().context("检查 EasyTier 是否已退出失败")? {
            Some(status) => {
                info!(pid, ?status, "EasyTier 已先行退出");
                remove_pid_file(paths)
            }
            None => anyhow::bail!("EasyTier 子进程身份不匹配，拒绝按 PID 终止: pid={pid}"),
        };
    };

    info!(pid, "向 EasyTier 发送 SIGTERM");
    match send_signal(&process, libc::SIGTERM) {
        Ok(()) => {}
        Err(err) if err.raw_os_error() == Some(libc::ESRCH) => {
            let _ = child.wait().await;
            remove_pid_file(paths)?;
            return Ok(());
        }
        Err(err) => return Err(err).with_context(|| format!("发送 SIGTERM 失败: pid={pid}")),
    }
    match time::timeout(STOP_TIMEOUT, child.wait()).await {
        Ok(status) => {
            let status = status.context("等待 EasyTier 优雅退出失败")?;
            info!(pid, ?status, "EasyTier 已退出");
        }
        Err(_) => {
            warn!(pid, "EasyTier 5 秒内未退出，发送 SIGKILL");
            match send_signal(&process, libc::SIGKILL) {
                Ok(()) => {}
                Err(err) if err.raw_os_error() == Some(libc::ESRCH) => {}
                Err(err) => return Err(err).context("通过 pidfd 强制终止 EasyTier 失败"),
            }
            child.wait().await.context("等待 EasyTier 强制退出失败")?;
        }
    }
    remove_pid_file(paths)
}

/// 失败退避：连续失败达上限后转入长退避并清零计数，永不放弃。
/// 返回 true 表示收到退出信号。
async fn backoff_or_shutdown(shutdown: &mut watch::Receiver<bool>, failures: &mut u32) -> bool {
    let delay = if *failures >= MAX_FAILURES {
        warn!(
            failures = *failures,
            backoff_secs = FAILURE_BACKOFF.as_secs(),
            "EasyTier 连续异常，转入长退避后继续重试"
        );
        *failures = 0;
        FAILURE_BACKOFF
    } else {
        RESTART_DELAY
    };
    wait_or_shutdown(shutdown, delay).await
}

async fn wait_or_shutdown(shutdown: &mut watch::Receiver<bool>, delay: Duration) -> bool {
    tokio::select! {
        _ = time::sleep(delay) => false,
        changed = shutdown.changed() => changed.is_err() || is_shutdown(shutdown),
    }
}

fn is_shutdown(shutdown: &watch::Receiver<bool>) -> bool {
    *shutdown.borrow()
}

fn write_pid_file(paths: &Paths, pid: u32) -> Result<()> {
    fs::write(&paths.pid_file, format!("{pid}\n"))
        .with_context(|| format!("写入 EasyTier PID 文件失败: {}", paths.pid_file.display()))
}

fn read_pid_file(paths: &Paths) -> Result<Option<u32>> {
    let pid = match fs::read_to_string(&paths.pid_file) {
        Ok(pid) => pid,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("读取 EasyTier PID 文件失败: {}", paths.pid_file.display()))
        }
    };
    Ok(crate::process::parse_pid(&pid))
}

fn remove_pid_file(paths: &Paths) -> Result<()> {
    match fs::remove_file(&paths.pid_file) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err)
            .with_context(|| format!("删除 EasyTier PID 文件失败: {}", paths.pid_file.display())),
    }
}

#[cfg(not(any(target_os = "android", target_os = "linux")))]
fn signal_process(pid: u32, signal: libc::c_int) -> std::io::Result<()> {
    let result = unsafe { libc::kill(pid as libc::pid_t, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// 真机预检报告：检查 TUN 可用性、二进制就绪、Mihomo iptables 共存边界。
/// 输出文本面向运维（adb shell 直接读），不返回结构化数据。
pub fn check_compatibility() -> Result<String> {
    use std::fmt::Write as _;
    let mut out = String::new();
    let mut warnings = 0usize;
    let mut errors = 0usize;

    writeln!(out, "=== EasyTier 兼容性预检 ===").ok();

    // 1. TUN 设备
    let tun_main = Path::new("/dev/net/tun").exists();
    let tun_legacy = Path::new("/dev/tun").exists();
    if tun_main {
        writeln!(out, "[OK] /dev/net/tun 存在").ok();
    } else if tun_legacy {
        writeln!(out, "[WARN] /dev/net/tun 缺失但 /dev/tun 存在；customize.sh 应已建软链").ok();
        warnings += 1;
    } else {
        writeln!(out, "[ERR] /dev/net/tun 与 /dev/tun 都缺失，EasyTier 无法启动").ok();
        errors += 1;
    }

    // 2. 二进制
    let bin = Path::new(EASYTIER_BIN);
    if bin.is_file() {
        writeln!(out, "[OK] {} 存在", bin.display()).ok();
    } else {
        writeln!(
            out,
            "[ERR] {} 缺失（重装 Magisk 模块或重新部署 fallback）",
            bin.display()
        )
        .ok();
        errors += 1;
    }

    // 3. 配置或 secrets
    let cfg = Path::new(CONFIG_FILE);
    if cfg.is_file() {
        writeln!(out, "[OK] 已生成 {}", cfg.display()).ok();
    } else {
        match crate::subscription::easytier::preview() {
            Ok(_) => {
                writeln!(out, "[OK] secrets.toml [easytier] 段可渲染配置").ok();
            }
            Err(err) => {
                writeln!(out, "[ERR] secrets.toml 不可渲染：{err}").ok();
                errors += 1;
            }
        }
    }

    // 4. Mihomo iptables 共存检查（只读）
    match crate::iptables::rules::is_active() {
        Ok(true) => {
            writeln!(out, "[OK] Mihomo iptables 规则已启用，与 EasyTier 共存：").ok();
            writeln!(
                out,
                "       - mangle PREROUTING TPROXY 仅匹配热点接口（br+/wlan+/ap+/...），不影响 F50 本机出向"
            ).ok();
            writeln!(
                out,
                "       - 私网 RETURN 表已含 10.0.0.0/8，EasyTier 子网流量天然放行"
            ).ok();
            writeln!(
                out,
                "       - policy route fwmark pref 100 优先于 EasyTier 的 main 表 pref 32766"
            ).ok();
        }
        Ok(false) => {
            writeln!(out, "[OK] Mihomo iptables 未启用，无共存约束").ok();
        }
        Err(err) => {
            writeln!(out, "[WARN] 无法读取 iptables 状态：{err}").ok();
            warnings += 1;
        }
    }

    // 5. 设备 ID（影响默认 hostname）
    match crate::device::device_id() {
        Ok(id) => {
            writeln!(out, "[OK] 设备标识：F50-{id}").ok();
        }
        Err(err) => {
            writeln!(out, "[WARN] 设备标识读取失败：{err}").ok();
            warnings += 1;
        }
    }

    writeln!(out).ok();
    writeln!(out, "总结：errors={errors}  warnings={warnings}").ok();
    if errors > 0 {
        writeln!(out, "存在 ERR，启动 EasyTier 前需修复").ok();
    }
    Ok(out)
}

pub fn stop_running() -> Result<()> {
    let paths = Paths::runtime();
    stop_running_with_paths(&paths)
}

fn stop_running_with_paths(paths: &Paths) -> Result<()> {
    stop_running_with_opener(
        paths,
        crate::process::PidFd::open_verified,
        |process, signal| process.send_signal(signal),
    )
}

fn stop_running_with_opener<H>(
    paths: &Paths,
    open: impl Fn(u32, &Path) -> std::io::Result<Option<H>>,
    send_signal: impl Fn(&H, libc::c_int) -> std::io::Result<()>,
) -> Result<()> {
    let Some(pid) = read_pid_file(paths)? else {
        return Ok(());
    };
    let Some(process) = open(pid, &paths.bin)
        .with_context(|| format!("无法通过 pidfd 固定 EasyTier 进程: pid={pid}"))?
    else {
        return remove_pid_file(paths);
    };
    match send_signal(&process, libc::SIGTERM) {
        Ok(()) => Ok(()),
        Err(err) if err.raw_os_error() == Some(libc::ESRCH) => {
            remove_pid_file(paths)?;
            Ok(())
        }
        Err(err) => Err(err).with_context(|| format!("停止 EasyTier 失败: pid={pid}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE_JSON: &str = r#"{
        "peer_id": 2609632904,
        "ipv4_addr": "10.0.5.41/24",
        "hostname": "F50-test",
        "stun_info": {"udp_nat_type": 6, "public_ip": ["203.0.113.10"]},
        "listeners": ["ring://private-instance", "tcp://0.0.0.0:14010", "udp://[::]:14010"],
        "config": "[network_identity]\nnetwork_secret = \"must-not-leak\"",
        "version": "2.4.5-test"
    }"#;

    const PEER_JSON: &str = r#"[{
        "cidr": "10.0.5.41/24",
        "ipv4": "10.0.5.41",
        "hostname": "F50-test",
        "cost": "p2p",
        "lat_ms": "45.70",
        "loss_rate": "0.0%",
        "rx_bytes": "23.42 kB",
        "tx_bytes": "27.81 kB",
        "tunnel_proto": "tcp6",
        "nat_type": "Symmetric",
        "id": "2609632904",
        "version": "2.4.5-4c4d172e"
    }]"#;

    #[test]
    fn parses_node_json_and_drops_sensitive_config() -> Result<()> {
        let status = parse_node_json(NODE_JSON.as_bytes(), 1234)?;

        assert_eq!(status.virtual_ip.as_deref(), Some("10.0.5.41/24"));
        assert_eq!(status.hostname.as_deref(), Some("F50-test"));
        assert_eq!(status.peer_id.as_deref(), Some("2609632904"));
        assert_eq!(status.public_ip.as_deref(), Some("203.0.113.10"));
        assert_eq!(status.udp_nat_type.as_deref(), Some("Symmetric"));
        assert_eq!(status.version.as_deref(), Some("2.4.5-test"));
        assert_eq!(status.listeners.len(), 2, "ring listener 不应暴露实例 ID");
        assert_eq!(status.updated_at_unix_ms, Some(1234));

        let serialized = serde_json::to_string(&status)?;
        assert!(!serialized.contains("must-not-leak"));
        assert!(!serialized.contains("private-instance"));
        assert!(!serialized.contains("config"));
        Ok(())
    }

    #[test]
    fn parses_peer_json_for_dashboard_table() -> Result<()> {
        let peers = parse_peer_json(PEER_JSON.as_bytes())?;

        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].cidr, "10.0.5.41/24");
        assert_eq!(peers[0].hostname, "F50-test");
        assert_eq!(peers[0].route, "p2p");
        assert_eq!(peers[0].latency_ms.as_deref(), Some("45.70"));
        assert_eq!(peers[0].version, "2.4.5-4c4d172e");
        Ok(())
    }

    #[test]
    fn rejects_node_json_over_output_limit() {
        let oversized = vec![b' '; EASYTIER_CLI_OUTPUT_LIMIT + 1];
        assert!(parse_node_json(&oversized, 1234).is_err());
    }

    #[test]
    fn caches_failed_node_status_for_full_ttl() {
        let attempted_at = Instant::now();
        let cache = NodeStatusCache {
            attempted_at: Some(attempted_at),
            response: Some(node_status_error("CLI unavailable".to_owned())),
        };

        assert!(cache
            .current(attempted_at + EASYTIER_CLI_CACHE_TTL - Duration::from_millis(1))
            .is_some());
        assert!(cache
            .current(attempted_at + EASYTIER_CLI_CACHE_TTL)
            .is_none());
    }

    #[test]
    fn parses_easytier_enabled_true() -> Result<()> {
        let cfg = "[mihomo]\nauto_start = true\n\n[easytier]\nenabled = true\n";
        assert!(parse_easytier_enabled(cfg)?);
        Ok(())
    }

    #[test]
    fn parses_easytier_enabled_false() -> Result<()> {
        let cfg = "[easytier]\nenabled = false\n";
        assert!(!parse_easytier_enabled(cfg)?);
        Ok(())
    }

    #[test]
    fn defaults_when_section_absent() -> Result<()> {
        let cfg = "[mihomo]\nauto_start = true\n";
        assert!(!parse_easytier_enabled(cfg)?);
        Ok(())
    }

    #[test]
    fn rejects_invalid_value() {
        let cfg = "[easytier]\nenabled = yes\n";
        assert!(parse_easytier_enabled(cfg).is_err());
    }
}
