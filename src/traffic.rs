//! 月度流量自计数：从 /proc/net/dev sipa_eth0 累计 rx/tx，跨重启用 anchor+carry 算法。
//!
//! ZTE F50 的 goform monthly_rx_bytes/monthly_tx_bytes 字段疑似有 tx/rx 倒置 bug
//! （2026-05-14 实测：实时 rx > tx，但月度 tx >> rx 反常）。本模块用 Linux 内核计数器
//! 自行累计，月初按 (year, month) 自动复位，daemon 重启不丢累计值。

use std::{
    fs::{self, File},
    io::{ErrorKind, Write},
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

const STATE_FILE: &str = "/data/adb/f50mgr/data/monthly_traffic.json";
const PROC_NET_DEV: &str = "/proc/net/dev";
const CELLULAR_IFACE: &str = "sipa_eth0";
static OBSERVE_LOCK: Mutex<()> = Mutex::new(());

/// 当前月份字符串 `YYYY-MM`，用 UTC+8（北京时间）按月份切换
fn current_period() -> String {
    // UTC 秒 + 8 小时偏移
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
        + 8 * 3600;
    let (y, m, _) = days_to_ymd(secs / 86400);
    format!("{y:04}-{m:02}")
}

/// 把 1970-01-01 起的天数转成 (year, month, day)。简易实现，不依赖 chrono。
fn days_to_ymd(mut days: i64) -> (i32, u32, u32) {
    let mut year = 1970_i32;
    loop {
        let leap = is_leap(year);
        let dy = if leap { 366 } else { 365 };
        if days >= dy as i64 {
            days -= dy as i64;
            year += 1;
        } else {
            break;
        }
    }
    let leap = is_leap(year);
    let mdays = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1_u32;
    for &d in &mdays {
        if days >= d as i64 {
            days -= d as i64;
            month += 1;
        } else {
            break;
        }
    }
    (year, month, days as u32 + 1)
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonthlyTraffic {
    pub period: String,
    /// 月度累计 rx 字节数（设备视角下行）
    pub rx_bytes: u64,
    /// 月度累计 tx 字节数（设备视角上行）
    pub tx_bytes: u64,
    pub last_updated_unix: u64,
    /// 本会话起始的内核计数器 anchor，用于增量计算；持久化后下次启动作为 carry 基线
    last_anchor_rx: u64,
    last_anchor_tx: u64,
}

impl MonthlyTraffic {
    fn empty(period: String) -> Self {
        Self {
            period,
            rx_bytes: 0,
            tx_bytes: 0,
            last_updated_unix: 0,
            last_anchor_rx: 0,
            last_anchor_tx: 0,
        }
    }
}

/// 读 /proc/net/dev 拿 (rx, tx) 累计字节数。
pub fn read_iface_counters(iface: &str) -> Result<(u64, u64)> {
    let content = fs::read_to_string(PROC_NET_DEV)
        .with_context(|| format!("读取 {PROC_NET_DEV} 失败"))?;
    parse_proc_net_dev(&content, iface)
}

fn parse_proc_net_dev(content: &str, iface: &str) -> Result<(u64, u64)> {
    let prefix = format!("{iface}:");
    for line in content.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(&prefix) {
            let nums: Vec<&str> = rest.split_whitespace().collect();
            // /proc/net/dev 接口冒号后：rx_bytes rx_packets ... (8 字段) tx_bytes tx_packets ...
            if nums.len() < 9 {
                bail!("/proc/net/dev 字段不足: {line}");
            }
            let rx = nums[0]
                .parse::<u64>()
                .with_context(|| format!("rx_bytes 解析失败: {}", nums[0]))?;
            let tx = nums[8]
                .parse::<u64>()
                .with_context(|| format!("tx_bytes 解析失败: {}", nums[8]))?;
            return Ok((rx, tx));
        }
    }
    bail!("接口 {iface} 在 /proc/net/dev 中未找到")
}

/// 空状态的周期用调用方传入的 fallback_period（而不是内部再取真实时钟）：
/// observe_with 传测试周期时若与真实时钟月份不同，会误触发跨月复位分支。
fn load_from(path: &Path, fallback_period: &str) -> Result<MonthlyTraffic> {
    match fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s)
            .with_context(|| format!("解析 {} 失败", path.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => {
            Ok(MonthlyTraffic::empty(fallback_period.to_owned()))
        }
        Err(e) => Err(e).with_context(|| format!("读取 {} 失败", path.display())),
    }
}

fn save_to(path: &Path, state: &MonthlyTraffic) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建状态目录失败: {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(state).context("序列化月度状态失败")?;
    let tmp = path.with_extension("json.tmp");
    let mut file =
        File::create(&tmp).with_context(|| format!("创建临时月度状态失败: {}", tmp.display()))?;
    file.write_all(json.as_bytes())
        .with_context(|| format!("写临时月度状态失败: {}", tmp.display()))?;
    file.sync_all()
        .with_context(|| format!("同步临时月度状态失败: {}", tmp.display()))?;
    drop(file);
    // F50 的 /data/adb 目录 fsync 会长时间阻塞；临时文件先落盘，再依赖同文件系统原子 rename。
    fs::rename(&tmp, path)
        .with_context(|| format!("替换月度状态失败: {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
fn observe_with(
    path: &Path,
    current_rx: u64,
    current_tx: u64,
    now_period: &str,
) -> Result<MonthlyTraffic> {
    let _guard = OBSERVE_LOCK
        .lock()
        .map_err(|_| anyhow!("月流量状态锁已中毒"))?;
    observe_with_locked(path, current_rx, current_tx, now_period)
}

/// 用当前 (rx, tx) 内核累计值 + 持久化状态，返回更新后的月度统计并原子写回。
/// 调用方必须持有 OBSERVE_LOCK，保证读取计数器和提交锚点属于同一事务。
fn observe_with_locked(
    path: &Path,
    current_rx: u64,
    current_tx: u64,
    now_period: &str,
) -> Result<MonthlyTraffic> {
    let mut state = load_from(path, now_period)?;

    // 1) 月份切换：复位
    if state.period != now_period {
        state = MonthlyTraffic::empty(now_period.to_owned());
        state.last_anchor_rx = current_rx;
        state.last_anchor_tx = current_tx;
    }

    // 2) 重启检测：current 比 last_anchor 小 → 内核计数器被重置，把已知 anchor 值算进 carry
    //    （表示重启前的累计已经读过的部分照常保留，重新建立 anchor）
    if current_rx < state.last_anchor_rx || current_tx < state.last_anchor_tx {
        state.last_anchor_rx = 0;
        state.last_anchor_tx = 0;
    }

    // 3) 增量 = current - anchor，累加到 rx/tx_bytes
    let delta_rx = current_rx.saturating_sub(state.last_anchor_rx);
    let delta_tx = current_tx.saturating_sub(state.last_anchor_tx);
    state.rx_bytes = state.rx_bytes.saturating_add(delta_rx);
    state.tx_bytes = state.tx_bytes.saturating_add(delta_tx);

    // 4) 更新 anchor
    state.last_anchor_rx = current_rx;
    state.last_anchor_tx = current_tx;
    state.last_updated_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    save_to(path, &state)?;
    Ok(state)
}

/// 一次性采样：读 sipa_eth0 当前累计 → 调 observe → 返回月度状态
pub fn sample() -> Result<MonthlyTraffic> {
    sample_with(
        Path::new(STATE_FILE),
        current_period,
        || read_iface_counters(CELLULAR_IFACE),
    )
}

fn sample_with<P, F>(path: &Path, period: P, read: F) -> Result<MonthlyTraffic>
where
    P: FnOnce() -> String,
    F: FnOnce() -> Result<(u64, u64)>,
{
    let _guard = OBSERVE_LOCK
        .lock()
        .map_err(|_| anyhow!("月流量状态锁已中毒"))?;
    let now_period = period();
    let (rx, tx) = read()?;
    observe_with_locked(path, rx, tx, &now_period)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        path::PathBuf,
        sync::{Arc, Barrier},
        thread,
    };

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "f50mgr-traffic-{}-{}-{}.json",
            name,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn parse_proc_net_dev_extracts_rx_tx() -> Result<()> {
        let sample = "Inter-|   Receive                                                |  Transmit\n face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed\n    lo: 12345     67    0    0    0     0          0         0    12345      67    0    0    0     0       0          0\nsipa_eth0: 411939049  578051    0  301    0     0          0         0 236743327 2373568    41    0    0     0       0          0\n";
        let (rx, tx) = parse_proc_net_dev(sample, "sipa_eth0")?;
        assert_eq!(rx, 411939049);
        assert_eq!(tx, 236743327);
        Ok(())
    }

    #[test]
    fn parse_proc_net_dev_unknown_iface_bails() {
        let sample = "Inter-|...\nsipa_eth0: 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16\n";
        assert!(parse_proc_net_dev(sample, "wlan0").is_err());
    }

    #[test]
    fn observe_first_call_initializes_anchor() -> Result<()> {
        let path = temp_path("init");
        let state = observe_with(&path, 1000, 500, "2026-05")?;
        // 首次：rx_bytes 包含从 0 到 1000 的增量
        assert_eq!(state.period, "2026-05");
        assert_eq!(state.rx_bytes, 1000);
        assert_eq!(state.tx_bytes, 500);
        let _ = fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn observe_increment_between_calls() -> Result<()> {
        let path = temp_path("inc");
        observe_with(&path, 1000, 500, "2026-05")?;
        let s2 = observe_with(&path, 1500, 700, "2026-05")?;
        // 第二次：rx_bytes = 1000 (首次) + 500 (增量)
        assert_eq!(s2.rx_bytes, 1500);
        assert_eq!(s2.tx_bytes, 700);
        let _ = fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn observe_detects_kernel_counter_reset() -> Result<()> {
        // 模拟设备重启：current < last_anchor → 不应让 rx_bytes 倒退
        let path = temp_path("reset");
        observe_with(&path, 1000, 500, "2026-05")?;
        let s2 = observe_with(&path, 200, 100, "2026-05")?; // 重启后内核计数器从小值开始
        // rx_bytes = 1000 (首次累计) + 200 (重启后从 0 算到 200)
        assert_eq!(s2.rx_bytes, 1200);
        assert_eq!(s2.tx_bytes, 600);
        let _ = fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn observe_resets_on_month_change() -> Result<()> {
        let path = temp_path("month");
        observe_with(&path, 1000, 500, "2026-05")?;
        let s2 = observe_with(&path, 1500, 700, "2026-06")?;
        // 跨月：rx_bytes 从 0 重新算，仅含 current 值（首次进入新月份当 anchor）
        assert_eq!(s2.period, "2026-06");
        assert_eq!(s2.rx_bytes, 0);
        assert_eq!(s2.tx_bytes, 0);
        let _ = fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn observe_persists_across_load() -> Result<()> {
        let path = temp_path("persist");
        observe_with(&path, 1000, 500, "2026-05")?;
        let loaded = load_from(&path, "2026-05")?;
        assert_eq!(loaded.rx_bytes, 1000);
        assert_eq!(loaded.tx_bytes, 500);
        let _ = fs::remove_file(&path);
        Ok(())
    }

    #[test]
    fn concurrent_observe_writes_all_succeed() -> Result<()> {
        const THREADS: usize = 16;
        let path = Arc::new(temp_path("concurrent"));
        let barrier = Arc::new(Barrier::new(THREADS));
        let handles = (0..THREADS)
            .map(|_| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    observe_with(&path, 1000, 500, "2026-05")
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().expect("并发月流量线程不应 panic")?;
        }
        let loaded = load_from(&path, "2026-05")?;
        assert_eq!(loaded.period, "2026-05");
        let _ = fs::remove_file(path.as_ref());
        Ok(())
    }

    #[test]
    fn concurrent_samples_serialize_counter_read_and_persist() -> Result<()> {
        use std::{sync::mpsc, time::Duration};

        let path = Arc::new(temp_path("sample-order"));
        let (a_read_tx, a_read_rx) = mpsc::channel();
        let (release_a_tx, release_a_rx) = mpsc::channel();
        let a_path = Arc::clone(&path);
        let a = thread::spawn(move || {
            sample_with(
                &a_path,
                || "2026-05".to_owned(),
                || {
                    a_read_tx.send(()).expect("应通知 A 已读取计数器");
                    release_a_rx.recv().expect("应释放 A 继续提交");
                    Ok((1100, 550))
                },
            )
        });
        a_read_rx.recv().expect("A 应先进入计数器读取");

        let (b_started_tx, b_started_rx) = mpsc::channel();
        let (b_read_tx, b_read_rx) = mpsc::channel();
        let b_path = Arc::clone(&path);
        let b = thread::spawn(move || {
            b_started_tx.send(()).expect("应通知 B 已启动");
            sample_with(
                &b_path,
                || "2026-05".to_owned(),
                || {
                    b_read_tx.send(()).expect("应通知 B 已读取计数器");
                    Ok((1200, 600))
                },
            )
        });
        b_started_rx.recv().expect("B 应已启动");
        let b_read_while_a_pending = b_read_rx.recv_timeout(Duration::from_secs(1)).is_ok();
        release_a_tx.send(()).expect("应释放 A");

        a.join().expect("A 不应 panic")?;
        b.join().expect("B 不应 panic")?;
        assert!(!b_read_while_a_pending, "B 不应在 A 提交前读取更新的计数器");
        let loaded = load_from(&path, "2026-05")?;
        assert_eq!(loaded.rx_bytes, 1200);
        assert_eq!(loaded.tx_bytes, 600);
        let _ = fs::remove_file(path.as_ref());
        Ok(())
    }

    #[test]
    fn current_period_format_is_yyyy_mm() {
        let p = current_period();
        assert_eq!(p.len(), 7);
        assert_eq!(&p[4..5], "-");
    }

    #[test]
    fn days_to_ymd_handles_leap_years() {
        // 2020-02-29 距 1970-01-01 = 50 年 + 29 天 = 18322 天
        let (y, m, d) = days_to_ymd(18321);
        assert_eq!(y, 2020);
        assert_eq!(m, 2);
        assert_eq!(d, 29);
    }
}
