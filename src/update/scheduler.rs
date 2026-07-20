use std::{
    path::{Path, PathBuf},
    sync::{atomic::{AtomicBool, Ordering}, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use serde::Serialize;
use tokio::{sync::{mpsc, watch}, time};
use tracing::{info, warn};

use super::{
    check_and_prepare, CommandVersionRunner, HttpFetcher, PreparedUpdate, UpdateSettings,
    UpdateSource,
};

const CONFIG_FILE: &str = "/data/adb/f50mgr/config/f50mgr.toml";
const SECRETS_FILE: &str = "/data/adb/f50mgr/config/secrets.toml";
const MIHOMO_BIN: &str = "/data/adb/f50mgr/bin/mihomo";
const RUN_DIR: &str = "/data/adb/f50mgr/run";
const INITIAL_DELAY: Duration = Duration::from_secs(60);
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
fn automatic_updates_enabled() -> bool {
    // 安全门禁：只有签名验证器与离线信任根同时落地后才能改为启用。
    false
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateSnapshot {
    pub state: &'static str,
    pub current_version: Option<String>,
    pub available_version: Option<String>,
    pub source: Option<&'static str>,
    pub last_check_unix: Option<u64>,
    pub error: Option<String>,
}

impl Default for UpdateSnapshot {
    fn default() -> Self {
        Self {
            state: "waiting",
            current_version: None,
            available_version: None,
            source: None,
            last_check_unix: None,
            error: None,
        }
    }
}

static SNAPSHOT: OnceLock<Mutex<UpdateSnapshot>> = OnceLock::new();
static ACTIVATION_PENDING: AtomicBool = AtomicBool::new(false);

pub fn activation_pending() -> bool { ACTIVATION_PENDING.load(Ordering::Acquire) }

impl UpdateSnapshot {
    pub(crate) fn recovery_started() {
        ACTIVATION_PENDING.store(true, Ordering::Release);
        set_snapshot(UpdateSnapshot {
            state: "activating",
            error: Some("检测到未完成的 Mihomo 更新，正在恢复上一确认版本".to_owned()),
            ..update_snapshot()
        });
    }

    pub(crate) fn recovery_completed() {
        ACTIVATION_PENDING.store(false, Ordering::Release);
        set_snapshot(UpdateSnapshot {
            state: "failed",
            available_version: None,
            source: None,
            error: Some("已恢复 daemon 中断前的 Mihomo 确认版本".to_owned()),
            ..update_snapshot()
        });
    }
}

pub(crate) fn activation_started(prepared: &PreparedUpdate) {
    ACTIVATION_PENDING.store(true, Ordering::Release);
    set_snapshot(UpdateSnapshot {
        state: "activating",
        available_version: Some(prepared.version.clone()),
        source: Some(source_label(&prepared.source)),
        error: None,
        ..update_snapshot()
    });
}

pub(crate) fn activation_succeeded(prepared: &PreparedUpdate) {
    ACTIVATION_PENDING.store(false, Ordering::Release);
    set_snapshot(UpdateSnapshot {
        state: "up_to_date",
        current_version: Some(prepared.version.clone()),
        available_version: None,
        source: Some(source_label(&prepared.source)),
        error: None,
        ..update_snapshot()
    });
}

pub(crate) fn activation_failed(prepared: &PreparedUpdate, error: String) {
    ACTIVATION_PENDING.store(false, Ordering::Release);
    set_snapshot(UpdateSnapshot {
        state: "failed",
        available_version: Some(prepared.version.clone()),
        source: Some(source_label(&prepared.source)),
        error: Some(error),
        ..update_snapshot()
    });
}

pub fn update_snapshot() -> UpdateSnapshot {
    snapshot_lock().lock().unwrap_or_else(|err| err.into_inner()).clone()
}

pub async fn run_scheduler(
    sender: mpsc::Sender<PreparedUpdate>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<()> {
    if !automatic_updates_enabled() {
        set_snapshot(UpdateSnapshot {
            state: "disabled_no_trust_root",
            error: None,
            ..update_snapshot()
        });
        info!("未配置离线签名信任根，Mihomo 无人值守更新已禁用");
        while !*shutdown.borrow() {
            if shutdown.changed().await.is_err() { break }
        }
        return Ok(());
    }
    if wait_or_shutdown(INITIAL_DELAY, &mut shutdown).await { return Ok(()) }
    loop {
        if *shutdown.borrow() { return Ok(()) }
        set_snapshot(UpdateSnapshot { state: "checking", ..update_snapshot() });
        let result = tokio::task::spawn_blocking(check_once).await;
        let interval = match result {
            Ok(Ok((current, Some(prepared), interval))) => {
                let version = prepared.version.clone();
                let source = source_label(&prepared.source);
                set_snapshot(UpdateSnapshot {
                    state: "pending",
                    current_version: current,
                    available_version: Some(version.clone()),
                    source: Some(source),
                    last_check_unix: Some(now()),
                    error: None,
                });
                info!(%version, source, "已准备 Mihomo 更新候选，等待 watchdog 激活");
                tokio::select! {
                    result = sender.send(prepared) => if result.is_err() { return Ok(()) },
                    _ = shutdown.changed() => return Ok(()),
                }
                interval
            }
            Ok(Ok((current, None, interval))) => {
                set_snapshot(UpdateSnapshot {
                    state: "up_to_date",
                    current_version: current,
                    available_version: None,
                    source: None,
                    last_check_unix: Some(now()),
                    error: None,
                });
                interval
            }
            Ok(Err(err)) => {
                warn!(error = %err, "Mihomo 自动更新检查失败，保留现有版本");
                record_error(err.to_string());
                Duration::from_secs(6 * 3600)
            }
            Err(err) => {
                warn!(error = %err, "Mihomo 自动更新任务异常，保留现有版本");
                record_error(format!("更新任务异常: {err}"));
                Duration::from_secs(6 * 3600)
            }
        };
        if wait_or_shutdown(interval, &mut shutdown).await { return Ok(()) }
    }
}

fn check_once() -> Result<(Option<String>, Option<PreparedUpdate>, Duration)> {
    let settings = UpdateSettings::load(Path::new(CONFIG_FILE), Path::new(SECRETS_FILE))?;
    let runner = CommandVersionRunner::new(VERSION_TIMEOUT);
    let current = runner.installed_version(Path::new(MIHOMO_BIN)).ok();
    let fetcher = HttpFetcher::new(settings.timeout);
    let prepared = check_and_prepare(
        &fetcher,
        &runner,
        current.as_deref(),
        settings.company.as_ref(),
        &PathBuf::from(RUN_DIR),
    )?;
    Ok((current, prepared, settings.interval))
}

async fn wait_or_shutdown(
    duration: Duration,
    shutdown: &mut watch::Receiver<bool>,
) -> bool {
    tokio::select! {
        _ = time::sleep(duration) => false,
        changed = shutdown.changed() => changed.is_err() || *shutdown.borrow(),
    }
}

fn record_error(error: String) {
    set_snapshot(UpdateSnapshot {
        state: "failed",
        last_check_unix: Some(now()),
        error: Some(error),
        ..update_snapshot()
    });
}

fn set_snapshot(value: UpdateSnapshot) {
    *snapshot_lock().lock().unwrap_or_else(|err| err.into_inner()) = value;
}

fn snapshot_lock() -> &'static Mutex<UpdateSnapshot> {
    SNAPSHOT.get_or_init(|| Mutex::new(UpdateSnapshot::default()))
}

fn source_label(source: &UpdateSource) -> &'static str {
    match source { UpdateSource::GitHub => "github", UpdateSource::Company => "company" }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|value| value.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_signature_verifier_disables_updates_without_sending_candidate() {
        assert!(!automatic_updates_enabled());
        let (sender, mut receiver) = mpsc::channel(1);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let scheduler = tokio::spawn(run_scheduler(sender, shutdown_rx));
        tokio::task::yield_now().await;

        assert_eq!(update_snapshot().state, "disabled_no_trust_root");
        assert!(matches!(receiver.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
        shutdown_tx.send(true).unwrap();
        scheduler.await.unwrap().unwrap();
    }
}
