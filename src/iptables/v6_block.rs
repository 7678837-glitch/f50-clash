//! 阻断热点客户端的 IPv6 forward，避免 IPv6 leak（IPv4-only TPROXY 的副作用）。
//!
//! 设计要点：
//! - 用 REJECT (icmp6-adm-prohibited) 而不是 DROP：浏览器 Happy Eyeballs 算法会优先尝试 v6，
//!   DROP 让客户端等 ~30s timeout 才 fallback v4（用户感觉"打不开网页"）；
//!   REJECT 立刻给 ICMPv6 不可达，客户端毫秒级 fallback 到 v4。
//! - 仅 REJECT 从 LAN 接口（br+/ap+/wlan+/rndis+/swlan+/ncm+/usb+）forward 出去的 IPv6 包
//! - **不影响** F50 自身的 IPv6 出向（lo/sipa_eth0/eth0），所以 EasyTier mesh 的 IPv6 连接照常
//! - **不影响** ip6tables INPUT，所以 EasyTier listeners `tcp://[::]:14010` 入向连接照常
//! - 用 `F50MGR_V6_FWD` 自定义链承载，不污染系统已有的 oem_fwd / fw_FORWARD 等链

use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};

const CHAIN: &str = "F50MGR_V6_FWD";
const LAN_IFACES: [&str; 7] = ["br+", "ap+", "wlan+", "rndis+", "swlan+", "ncm+", "usb+"];

#[derive(Debug, Clone)]
struct Tools {
    ip6tables: Tool,
    ip6tables_restore: Tool,
}

#[derive(Debug, Clone)]
struct Tool {
    path: &'static str,
    applet: Option<&'static str>,
}

pub fn enable() -> Result<()> {
    let tools = Tools::probe()?;
    disable_inner(&tools); // 幂等：先清理旧链
    create_chain(&tools)?;
    restore_rules(&tools, &restore_payload())?;
    install_jump(&tools)
}

pub fn disable() -> Result<()> {
    let tools = Tools::probe()?;
    disable_inner(&tools);
    if is_active_with(&tools) {
        bail!("禁用 IPv6 block 后仍检测到 F50MGR_V6_FWD 规则 active");
    }
    Ok(())
}

pub fn is_active() -> Result<bool> {
    let tools = Tools::probe()?;
    Ok(is_active_with(&tools))
}

pub(crate) fn restore_payload() -> String {
    let mut lines = vec!["*filter".to_string()];
    for iface in LAN_IFACES {
        // REJECT 而非 DROP：让浏览器 Happy Eyeballs 立即 fallback 到 v4，避免 30s 超时
        lines.push(format!(
            "-A {CHAIN} -i {iface} -j REJECT --reject-with icmp6-adm-prohibited"
        ));
    }
    lines.push("COMMIT".to_string());
    format!("{}\n", lines.join("\n"))
}

fn is_active_with(tools: &Tools) -> bool {
    let listed = output_or_empty(&tools.ip6tables, &["-S", CHAIN]);
    listed.contains(&format!("-A {CHAIN} -i br+ -j REJECT"))
        && rule_present(tools, &["-C", "FORWARD", "-j", CHAIN])
}

fn disable_inner(tools: &Tools) {
    // 反复删 FORWARD 跳转直到失败（清理重复）
    for _ in 0..16 {
        if !run_ignore(&tools.ip6tables, &["-D", "FORWARD", "-j", CHAIN]) {
            break;
        }
    }
    let _ = run_ignore(&tools.ip6tables, &["-F", CHAIN]);
    let _ = run_ignore(&tools.ip6tables, &["-X", CHAIN]);
}

fn create_chain(tools: &Tools) -> Result<()> {
    run_ignore(&tools.ip6tables, &["-N", CHAIN]);
    run(&tools.ip6tables, &["-F", CHAIN])
}

fn install_jump(tools: &Tools) -> Result<()> {
    if rule_present(tools, &["-C", "FORWARD", "-j", CHAIN]) {
        return Ok(());
    }
    run(&tools.ip6tables, &["-I", "FORWARD", "1", "-j", CHAIN])
}

fn rule_present(tools: &Tools, args: &[&str]) -> bool {
    tools
        .ip6tables
        .command()
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn restore_rules(tools: &Tools, rules: &str) -> Result<()> {
    let mut command = tools.ip6tables_restore.command();
    command
        .arg("--noflush")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().context("启动 ip6tables-restore 失败")?;
    let mut stdin = child
        .stdin
        .take()
        .context("打开 ip6tables-restore stdin 失败")?;
    stdin
        .write_all(rules.as_bytes())
        .context("写入 ip6tables-restore 规则失败")?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .context("等待 ip6tables-restore 失败")?;
    if !output.status.success() {
        bail!(
            "ip6tables-restore 失败\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn output_or_empty(tool: &Tool, args: &[&str]) -> String {
    tool.command()
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn run(tool: &Tool, args: &[&str]) -> Result<()> {
    let output = tool
        .command()
        .args(args)
        .output()
        .with_context(|| format!("执行命令失败: {} {}", tool.display(), args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "命令失败: {} {}\nstdout: {}\nstderr: {}",
            tool.display(),
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

fn run_ignore(tool: &Tool, args: &[&str]) -> bool {
    tool.command()
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

impl Tool {
    const fn plain(path: &'static str) -> Self {
        Self { path, applet: None }
    }

    const fn applet(path: &'static str, applet: &'static str) -> Self {
        Self {
            path,
            applet: Some(applet),
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(self.path);
        if let Some(applet) = self.applet {
            command.arg(applet);
        }
        command
    }

    fn display(&self) -> String {
        match self.applet {
            Some(applet) => format!("{} {}", self.path, applet),
            None => self.path.to_string(),
        }
    }
}

impl Tools {
    fn probe() -> Result<Self> {
        if Path::new("/system/bin/ip6tables").exists() {
            return Ok(Self {
                ip6tables: Tool::plain("/system/bin/ip6tables"),
                ip6tables_restore: Tool::plain("/system/bin/ip6tables-restore"),
            });
        }
        for path in ["/system/xbin/busybox", "/system/bin/busybox"] {
            if Path::new(path).exists() {
                return Ok(Self {
                    ip6tables: Tool::applet(path, "ip6tables"),
                    ip6tables_restore: Tool::applet(path, "ip6tables-restore"),
                });
            }
        }
        bail!("未找到 ip6tables，已检查 /system/bin/ip6tables 和 busybox");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_payload_rejects_all_lan_ifaces_with_icmp6() {
        // REJECT 优于 DROP：浏览器 Happy Eyeballs 立即 fallback v4
        let payload = restore_payload();
        for iface in LAN_IFACES {
            assert!(
                payload.contains(&format!(
                    "-A {CHAIN} -i {iface} -j REJECT --reject-with icmp6-adm-prohibited"
                )),
                "缺少 {iface} 的 REJECT 规则（必须 icmp6-adm-prohibited）"
            );
            // 不能再用 DROP（性能差）
            assert!(
                !payload.contains(&format!("-A {CHAIN} -i {iface} -j DROP")),
                "{iface} 仍然用 DROP，应改为 REJECT"
            );
        }
        assert!(payload.starts_with("*filter\n"));
        assert!(payload.trim_end().ends_with("COMMIT"));
    }

    #[test]
    fn restore_payload_does_not_touch_other_directions() {
        // 只在 filter 表加 DROP，不动 mangle/nat；不动 INPUT/OUTPUT，避免影响 F50 自身 v6 与 EasyTier listeners
        let payload = restore_payload();
        assert!(!payload.contains("*mangle"));
        assert!(!payload.contains("*nat"));
        assert!(!payload.contains("INPUT"));
        assert!(!payload.contains("OUTPUT"));
    }

    #[test]
    fn restore_payload_uses_forward_only_chain() {
        // 自定义链不能加跳转到 INPUT/OUTPUT
        let payload = restore_payload();
        for line in payload.lines() {
            if !line.starts_with("-A") {
                continue;
            }
            assert!(
                line.contains(&format!("-A {CHAIN}")),
                "意外规则不在 {CHAIN} 链上: {line}"
            );
        }
    }
}
