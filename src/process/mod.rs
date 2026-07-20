pub mod easytier;
pub mod mihomo;

use std::{fs, path::Path, time::Duration};

#[cfg(any(target_os = "android", target_os = "linux"))]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

#[cfg(any(target_os = "android", target_os = "linux"))]
use tokio::time;
use tracing::warn;

/// /proc/<pid>/cmdline 以 NUL 分隔，第一段为可执行路径。返回 None 表示无法判断。
#[cfg(any(test, target_os = "android", target_os = "linux"))]
pub(crate) fn cmdline_matches_binary(cmdline: &[u8], bin: &Path) -> Option<bool> {
    let first = cmdline.split(|byte| *byte == 0).next()?;
    if first.is_empty() {
        return None;
    }
    let first = std::str::from_utf8(first).ok()?;
    Some(Path::new(first) == bin)
}

/// 回收上一代 daemon 残留的子进程：daemon 被 kill -9 时子进程孤儿化，
/// 新 daemon 直接 spawn 会造成双实例抢端口（reuseport 下 UDP 流量被分流，
/// 订阅更新也只能管到新实例）。只在 /proc/<pid>/cmdline 与我们 spawn 的
/// 二进制一致时才动手，防 pid 复用误杀无辜进程。
///
pub(crate) async fn reap_stale_process(
    label: &str,
    pid_file: &Path,
    bin: &Path,
    stop_timeout: Duration,
) -> bool {
    #[cfg(not(any(target_os = "android", target_os = "linux")))]
    {
        let _ = (bin, stop_timeout);
        if !pid_file.exists() {
            return true;
        }
        warn!(label, "当前平台无法扫描 /proc，存在 PID 文件时拒绝启动新实例");
        false
    }

    #[cfg(any(target_os = "android", target_os = "linux"))]
    {
        let pids = match scan_matching_pids(Path::new("/proc"), bin) {
            Ok(pids) => pids,
            Err(err) => {
                warn!(label, error = %err, "扫描残留进程失败，拒绝启动新实例");
                return false;
            }
        };
        for pid in pids {
            let process = match PidFd::open_verified(pid, bin) {
                Ok(Some(process)) => process,
                Ok(None) => continue,
                Err(err) => {
                    warn!(pid, label, error = %err, "无法固定残留进程身份，拒绝启动新实例");
                    return false;
                }
            };
            if !reap_verified_process(label, pid, &process, stop_timeout).await {
                return false;
            }
        }
        let _ = fs::remove_file(pid_file);
        true
    }
}

#[cfg(any(target_os = "android", target_os = "linux"))]
async fn reap_verified_process(
    label: &str,
    pid: u32,
    process: &PidFd,
    stop_timeout: Duration,
) -> bool {
    warn!(pid, label, "发现上一代残留进程，回收后再启动新实例");
    if let Err(err) = process.send_signal(libc::SIGTERM) {
        if err.raw_os_error() == Some(libc::ESRCH) {
            return true;
        }
        warn!(pid, label, error = %err, "向残留进程发送 SIGTERM 失败");
        return false;
    }
    let deadline = std::time::Instant::now() + stop_timeout;
    while std::time::Instant::now() < deadline {
        match process.is_alive() {
            Ok(false) => return true,
            Ok(true) => {}
            Err(err) => {
                warn!(pid, label, error = %err, "检查残留进程 pidfd 失败");
                return false;
            }
        }
        time::sleep(Duration::from_millis(100)).await;
    }
    warn!(pid, label, "残留进程未响应 SIGTERM，发送 SIGKILL");
    match process.send_signal(libc::SIGKILL) {
        Ok(()) => {}
        Err(err) if err.raw_os_error() == Some(libc::ESRCH) => return true,
        Err(err) => {
            warn!(pid, label, error = %err, "向残留进程发送 SIGKILL 失败");
            return false;
        }
    }
    let deadline = std::time::Instant::now() + stop_timeout;
    while std::time::Instant::now() < deadline {
        match process.is_alive() {
            Ok(false) => return true,
            Ok(true) => {}
            Err(err) => {
                warn!(pid, label, error = %err, "等待残留进程强制退出失败");
                return false;
            }
        }
        time::sleep(Duration::from_millis(100)).await;
    }
    warn!(pid, label, "残留进程收到 SIGKILL 后仍未退出，保留 PID 文件并拒绝启动新实例");
    false
}

#[cfg(any(test, target_os = "android", target_os = "linux"))]
fn scan_matching_pids(proc_root: &Path, bin: &Path) -> std::io::Result<Vec<u32>> {
    let mut pids = Vec::new();
    for entry in fs::read_dir(proc_root)? {
        let entry = entry?;
        let Some(pid) = entry.file_name().to_str().and_then(parse_pid) else {
            continue;
        };
        let cmdline = match fs::read(entry.path().join("cmdline")) {
            Ok(cmdline) => cmdline,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err),
        };
        if cmdline_matches_binary(&cmdline, bin) == Some(true) {
            pids.push(pid);
        }
    }
    pids.sort_unstable();
    Ok(pids)
}

pub(crate) fn parse_pid(raw: &str) -> Option<u32> {
    let pid = raw.trim().parse::<u32>().ok()?;
    (pid > 1 && pid <= libc::pid_t::MAX as u32).then_some(pid)
}

pub(crate) struct PidFd {
    #[cfg(any(target_os = "android", target_os = "linux"))]
    fd: OwnedFd,
}

impl PidFd {
    pub(crate) fn open_verified(pid: u32, bin: &Path) -> std::io::Result<Option<Self>> {
        #[cfg(any(target_os = "android", target_os = "linux"))]
        {
            // pidfd_open 只读取 pid/flags；成功返回的 fd 由 OwnedFd 接管。
            let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0_u32) };
            if raw < 0 {
                let err = std::io::Error::last_os_error();
                return if err.raw_os_error() == Some(libc::ESRCH) {
                    Ok(None)
                } else {
                    Err(err)
                };
            }
            // SAFETY: pidfd_open 成功时返回一个新的、由调用方拥有的文件描述符。
            let process = Self {
                fd: unsafe { OwnedFd::from_raw_fd(raw as libc::c_int) },
            };
            let cmdline = match fs::read(format!("/proc/{pid}/cmdline")) {
                Ok(cmdline) => cmdline,
                Err(err) => {
                    return match process.is_alive() {
                        Ok(false) => Ok(None),
                        _ => Err(err),
                    };
                }
            };
            return match cmdline_matches_binary(&cmdline, bin) {
                Some(true) => Ok(Some(process)),
                Some(false) => Ok(None),
                None => Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("进程 cmdline 无法解析: pid={pid}"),
                )),
            };
        }

        #[cfg(not(any(target_os = "android", target_os = "linux")))]
        {
            let _ = (pid, bin);
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "当前平台不支持 pidfd",
            ))
        }
    }

    pub(crate) fn send_signal(&self, signal: libc::c_int) -> std::io::Result<()> {
        #[cfg(any(target_os = "android", target_os = "linux"))]
        {
            // pidfd_send_signal 只读取 pidfd/signal，空 siginfo 与 flags=0 等价于 kill 语义。
            let result = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.fd.as_raw_fd(),
                    signal,
                    std::ptr::null::<libc::siginfo_t>(),
                    0_u32,
                )
            };
            return if result == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            };
        }

        #[cfg(not(any(target_os = "android", target_os = "linux")))]
        {
            let _ = signal;
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "当前平台不支持 pidfd",
            ))
        }
    }

    pub(crate) fn is_alive(&self) -> std::io::Result<bool> {
        match self.send_signal(0) {
            Ok(()) => Ok(true),
            Err(err) if err.raw_os_error() == Some(libc::ESRCH) => Ok(false),
            Err(err) if err.raw_os_error() == Some(libc::EPERM) => Ok(true),
            Err(err) => Err(err),
        }
    }
}

#[cfg(test)]
mod pidfd_tests {
    #[cfg(any(target_os = "android", target_os = "linux"))]
    #[test]
    fn pidfd_pins_current_process_identity() -> std::io::Result<()> {
        let executable = std::env::current_exe()?;
        let process = super::PidFd::open_verified(std::process::id(), &executable)?
            .expect("当前测试进程应仍存活且身份匹配");
        assert!(process.is_alive()?);
        Ok(())
    }
}

/// 子进程日志大小上限；超过则在下次打开时轮转为 `<path>.1`。
/// 局限：轮转只发生在进程重启时——运行中的子进程持有旧 fd，rename 后
/// 仍写旧 inode，无法在线截断。mihomo 会随订阅更新/watchdog 周期性重启，
/// 单次运行增长可接受。
pub(crate) const LOG_MAX_SIZE: u64 = 5 * 1024 * 1024;

/// 打开子进程日志文件（append）。文件超过 max_size 时先轮转为 `<path>.1`
/// （覆盖旧 .1），保证磁盘占用有上界（约 2×max_size）。
pub(crate) fn open_rotated_log(path: &Path, max_size: u64) -> std::io::Result<fs::File> {
    if let Ok(meta) = fs::metadata(path) {
        if meta.len() > max_size {
            let rotated = path.with_extension("log.1");
            fs::rename(path, &rotated)?;
        }
    }
    fs::OpenOptions::new().append(true).create(true).open(path)
}

/// 运行中日志超限则原地截断（丢历史保上界）。子进程持有 O_APPEND fd，
/// truncate 后继续从新 EOF 写入，安全。rename 式轮转对运行中的写者无效
/// （fd 跟随 inode，.1 会继续膨胀），只能截断。
pub(crate) fn truncate_oversized_log(path: &Path, max_size: u64) {
    let oversized = fs::metadata(path).map(|m| m.len() > max_size).unwrap_or(false);
    if !oversized {
        return;
    }
    match fs::OpenOptions::new().write(true).open(path) {
        Ok(file) => {
            if file.set_len(0).is_ok() {
                warn!(path = %path.display(), "运行中日志超限，已原地截断");
            }
        }
        Err(err) => warn!(path = %path.display(), error = %err, "截断超限日志失败"),
    }
}

/// 打开轮转日志并复制出 stdout/stderr 两个 Stdio 句柄；
/// 失败时退回 inherit（日志问题不得阻断子进程启动）。
pub(crate) fn log_stdio_or_inherit(path: &Path) -> (std::process::Stdio, std::process::Stdio) {
    let opened = open_rotated_log(path, LOG_MAX_SIZE).and_then(|file| {
        let clone = file.try_clone()?;
        Ok((file.into(), clone.into()))
    });
    match opened {
        Ok(pair) => pair,
        Err(err) => {
            warn!(path = %path.display(), error = %err, "打开子进程日志失败，退回 stdout");
            (
                std::process::Stdio::inherit(),
                std::process::Stdio::inherit(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_log_rotates_to_dot_one() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!("f50mgr-rotate-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        let log = dir.join("mihomo.log");
        fs::write(&log, vec![b'x'; 1024])?;

        let file = open_rotated_log(&log, 512)?; // 超限 → 轮转
        drop(file);
        assert_eq!(
            fs::read(dir.join("mihomo.log.1"))?.len(),
            1024,
            "旧内容应轮转到 .1"
        );
        assert_eq!(fs::metadata(&log)?.len(), 0, "新日志应为空文件");

        // 未超限 → 原样追加，不轮转
        fs::write(&log, b"abc")?;
        let file = open_rotated_log(&log, 512)?;
        drop(file);
        assert_eq!(fs::metadata(&log)?.len(), 3, "未超限不得清空");

        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn oversized_log_truncates_in_place() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!("f50mgr-trunc-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        let log = dir.join("mihomo.log");
        fs::write(&log, vec![b'x'; 1024])?;

        truncate_oversized_log(&log, 512); // 超限 → 原地清空
        assert_eq!(fs::metadata(&log)?.len(), 0);

        fs::write(&log, b"abc")?;
        truncate_oversized_log(&log, 512); // 未超限 → 不动
        assert_eq!(fs::metadata(&log)?.len(), 3);

        truncate_oversized_log(&dir.join("missing.log"), 512); // 不存在 → 不 panic
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    #[test]
    fn cmdline_first_arg_matches_expected_binary() {
        // /proc/<pid>/cmdline 以 NUL 分隔；第一段必须与我们 spawn 的二进制路径一致
        let bin = Path::new("/data/adb/f50mgr/bin/mihomo");
        assert_eq!(
            cmdline_matches_binary(b"/data/adb/f50mgr/bin/mihomo\0-d\0/x\0", bin),
            Some(true)
        );
        assert_eq!(
            cmdline_matches_binary(b"/system/bin/toybox\0sleep\x000030\0", bin),
            Some(false)
        );
        assert_eq!(cmdline_matches_binary(b"", bin), None);
    }

    #[test]
    fn pid_parser_rejects_corruption_and_signal_special_values() {
        assert_eq!(parse_pid("2236\n"), Some(2236));
        for invalid in ["", "\0\0\0\0\0", "0", "1", "2147483648", "4294967295"] {
            assert_eq!(
                parse_pid(invalid),
                None,
                "必须拒绝特殊或损坏 PID: {invalid:?}"
            );
        }
    }

    #[test]
    fn proc_scan_finds_orphan_when_pid_file_is_missing() -> std::io::Result<()> {
        let root = std::env::temp_dir().join(format!("f50mgr-proc-scan-test-{}", std::process::id()));
        let matching = root.join("321");
        let unrelated = root.join("322");
        fs::create_dir_all(&matching)?;
        fs::create_dir_all(&unrelated)?;
        let bin = Path::new("/data/adb/f50mgr/bin/mihomo");
        fs::write(matching.join("cmdline"), b"/data/adb/f50mgr/bin/mihomo\0-d\0")?;
        fs::write(unrelated.join("cmdline"), b"/system/bin/sleep\0arg\0")?;

        let found = scan_matching_pids(&root, bin)?;

        assert_eq!(found, vec![321], "缺少 PID 文件时仍必须发现孤儿进程");
        fs::remove_dir_all(&root)?;
        Ok(())
    }
}
