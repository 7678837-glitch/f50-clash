use std::{
    fs,
    io::ErrorKind,
    net::SocketAddr,
    path::{Path, PathBuf},
    process as std_process,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

const IPTABLES_WATCHDOG_INTERVAL: Duration = Duration::from_secs(30);
const IPTABLES_READY_POLL: Duration = Duration::from_secs(2);
/// mihomo 或蜂窝网络连续不健康达到该 tick 数（间隔 IPTABLES_WATCHDOG_INTERVAL）后
/// 撤除 TPROXY 转直连（fail-open）。取 2 而不是 1：容忍进程重启/网络切换的瞬时抖动，
/// 避免规则频繁拆建。
const PROXY_STACK_UNHEALTHY_TICKS_BEFORE_DISABLE: u32 = 2;
/// mihomo external-controller。可连即代表配置已加载、tproxy/DNS 已就绪。
/// 注意：mihomo 把所有端口绑在 LAN IP 192.168.0.1（非 127.0.0.1），探测须用该地址。
pub(crate) const MIHOMO_API_ADDR: &str = "192.168.0.1:14193";
/// 设备联网门控：等待「公网可连」的最长时间，超时则兜底启动代理栈（避免代理永不启动）。
const NETWORK_READY_TIMEOUT: Duration = Duration::from_secs(180);
const NETWORK_READY_POLL: Duration = Duration::from_secs(3);
/// 直连 HTTP 204 探测目标。停机态仍可能放行 DNS/TCP 53，不能用端口可连代替互联网可用。
/// 两个目标分别覆盖 Android 通用探针与 ZTE 固件自带探针，任一返回 204 即视为健康。
const CONNECTIVITY_PROBE_URLS: [&str; 2] = [
    "http://connectivitycheck.gstatic.com/generate_204",
    "http://netcheck.ztems.com/generate_204",
];
const CONNECTIVITY_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

mod device;
mod iptables;
mod process;
mod status;
mod subscription;
mod traffic;
pub mod update;
mod web;

const RUN_DIR: &str = "/data/adb/f50mgr/run";
const PID_FILE: &str = "/data/adb/f50mgr/run/f50mgr.pid";
const CONFIG_FILE: &str = "/data/adb/f50mgr/config/f50mgr.toml";
const MIHOMO_BIN: &str = "/data/adb/f50mgr/bin/mihomo";
const WEB_LISTEN: &str = "192.168.0.1:14180";

#[derive(Debug, Parser)]
#[command(name = "f50mgr")]
#[command(
    version = env!("F50MGR_VERSION"),
    about = "ZTE F50 Magisk transparent proxy manager"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 以前台 daemon 模式运行，由 Magisk service.sh 拉起。
    Daemon,
    /// 管理透明代理 iptables 规则集。
    Iptables {
        #[command(subcommand)]
        command: Option<IptablesCommand>,
    },
    /// 生成或刷新 Mihomo config.yaml。
    Subscription {
        #[command(subcommand)]
        command: SubscriptionCommand,
    },
    /// 输出当前最小运行状态。
    Status,
    /// EasyTier mesh VPN 工具子命令。
    Easytier {
        #[command(subcommand)]
        command: EasytierCommand,
    },
    /// 管理 Web/Mihomo 共用的设备认证密钥（仅限 root shell）。
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// 首次安装生成密钥；已有密钥只校验并收紧权限。
    Ensure,
    /// 生成新密钥；daemon 重启后旧会话全部失效。
    Reset,
}

#[derive(Debug, Subcommand)]
enum EasytierCommand {
    /// 渲染当前 secrets.toml 对应的 EasyTier config.toml 并打印（不落盘）。
    Render,
    /// 真机预检：检测 TUN 设备、二进制、Mihomo 规则共存性。
    Check,
}

#[derive(Debug, Subcommand)]
enum IptablesCommand {
    /// 启用 F50 透明代理规则。
    Enable,
    /// 禁用并清理 F50 透明代理规则。
    Disable,
    /// 检查规则是否处于启用状态。
    Status,
    /// 打印将写入 iptables-restore 的规则文本。
    PrintRules,
    /// 启用 IPv6 leak guard（DROP 客户端 v6 forward）。
    V6BlockEnable,
    /// 禁用 IPv6 leak guard。
    V6BlockDisable,
    /// 检查 IPv6 leak guard 是否启用。
    V6BlockStatus,
}

#[derive(Debug, Subcommand)]
enum SubscriptionCommand {
    /// 从本地订阅 YAML 生成 F50 专用 config.yaml。
    Render {
        /// 输入的订阅 YAML 路径。
        input: PathBuf,
        /// 输出路径，默认写入运行时 config.yaml。
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// 从订阅 URL 下载并生成 F50 专用 config.yaml。
    Update {
        /// 订阅 URL。
        url: String,
        /// 输出路径，默认写入运行时 config.yaml。
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // 仅 daemon 模式落盘日志：service.sh 拉起时 stdout 被 Magisk 丢弃；
    // CLI 子命令输出面向 root shell 用户，不写文件。
    init_logging(matches!(cli.command, Command::Daemon));

    match cli.command {
        Command::Daemon => run_daemon().await,
        Command::Iptables { command } => run_iptables(command),
        Command::Subscription { command } => run_subscription(command),
        Command::Status => run_status(),
        Command::Easytier { command } => run_easytier(command),
        Command::Auth { command } => run_auth(command),
    }
}

fn run_auth(command: AuthCommand) -> Result<()> {
    match command {
        AuthCommand::Ensure => {
            let result = web::auth_store::ensure_management_key()?;
            if result.created {
                println!("created={}", result.secret);
            } else {
                println!("existing");
            }
        }
        AuthCommand::Reset => println!("secret={}", web::auth_store::reset_management_key()?),
    }
    Ok(())
}

fn run_easytier(command: EasytierCommand) -> Result<()> {
    match command {
        EasytierCommand::Render => {
            let cfg = subscription::easytier::preview()?;
            print!("{cfg}");
        }
        EasytierCommand::Check => {
            let report = process::easytier::check_compatibility()?;
            print!("{report}");
        }
    }
    Ok(())
}

fn init_logging(tee_to_file: bool) {
    let builder = tracing_subscriber::fmt().with_ansi(false).with_target(false);
    if tee_to_file {
        // 落盘失败（首次启动 run/ 未建等）不阻断 daemon，退回纯 stdout
        let _ = fs::create_dir_all(RUN_DIR);
        let log_path = Path::new(RUN_DIR).join("f50mgr.log");
        match process::open_rotated_log(&log_path, process::LOG_MAX_SIZE) {
            Ok(file) => {
                builder
                    .with_writer(move || TeeWriter {
                        file: file.try_clone().ok(),
                    })
                    .init();
                return;
            }
            Err(err) => eprintln!("打开 {} 失败，日志仅输出 stdout: {err}", log_path.display()),
        }
    }
    builder.with_writer(std::io::stdout).init();
}

/// 同时写 stdout 与日志文件；文件句柄复制失败时静默降级为纯 stdout。
struct TeeWriter {
    file: Option<fs::File>,
}

impl std::io::Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Some(file) = &mut self.file {
            let _ = file.write_all(buf);
        }
        std::io::stdout().write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(file) = &mut self.file {
            let _ = file.flush();
        }
        std::io::stdout().flush()
    }
}

async fn run_daemon() -> Result<()> {
    fs::create_dir_all(RUN_DIR).with_context(|| format!("创建运行目录失败: {RUN_DIR}"))?;
    write_pid_file()?;

    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("系统时间早于 UNIX_EPOCH")?
        .as_secs();

    info!(
        version = env!("F50MGR_VERSION"),
        pid = std_process::id(),
        started_at_unix = started_at,
        "f50mgr daemon started"
    );

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    recover_interrupted_mihomo_update(Path::new(MIHOMO_BIN), iptables::rules::disable)?;

    // IPv6 leak guard 是纯 DROP 规则，与联网无关，立即应用。
    if block_lan_ipv6_enabled()? {
        match iptables::v6_block::enable() {
            Ok(()) => info!("开机已启用 IPv6 leak guard（DROP 客户端 v6 forward）"),
            Err(err) => warn!(error = %err, "启用 IPv6 leak guard 失败"),
        }
    } else {
        // 配置关闭时主动清理残留规则
        let _ = iptables::v6_block::disable();
    }

    // Web 管理界面立即启动：即使设备尚未联网，也要能访问管理面板。
    let web_addr = WEB_LISTEN.parse::<SocketAddr>()?;
    let mut web_task = tokio::spawn(web::run(web_addr, shutdown_rx.clone()));

    // 代理栈（mihomo / EasyTier / TPROXY）必须等设备本身能联网后再启动，
    // 避免无网时拉起代理栈并把 LAN 客户端流量黑洞（开机竞态导致「无网络」）。
    let auto_enable = iptables_auto_enable()?;
    let mut services_task = tokio::spawn(run_proxy_services(shutdown_rx.clone(), auto_enable));

    let daemon_result: Result<()> = tokio::select! {
        signal_result = wait_for_shutdown_signal() => {
            signal_result?;
            info!("收到退出信号，准备关闭 f50mgr daemon");
            let _ = shutdown_tx.send(true);
            wait_task(&mut services_task, "代理服务").await?;
            wait_task(&mut web_task, "Web 管理界面").await
        }
        services_result = &mut services_task => {
            match services_result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(err)) => Err(err).context("代理服务异常退出"),
                Err(err) => Err(err).context("代理服务任务失败"),
            }
        }
        web_result = &mut web_task => {
            match web_result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(err)) => Err(err).context("Web 管理界面异常退出"),
                Err(err) => Err(err).context("Web 管理界面任务失败"),
            }
        }
    };

    // TPROXY fail-closed：daemon 退出后无人守护 mihomo，残留规则会把 LAN 全部流量
    // 打进死端口形成持久黑洞。无论正常退出还是错误退出，都撤规则转直连（fail-open）。
    match iptables::rules::is_active() {
        Ok(true) => match iptables::rules::disable() {
            Ok(()) => info!("daemon 退出：已撤除 iptables 规则（fail-open 直连）"),
            Err(err) => warn!(error = %err, "daemon 退出时撤除 iptables 规则失败"),
        },
        Ok(false) => {}
        Err(err) => warn!(error = %err, "daemon 退出时检查 iptables 状态失败"),
    }

    let cleanup_result = remove_pid_file();
    daemon_result?;
    cleanup_result
}

fn recover_interrupted_mihomo_update(
    current: &Path,
    disable_rules: impl FnOnce() -> Result<()>,
) -> Result<()> {
    if !update::InstalledUpdate::interrupted_update_pending(current)? {
        return Ok(());
    }
    update::UpdateSnapshot::recovery_started();
    disable_rules().context("恢复中断的 Mihomo 更新前撤除 TPROXY 规则失败")?;
    update::InstalledUpdate::recover_interrupted_update(current)
        .context("恢复中断的 Mihomo 更新失败")?;
    update::UpdateSnapshot::recovery_completed();
    warn!("检测到未完成的 Mihomo 更新，已撤除 TPROXY 并恢复上一确认版本");
    Ok(())
}

/// 等设备本身能联网后，再启动 mihomo / EasyTier / 透明代理三件套。
/// 收到退出信号时负责回收三个子任务。
async fn run_proxy_services(
    mut shutdown: watch::Receiver<bool>,
    auto_enable: bool,
) -> Result<()> {
    if !wait_for_network_ready(&mut shutdown).await {
        return Ok(());
    }
    info!("设备网络已就绪，开始启动 mihomo / EasyTier / 透明代理");

    let (update_tx, update_rx) = mpsc::channel(1);
    let mut mihomo_task = tokio::spawn(process::mihomo::run_watchdog_with_updates(
        shutdown.clone(),
        update_rx,
    ));
    let mut easytier_task = tokio::spawn(process::easytier::run_watchdog(shutdown.clone()));
    let mut update_task = tokio::spawn(update::run_scheduler(update_tx, shutdown.clone()));
    // TPROXY 由 watchdog 内部再做一次门控（等 mihomo API 监听就绪）后启用，避免 mihomo
    // 还没监听时 TPROXY 黑洞流量。
    let mut iptables_watchdog_task =
        tokio::spawn(run_iptables_watchdog(shutdown.clone(), auto_enable));

    tokio::select! {
        _ = shutdown.changed() => {
            wait_task(&mut mihomo_task, "Mihomo watchdog").await?;
            wait_task(&mut easytier_task, "EasyTier watchdog").await?;
            wait_task(&mut update_task, "Mihomo 更新调度器").await?;
            wait_task(&mut iptables_watchdog_task, "iptables watchdog").await
        }
        watchdog_result = &mut mihomo_task => {
            match watchdog_result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(err)) => Err(err).context("Mihomo watchdog 异常退出"),
                Err(err) => Err(err).context("Mihomo watchdog 任务失败"),
            }
        }
        easytier_result = &mut easytier_task => {
            match easytier_result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(err)) => Err(err).context("EasyTier watchdog 异常退出"),
                Err(err) => Err(err).context("EasyTier watchdog 任务失败"),
            }
        }
        update_result = &mut update_task => {
            match update_result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(err)) => Err(err).context("Mihomo 更新调度器异常退出"),
                Err(err) => Err(err).context("Mihomo 更新调度器任务失败"),
            }
        }
        iptables_result = &mut iptables_watchdog_task => {
            match iptables_result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(err)) => Err(err).context("iptables watchdog 异常退出"),
                Err(err) => Err(err).context("iptables watchdog 任务失败"),
            }
        }
    }
}

fn iptables_auto_enable() -> Result<bool> {
    match fs::read_to_string(CONFIG_FILE) {
        Ok(config) => parse_iptables_auto_enable(&config),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("读取配置文件失败: {CONFIG_FILE}")),
    }
}

fn parse_iptables_auto_enable(config: &str) -> Result<bool> {
    parse_iptables_bool(config, "auto_enable", false)
}

fn block_lan_ipv6_enabled() -> Result<bool> {
    match fs::read_to_string(CONFIG_FILE) {
        Ok(config) => parse_iptables_bool(&config, "block_lan_ipv6", true),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(true),
        Err(err) => Err(err).with_context(|| format!("读取配置文件失败: {CONFIG_FILE}")),
    }
}

fn parse_iptables_bool(config: &str, key_name: &str, default: bool) -> Result<bool> {
    let mut in_iptables = false;

    for line in config.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            in_iptables = line == "[iptables]";
            continue;
        }
        if !in_iptables {
            continue;
        }

        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != key_name {
            continue;
        }

        return match value.trim() {
            "true" => Ok(true),
            "false" => Ok(false),
            value => anyhow::bail!("iptables.{key_name} 只能是 true/false，当前值: {value}"),
        };
    }

    Ok(default)
}

async fn wait_task<T>(
    task: &mut tokio::task::JoinHandle<Result<T>>,
    name: &'static str,
) -> Result<T> {
    match task.await {
        Ok(result) => result.with_context(|| format!("{name} 退出失败")),
        Err(err) => Err(err).with_context(|| format!("等待 {name} 任务失败")),
    }
}

/// TPROXY 规则该执行的维护动作。TPROXY 是 fail-closed 的：规则在而 mihomo 不健康
/// = LAN 全断网，所以所有分支的兜底方向都是「宁可直连，不可黑洞」。
#[derive(Debug, PartialEq, Eq)]
enum TproxyAction {
    /// 现状正确，不动。
    Keep,
    /// mihomo 健康但规则丢失（ZTE 系统重置）→ 恢复。
    Enable,
    /// mihomo 持续不健康而规则仍在 → 撤规则转直连（fail-open）。
    Disable,
}

fn tproxy_watchdog_action(
    mihomo_healthy: bool,
    network_healthy: bool,
    rules_active: bool,
    unhealthy_ticks: u32,
    update_pending: bool,
) -> TproxyAction {
    if update_pending {
        return if rules_active { TproxyAction::Disable } else { TproxyAction::Keep };
    }
    if mihomo_healthy && network_healthy {
        if rules_active {
            TproxyAction::Keep
        } else {
            TproxyAction::Enable
        }
    } else if rules_active && unhealthy_ticks >= PROXY_STACK_UNHEALTHY_TICKS_BEFORE_DISABLE {
        TproxyAction::Disable
    } else {
        // 不健康 + 规则不在：保持直连，绝不启用（启用即黑洞）。
        TproxyAction::Keep
    }
}

/// 定期检测 iptables 规则存活，若丢失则自动恢复；mihomo 持续不健康时撤规则转直连。
/// ZTE NetworkManagement 在开机初始化时会刷新 iptables，可能冲掉 F50MGR_EXTERNAL 跳转。
async fn run_iptables_watchdog(
    mut shutdown: watch::Receiver<bool>,
    auto_enable: bool,
) -> Result<()> {
    if !auto_enable {
        return Ok(());
    }

    // 开机就绪门控：mihomo API 与蜂窝网络必须同时可用才启用 TPROXY。无超时兜底——
    // 任一侧不健康时启用都会把 LAN 流量打进不可用代理栈，形成持久黑洞。
    if !wait_for_proxy_stack_ready(&mut shutdown).await {
        return Ok(());
    }
    match iptables::rules::enable() {
        Ok(()) => info!("就绪门控通过，已启用 f50mgr iptables 规则"),
        Err(err) => warn!(error = %err, "就绪门控后启用 f50mgr iptables 规则失败，等待 watchdog 重试"),
    }

    let mut interval = tokio::time::interval(IPTABLES_WATCHDOG_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut unhealthy_ticks = 0_u32;
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let mihomo_healthy = mihomo_api_ready().await;
                let network_healthy = mihomo_healthy && connectivity_ok().await;
                if mihomo_healthy && network_healthy {
                    unhealthy_ticks = 0;
                } else {
                    unhealthy_ticks += 1;
                }
                match iptables::rules::is_active() {
                    Ok(active) => match tproxy_watchdog_action(
                        mihomo_healthy,
                        network_healthy,
                        active,
                        unhealthy_ticks,
                        update::activation_pending(),
                    ) {
                        TproxyAction::Keep => {}
                        TproxyAction::Enable => {
                            warn!("iptables 规则已丢失（ZTE 系统可能重置了规则），尝试自动恢复");
                            match iptables::rules::enable() {
                                Ok(()) => info!("iptables 规则已自动恢复"),
                                Err(err) => warn!(error = %err, "自动恢复 iptables 规则失败"),
                            }
                        }
                        TproxyAction::Disable => {
                            warn!(
                                unhealthy_ticks,
                                mihomo_healthy,
                                network_healthy,
                                "代理栈持续不健康，撤除 TPROXY 规则转直连（fail-open），恢复后自动重新启用"
                            );
                            if let Err(err) = iptables::rules::disable() {
                                warn!(error = %err, "撤除 iptables 规则失败");
                            }
                        }
                    },
                    Err(err) => warn!(error = %err, "检查 iptables 状态失败"),
                }
                maintain_v6_guard();
            }
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
        }
    }
    Ok(())
}

/// IPv6 leak guard 存活检查：ZTE 刷新 ip6tables 同样会冲掉 FORWARD 跳转（泄漏，非断网）。
/// 局限：仅随 TPROXY watchdog（auto_enable=true）运行；auto_enable=false 时 v6 只在开机
/// 启用一次，不做周期恢复。
fn maintain_v6_guard() {
    match block_lan_ipv6_enabled() {
        Ok(true) => match iptables::v6_block::is_active() {
            Ok(true) => {}
            Ok(false) => {
                warn!("IPv6 leak guard 已丢失，尝试自动恢复");
                match iptables::v6_block::enable() {
                    Ok(()) => info!("IPv6 leak guard 已自动恢复"),
                    Err(err) => warn!(error = %err, "自动恢复 IPv6 leak guard 失败"),
                }
            }
            Err(err) => warn!(error = %err, "检查 IPv6 leak guard 状态失败"),
        },
        Ok(false) => {}
        Err(err) => warn!(error = %err, "读取 block_lan_ipv6 配置失败"),
    }
}

/// 设备联网门控：等「存在默认路由 + 公网可连」后返回 true（就绪或超时兜底），
/// 收到退出信号返回 false。用于决定何时启动 mihomo / EasyTier / TPROXY。
async fn wait_for_network_ready(shutdown: &mut watch::Receiver<bool>) -> bool {
    let deadline = Instant::now() + NETWORK_READY_TIMEOUT;
    loop {
        if *shutdown.borrow() {
            return false;
        }
        if connectivity_ok().await {
            info!("设备联网门控：公网可连");
            return true;
        }
        if Instant::now() >= deadline {
            warn!("设备联网门控超时（默认路由/公网探测未在限定时间内通过），兜底启动代理栈");
            return true;
        }
        tokio::select! {
            _ = tokio::time::sleep(NETWORK_READY_POLL) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return false;
                }
            }
        }
    }
}

/// 公网连通性探测：任一直连目标明确返回 HTTP 204 才视为设备已能联网。
async fn connectivity_ok() -> bool {
    tokio::task::spawn_blocking(|| {
        let agent = connectivity_probe_agent();
        for url in CONNECTIVITY_PROBE_URLS {
            let status = agent
                .get(url)
                .call()
                .ok()
                .map(|response| response.status().as_u16());
            if connectivity_probe_succeeded(status) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

fn connectivity_probe_agent() -> ureq::Agent {
    connectivity_probe_config().into()
}

fn connectivity_probe_config() -> ureq::config::Config {
    ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .timeout_global(Some(CONNECTIVITY_PROBE_TIMEOUT))
        .build()
}

fn connectivity_probe_succeeded(status: Option<u16>) -> bool {
    status == Some(204)
}

/// 开机就绪门控循环：mihomo API 就绪返回 true，收到退出信号返回 false。
/// 没有超时兜底：TPROXY fail-closed，mihomo 未监听时启用规则等于黑洞全部 LAN 流量，
/// 宁可无限期保持直连（每 60s 记一条进度日志便于排查）。
async fn wait_for_proxy_stack_ready(shutdown: &mut watch::Receiver<bool>) -> bool {
    let mut last_progress_log = Instant::now();
    loop {
        if *shutdown.borrow() {
            return false;
        }
        if proxy_stack_ready(
            mihomo_api_ready().await,
            connectivity_ok().await,
            update::activation_pending(),
        ) {
            info!("就绪门控：mihomo API 与蜂窝网络均就绪");
            return true;
        }
        if last_progress_log.elapsed() >= Duration::from_secs(60) {
            warn!("就绪门控：mihomo API/蜂窝网络未同时就绪，TPROXY 保持关闭（直连），继续等待");
            last_progress_log = Instant::now();
        }
        tokio::select! {
            _ = tokio::time::sleep(IPTABLES_READY_POLL) => {}
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    return false;
                }
            }
        }
    }
}

fn proxy_stack_ready(mihomo_ready: bool, network_ready: bool, update_pending: bool) -> bool {
    mihomo_ready && network_ready && !update_pending
}

/// mihomo external-controller 端口可连即代表 mihomo 已加载配置、tproxy/DNS 监听已就绪。
async fn mihomo_api_ready() -> bool {
    matches!(
        tokio::time::timeout(
            Duration::from_secs(1),
            tokio::net::TcpStream::connect(MIHOMO_API_ADDR),
        )
        .await,
        Ok(Ok(_))
    )
}

async fn wait_for_shutdown_signal() -> Result<()> {
    let mut sigterm = signal(SignalKind::terminate()).context("注册 SIGTERM 监听失败")?;

    tokio::select! {
        _ = sigterm.recv() => {}
        ctrl_c = tokio::signal::ctrl_c() => {
            ctrl_c.context("监听 Ctrl-C 失败")?;
        }
    }

    Ok(())
}

fn write_pid_file() -> Result<()> {
    fs::write(PID_FILE, format!("{}\n", std_process::id()))
        .with_context(|| format!("写入 PID 文件失败: {PID_FILE}"))
}

fn remove_pid_file() -> Result<()> {
    match fs::remove_file(PID_FILE) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            warn!(path = PID_FILE, "PID 文件已不存在");
            Ok(())
        }
        Err(err) => Err(err).with_context(|| format!("删除 PID 文件失败: {PID_FILE}")),
    }
}

fn run_iptables(command: Option<IptablesCommand>) -> Result<()> {
    match command.unwrap_or(IptablesCommand::Status) {
        IptablesCommand::Enable => {
            iptables::rules::enable()?;
            println!("iptables: enabled");
        }
        IptablesCommand::Disable => {
            iptables::rules::disable()?;
            println!("iptables: disabled");
        }
        IptablesCommand::Status => {
            let status = if iptables::rules::is_active()? {
                "active"
            } else {
                "inactive"
            };
            println!("iptables: {status}");
        }
        IptablesCommand::PrintRules => {
            print!("{}", iptables::rules::restore_rules());
        }
        IptablesCommand::V6BlockEnable => {
            iptables::v6_block::enable()?;
            println!("v6_block: enabled");
        }
        IptablesCommand::V6BlockDisable => {
            iptables::v6_block::disable()?;
            println!("v6_block: disabled");
        }
        IptablesCommand::V6BlockStatus => {
            let s = if iptables::v6_block::is_active()? { "active" } else { "inactive" };
            println!("v6_block: {s}");
        }
    }
    Ok(())
}

fn run_subscription(command: SubscriptionCommand) -> Result<()> {
    match command {
        SubscriptionCommand::Render { input, output } => {
            let output = output.unwrap_or_else(subscription::default_output);
            subscription::render_from_file(&input, &output)?;
            println!("subscription: rendered\noutput: {}", output.display());
        }
        SubscriptionCommand::Update { url, output } => {
            let output = output.unwrap_or_else(subscription::default_output);
            subscription::update_from_url(&url, &output)?;
            println!("subscription: updated\noutput: {}", output.display());
        }
    }
    Ok(())
}

fn run_status() -> Result<()> {
    println!("f50mgr {}", env!("F50MGR_VERSION"));

    match fs::read_to_string(PID_FILE) {
        Ok(pid) => println!("daemon: running\npid: {}", pid.trim()),
        Err(err) if err.kind() == ErrorKind::NotFound => println!("daemon: stopped"),
        Err(err) => return Err(err).with_context(|| format!("读取 PID 文件失败: {PID_FILE}")),
    }

    let mihomo = process::mihomo::status()?;
    match (mihomo.running, mihomo.pid) {
        (true, Some(pid)) => println!("mihomo: running\nmihomo_pid: {pid}"),
        (false, Some(pid)) => println!("mihomo: stopped\nmihomo_stale_pid: {pid}"),
        _ => println!("mihomo: stopped"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use super::{
        connectivity_probe_agent, connectivity_probe_succeeded, tproxy_watchdog_action,
        proxy_stack_ready, recover_interrupted_mihomo_update, TproxyAction,
        PROXY_STACK_UNHEALTHY_TICKS_BEFORE_DISABLE,
    };

    #[test]
    fn startup_disables_tproxy_before_recovering_interrupted_update() {
        let dir = std::env::temp_dir().join(format!(
            "f50mgr-startup-recovery-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let current = dir.join("mihomo");
        let pending = dir.join("mihomo.pending");
        std::fs::write(&current, b"unverified").unwrap();
        std::fs::write(&pending, b"confirmed").unwrap();

        recover_interrupted_mihomo_update(&current, || {
            assert!(pending.exists(), "撤规则必须发生在磁盘事务恢复之前");
            Ok(())
        })
        .unwrap();

        assert_eq!(std::fs::read(&current).unwrap(), b"confirmed");
        assert!(!pending.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cli_exposes_root_auth_ensure_and_reset_commands() {
        for subcommand in ["ensure", "reset"] {
            let cli = super::Cli::try_parse_from(["f50mgr", "auth", subcommand]).unwrap();
            assert!(matches!(cli.command, super::Command::Auth { .. }));
        }
    }

    #[test]
    fn connectivity_probe_is_direct_and_does_not_follow_redirects() {
        let agent = connectivity_probe_agent();
        assert!(agent.config().proxy().is_none());
        assert_eq!(agent.config().max_redirects(), 0);
    }

    #[test]
    fn connectivity_probe_accepts_only_http_204() {
        assert!(connectivity_probe_succeeded(Some(204)));
        assert!(!connectivity_probe_succeeded(Some(200)));
        assert!(!connectivity_probe_succeeded(Some(302)));
        assert!(!connectivity_probe_succeeded(None));
    }

    #[test]
    fn watchdog_restores_rules_when_mihomo_healthy_but_rules_lost() {
        // ZTE 冲掉跳转的自愈路径
        assert_eq!(
            tproxy_watchdog_action(true, true, false, 0, false),
            TproxyAction::Enable
        );
    }

    #[test]
    fn watchdog_keeps_rules_when_all_healthy() {
        assert_eq!(
            tproxy_watchdog_action(true, true, true, 0, false),
            TproxyAction::Keep
        );
    }

    #[test]
    fn watchdog_tolerates_transient_mihomo_hiccup() {
        // mihomo watchdog 重启窗口的瞬时不可连不应立刻拆规则
        assert_eq!(
            tproxy_watchdog_action(false, true, true, 1, false),
            TproxyAction::Keep
        );
    }

    #[test]
    fn watchdog_fails_open_when_mihomo_stays_unhealthy() {
        // TPROXY fail-closed：mihomo 持续不健康时必须撤规则转直连，不能维持黑洞
        assert_eq!(
            tproxy_watchdog_action(
                false,
                true,
                true,
                PROXY_STACK_UNHEALTHY_TICKS_BEFORE_DISABLE,
                false,
            ),
            TproxyAction::Disable
        );
    }

    #[test]
    fn watchdog_never_enables_rules_while_mihomo_unhealthy() {
        // 规则不在 + mihomo 不健康：保持直连，绝不启用（启用即黑洞）
        assert_eq!(
            tproxy_watchdog_action(false, true, false, 99, false),
            TproxyAction::Keep
        );
    }

    #[test]
    fn watchdog_fails_open_when_cellular_network_stays_unhealthy() {
        // mihomo API 仍可连不代表蜂窝可出网；网络切换后持续失联也必须撤 TPROXY。
        assert_eq!(
            tproxy_watchdog_action(
                true,
                false,
                true,
                PROXY_STACK_UNHEALTHY_TICKS_BEFORE_DISABLE,
                false,
            ),
            TproxyAction::Disable
        );
    }

    #[test]
    fn watchdog_keeps_tproxy_fail_open_during_update_validation() {
        assert_eq!(
            tproxy_watchdog_action(true, true, true, 0, true),
            TproxyAction::Disable
        );
        assert_eq!(
            tproxy_watchdog_action(true, true, false, 0, true),
            TproxyAction::Keep
        );
        assert!(!proxy_stack_ready(true, true, true));
    }
}
