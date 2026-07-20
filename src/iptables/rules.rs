use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

use anyhow::{bail, Context, Result};

const MARK: &str = "0x1000000/0x1000000";
const TABLE_ID: &str = "2024";
const TPROXY_PORT: &str = "14198";
const DNS_PORT: &str = "14153";
const EXTERNAL_CHAIN: &str = "F50MGR_EXTERNAL";
const LOCAL_CHAIN: &str = "F50MGR_LOCAL";
const LOCAL_IP_CHAIN: &str = "F50MGR_LOCAL_IP_V4";
const DIVERT_CHAIN: &str = "F50MGR_DIVERT";
const DNS_EXTERNAL_CHAIN: &str = "F50MGR_DNS_EXTERNAL";
const DNS_LOCAL_CHAIN: &str = "F50MGR_DNS_LOCAL";
const ET_NAT_CHAIN: &str = "F50MGR_ET_NAT";
const ET_FWD_CHAIN: &str = "F50MGR_ET_FWD";
const ET_TUN_DEV: &str = "tun_et"; // 与 src/subscription/easytier.rs::TUN_DEV_NAME 同步
const LAN_IFACES: [&str; 7] = ["br+", "ap+", "wlan+", "rndis+", "swlan+", "ncm+", "usb+"];
const TPROXY_IFACES: [&str; 8] = ["lo", "ap+", "wlan+", "rndis+", "swlan+", "ncm+", "eth+", "br+"];
const LAN_BIND_IP: &str = "192.168.0.1";
const MANGLE_CHAINS: [&str; 4] = [EXTERNAL_CHAIN, LOCAL_CHAIN, LOCAL_IP_CHAIN, DIVERT_CHAIN];
const NAT_CHAINS: [&str; 3] = [DNS_EXTERNAL_CHAIN, DNS_LOCAL_CHAIN, ET_NAT_CHAIN];
const FILTER_CHAINS: [&str; 1] = [ET_FWD_CHAIN];

#[derive(Debug, Clone)]
struct Tool {
    path: &'static str,
    applet: Option<&'static str>,
}

#[derive(Debug, Clone)]
struct Tools {
    iptables: Tool,
    iptables_save: Tool,
    iptables_restore: Tool,
    ip: Tool,
}

pub fn enable() -> Result<()> {
    let tools = Tools::probe()?;
    let mangle_backup = output(&tools.iptables_save, &["-t", "mangle"])?;
    let nat_backup = output(&tools.iptables_save, &["-t", "nat"])?;
    let filter_backup = output(&tools.iptables_save, &["-t", "filter"])?;

    if let Err(err) = enable_inner(&tools) {
        let _ = restore(&tools, &mangle_backup);
        let _ = restore(&tools, &nat_backup);
        let _ = restore(&tools, &filter_backup);
        let _ = remove_policy_route(&tools);
        return Err(err).context("启用 iptables 失败，已尝试回滚");
    }

    Ok(())
}

pub fn disable() -> Result<()> {
    let tools = Tools::probe()?;
    disable_inner(&tools);
    if is_active_with_tools(&tools) {
        bail!("禁用 iptables 后仍检测到 F50MGR 规则 active");
    }
    Ok(())
}

pub fn is_active() -> Result<bool> {
    let tools = Tools::probe()?;
    Ok(is_active_with_tools(&tools))
}

/// is_active 判定所需的探测结果。
struct ActiveProbe {
    /// mangle PREROUTING -j F50MGR_EXTERNAL（TPROXY 入口）
    jump_external: bool,
    /// nat PREROUTING udp/53 -j F50MGR_DNS_EXTERNAL（LAN DNS 重定向入口）
    jump_dns: bool,
    /// nat POSTROUTING -j F50MGR_ET_NAT（mesh 出向 NAT 入口）
    jump_et_nat: bool,
    /// filter FORWARD -j F50MGR_ET_FWD（覆盖 tetherctrl catch-all DROP）
    jump_et_fwd: bool,
    external: String,
    dns: String,
    ip_rules: String,
}

fn is_active_with_tools(tools: &Tools) -> bool {
    // 内置链上的跳转必须用 `iptables -C` 单独校验：ZTE NetworkManagement 开机初始化
    // 只冲掉内置链（PREROUTING 等）上的跳转，自定义链及其内容原样保留（0 references）。
    // 只查链内容会把这种失效状态误判为 active → watchdog 永不恢复 → 上电后持久无网络。
    let probe = ActiveProbe {
        jump_external: rule_present(
            tools,
            &["-t", "mangle", "-C", "PREROUTING", "-j", EXTERNAL_CHAIN],
        ),
        jump_dns: rule_present(
            tools,
            &[
                "-t", "nat", "-C", "PREROUTING", "-p", "udp", "--dport", "53", "-j",
                DNS_EXTERNAL_CHAIN,
            ],
        ),
        jump_et_nat: rule_present(tools, &["-t", "nat", "-C", "POSTROUTING", "-j", ET_NAT_CHAIN]),
        jump_et_fwd: rule_present(tools, &["-t", "filter", "-C", "FORWARD", "-j", ET_FWD_CHAIN]),
        external: output_or_empty(&tools.iptables, &["-t", "mangle", "-S", EXTERNAL_CHAIN]),
        dns: output_or_empty(&tools.iptables, &["-t", "nat", "-S", DNS_EXTERNAL_CHAIN]),
        ip_rules: output_or_empty(&tools.ip, &["rule", "show"]),
    };
    is_active_eval(&probe)
}

fn is_active_eval(probe: &ActiveProbe) -> bool {
    // 注意：iptables -S 会把 TPROXY 选项重排为 --on-port 在前、--on-ip 在后，
    // 故必须分别匹配两个 token，不能拼成固定顺序的子串，否则 is_active 永远 false →
    // watchdog 每 30s 误判规则丢失 → enable() 先 disable 再重建 → 周期性 TPROXY 中断。
    probe.jump_external
        && probe.jump_dns
        && probe.jump_et_nat
        && probe.jump_et_fwd
        && probe.external.contains("-i br+")
        && probe.external.contains(&format!("--on-ip {LAN_BIND_IP}"))
        && probe.external.contains(&format!("--on-port {TPROXY_PORT}"))
        && probe.dns.contains(&format!("REDIRECT --to-ports {DNS_PORT}"))
        && probe.ip_rules.contains("fwmark 0x1000000")
        && probe.ip_rules.contains("lookup 2024")
}

fn rule_present(tools: &Tools, args: &[&str]) -> bool {
    run_ignore(&tools.iptables, args)
}

/// 渲染 mangle/nat 规则集。
///
/// **EasyTier 共存性约束**（本规则集必须始终满足）：
/// 1. EXTERNAL_CHAIN 顶部保留 `-d 10.0.0.0/8 -j RETURN`，让 EasyTier 默认子网流量放行到 main 表
/// 2. LOCAL_CHAIN（mangle OUTPUT）保持空规则，F50 本机进程出向到 mesh peer 不被 TPROXY 接管
/// 3. TPROXY 规则只匹配热点接口（br+/wlan+/ap+/eth+/...），不含 EasyTier TUN 接口
/// 4. policy route 用 `pref 100`，优先于 EasyTier 启动时添加的 `from all lookup main`（pref 32766）
///
/// 修改本函数前必看 `coexists_with_easytier_*` 测试断言。
pub fn restore_rules() -> String {
    let mut lines = vec![
        "*mangle".to_string(),
        format!("-A PREROUTING -j {EXTERNAL_CHAIN}"),
        format!("-A OUTPUT -j {LOCAL_CHAIN}"),
        format!("-A {DIVERT_CHAIN} -j MARK --set-mark {MARK}"),
        format!("-A {DIVERT_CHAIN} -j ACCEPT"),
    ];
    // DIVERT 快速路径限定 LAN 接口：若不加 -i，Mihomo 经 sipa_eth0 收到代理服务器回包时
    // -m socket 会命中 → mark 0x1000000 → lo policy route → Mihomo 自身连接断裂
    for iface in TPROXY_IFACES {
        lines.push(format!(
            "-A {EXTERNAL_CHAIN} -p tcp -i {iface} -m socket -j {DIVERT_CHAIN}"
        ));
    }
    lines.extend([
        format!("-A {EXTERNAL_CHAIN} -p tcp --dport 53 -j RETURN"),
        format!("-A {EXTERNAL_CHAIN} -p udp --dport 53 -j RETURN"),
    ]);

    for cidr in [
        "10.0.0.0/8",
        "127.0.0.0/8",
        "169.254.0.0/16",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "224.0.0.0/4",
        "240.0.0.0/4",
    ] {
        lines.push(format!("-A {EXTERNAL_CHAIN} -d {cidr} -j RETURN"));
    }

    lines.push(format!("-A {EXTERNAL_CHAIN} -j {LOCAL_IP_CHAIN}"));
    for iface in TPROXY_IFACES {
        lines.push(tproxy_rule("tcp", iface));
        lines.push(tproxy_rule("udp", iface));
    }

    lines.extend([
        "COMMIT".to_string(),
        "*nat".to_string(),
        format!("-A PREROUTING -p udp --dport 53 -j {DNS_EXTERNAL_CHAIN}"),
        format!("-A {DNS_EXTERNAL_CHAIN} -p udp -j REDIRECT --to-ports {DNS_PORT}"),
        // EasyTier mesh 出向 NAT：把热点客户端 src（192.168.x.x）改写成 EasyTier TUN 接口 IP
        // （10.0.5.x），让 mesh 对端能正确回包。仅匹配 -o tun_et，不影响其他出口。
        format!("-A POSTROUTING -j {ET_NAT_CHAIN}"),
        format!("-A {ET_NAT_CHAIN} -o {ET_TUN_DEV} -j MASQUERADE"),
        "COMMIT".to_string(),
    ]);

    // *filter：覆盖 Android tetherctrl_FORWARD 末尾的 catch-all DROP，
    // 让 LAN 客户端 ↔ EasyTier mesh 双向 forward 不被拦截。
    lines.push("*filter".to_string());
    // FORWARD 第一条跳到自定义链（restore_noflush 不动 FORWARD policy / 既有规则）
    lines.push(format!("-I FORWARD 1 -j {ET_FWD_CHAIN}"));
    for iface in LAN_IFACES {
        lines.push(format!(
            "-A {ET_FWD_CHAIN} -i {iface} -o {ET_TUN_DEV} -m conntrack --ctstate NEW,ESTABLISHED,RELATED -j ACCEPT"
        ));
        lines.push(format!(
            "-A {ET_FWD_CHAIN} -i {ET_TUN_DEV} -o {iface} -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT"
        ));
    }
    lines.push("COMMIT".to_string());

    format!("{}\n", lines.join("\n"))
}

fn enable_inner(tools: &Tools) -> Result<()> {
    disable_inner(tools);
    ensure_chains(tools)?;
    restore_noflush(tools, &restore_rules())?;
    configure_policy_route(tools)
}

fn disable_inner(tools: &Tools) {
    delete_jump(tools, "mangle", "PREROUTING", &["-j", EXTERNAL_CHAIN]);
    delete_jump(tools, "mangle", "OUTPUT", &["-j", LOCAL_CHAIN]);
    delete_jump(
        tools,
        "nat",
        "PREROUTING",
        &["-p", "udp", "--dport", "53", "-j", DNS_EXTERNAL_CHAIN],
    );
    delete_jump(
        tools,
        "nat",
        "OUTPUT",
        &["-p", "udp", "--dport", "53", "-j", DNS_LOCAL_CHAIN],
    );
    delete_jump(tools, "nat", "POSTROUTING", &["-j", ET_NAT_CHAIN]);
    delete_jump(tools, "filter", "FORWARD", &["-j", ET_FWD_CHAIN]);

    flush_delete_chains(tools, "mangle", &MANGLE_CHAINS);
    flush_delete_chains(tools, "nat", &NAT_CHAINS);
    flush_delete_chains(tools, "filter", &FILTER_CHAINS);

    let _ = remove_policy_route(tools);
}

fn ensure_chains(tools: &Tools) -> Result<()> {
    create_flush_chains(tools, "mangle", &MANGLE_CHAINS)?;
    create_flush_chains(tools, "nat", &NAT_CHAINS)?;
    create_flush_chains(tools, "filter", &FILTER_CHAINS)?;
    Ok(())
}

fn create_flush_chains(tools: &Tools, table: &str, chains: &[&str]) -> Result<()> {
    for chain in chains {
        run_ignore(&tools.iptables, &["-t", table, "-N", chain]);
        run(&tools.iptables, &["-t", table, "-F", chain])?;
    }
    Ok(())
}

fn flush_delete_chains(tools: &Tools, table: &str, chains: &[&str]) {
    for chain in chains {
        run_ignore(&tools.iptables, &["-t", table, "-F", chain]);
    }
    for chain in chains.iter().rev() {
        run_ignore(&tools.iptables, &["-t", table, "-X", chain]);
    }
}

fn configure_policy_route(tools: &Tools) -> Result<()> {
    let _ = remove_policy_route(tools);
    run(&tools.ip, &["route", "flush", "table", TABLE_ID])?;
    run(
        &tools.ip,
        &[
            "route", "add", "local", "default", "dev", "lo", "table", TABLE_ID,
        ],
    )?;
    run(
        &tools.ip,
        &[
            "rule", "add", "fwmark", MARK, "lookup", TABLE_ID, "pref", "100",
        ],
    )
}

fn remove_policy_route(tools: &Tools) -> Result<()> {
    for _ in 0..8 {
        if !run_ignore(
            &tools.ip,
            &["rule", "del", "fwmark", MARK, "lookup", TABLE_ID],
        ) {
            break;
        }
    }
    run_ignore(&tools.ip, &["route", "flush", "table", TABLE_ID]);
    Ok(())
}

fn delete_jump(tools: &Tools, table: &str, chain: &str, rule: &[&str]) {
    for _ in 0..16 {
        let mut args = vec!["-t", table, "-D", chain];
        args.extend_from_slice(rule);
        if !run_ignore(&tools.iptables, &args) {
            break;
        }
    }
}

fn tproxy_rule(protocol: &str, iface: &str) -> String {
    format!(
        "-A {EXTERNAL_CHAIN} -p {protocol} -i {iface} -j TPROXY --on-ip {LAN_BIND_IP} --on-port {TPROXY_PORT} --tproxy-mark {MARK}"
    )
}

fn restore_noflush(tools: &Tools, rules: &str) -> Result<()> {
    restore_with_args(tools, &["--noflush"], rules)
}

fn restore(tools: &Tools, rules: &str) -> Result<()> {
    restore_with_args(tools, &[], rules)
}

fn restore_with_args(tools: &Tools, args: &[&str], rules: &str) -> Result<()> {
    let mut command = tools.iptables_restore.command();
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().context("启动 iptables-restore 失败")?;
    let mut stdin = child
        .stdin
        .take()
        .context("打开 iptables-restore stdin 失败")?;
    stdin
        .write_all(rules.as_bytes())
        .context("写入 iptables-restore 规则失败")?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .context("等待 iptables-restore 失败")?;
    ensure_success(&tools.iptables_restore, args, output).map(|_| ())
}

fn output(tool: &Tool, args: &[&str]) -> Result<String> {
    let output = tool
        .command()
        .args(args)
        .output()
        .with_context(|| format!("执行命令失败: {} {}", tool.display(), args.join(" ")))?;
    ensure_success(tool, args, output)
}

fn output_or_empty(tool: &Tool, args: &[&str]) -> String {
    output(tool, args).unwrap_or_default()
}

fn run(tool: &Tool, args: &[&str]) -> Result<()> {
    output(tool, args).map(|_| ())
}

fn run_ignore(tool: &Tool, args: &[&str]) -> bool {
    tool.command()
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn ensure_success(tool: &Tool, args: &[&str], output: std::process::Output) -> Result<String> {
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    bail!(
        "命令失败: {} {}\nstdout: {}\nstderr: {}",
        tool.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
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
        if Path::new("/system/bin/iptables").exists() {
            return Ok(Self {
                iptables: Tool::plain("/system/bin/iptables"),
                iptables_save: Tool::plain("/system/bin/iptables-save"),
                iptables_restore: Tool::plain("/system/bin/iptables-restore"),
                ip: Tool::plain("/system/bin/ip"),
            });
        }

        for path in ["/system/xbin/busybox", "/system/bin/busybox"] {
            if Path::new(path).exists() {
                return Ok(Self {
                    iptables: Tool::applet(path, "iptables"),
                    iptables_save: Tool::applet(path, "iptables-save"),
                    iptables_restore: Tool::applet(path, "iptables-restore"),
                    ip: Tool::applet(path, "ip"),
                });
            }
        }

        bail!("未找到 iptables，已检查 /system/bin/iptables 和 busybox");
    }
}

#[cfg(test)]
mod tests {
    use super::{is_active_eval, restore_rules, ActiveProbe};

    /// 规则完整存活时的探测结果（内容取自真机 iptables -S 回读，选项顺序已被重排）。
    fn full_probe() -> ActiveProbe {
        ActiveProbe {
            jump_external: true,
            jump_dns: true,
            jump_et_nat: true,
            jump_et_fwd: true,
            // -S 回读会把 --on-port 排在 --on-ip 前面（历史 bug：拼固定顺序子串永远 false）
            external: "-A F50MGR_EXTERNAL -i br+ -p tcp -j TPROXY --on-port 14198 \
                       --on-ip 192.168.0.1 --tproxy-mark 0x1000000/0x1000000\n"
                .to_string(),
            dns: "-A F50MGR_DNS_EXTERNAL -p udp -j REDIRECT --to-ports 14153\n".to_string(),
            ip_rules: "100:\tfrom all fwmark 0x1000000/0x1000000 lookup 2024\n".to_string(),
        }
    }

    #[test]
    fn is_active_true_when_jumps_and_content_present() {
        assert!(is_active_eval(&full_probe()));
    }

    #[test]
    fn is_active_false_when_prerouting_jump_missing() {
        // ZTE NetworkManagement 开机场景：只冲掉 PREROUTING 跳转，自定义链内容原样保留。
        // 该状态必须判 inactive，否则 watchdog 永不恢复 → 上电后持久无网络。
        let probe = ActiveProbe {
            jump_external: false,
            ..full_probe()
        };
        assert!(!is_active_eval(&probe));
    }

    #[test]
    fn is_active_false_when_dns_jump_missing() {
        let probe = ActiveProbe {
            jump_dns: false,
            ..full_probe()
        };
        assert!(!is_active_eval(&probe));
    }

    #[test]
    fn is_active_false_when_forward_jump_missing() {
        let probe = ActiveProbe {
            jump_et_fwd: false,
            ..full_probe()
        };
        assert!(!is_active_eval(&probe));
    }

    #[test]
    fn is_active_tolerates_tproxy_option_reorder() {
        // full_probe 的 external 已模拟 --on-port 在前的重排；只要 token 齐全就必须判 active。
        let probe = full_probe();
        assert!(probe.external.find("--on-port").unwrap() < probe.external.find("--on-ip").unwrap());
        assert!(is_active_eval(&probe));
    }

    #[test]
    fn rules_include_f50_bridge_and_dns_redirect() {
        let rules = restore_rules();
        assert!(rules.contains("-i br+ -j TPROXY --on-ip 192.168.0.1 --on-port 14198"));
        assert!(rules.contains("F50MGR_DNS_EXTERNAL -p udp -j REDIRECT --to-ports 14153"));
        assert!(!rules.contains("-A OUTPUT -p udp --dport 53 -j F50MGR_DNS_LOCAL"));
        assert!(rules.contains("-A PREROUTING -j F50MGR_EXTERNAL"));
    }

    #[test]
    fn coexists_with_easytier_private_subnet_return() {
        // EasyTier 默认子网 10.x，F50 自身 ipv4 例如 10.0.5.3。
        // EXTERNAL_CHAIN 必须有 10.0.0.0/8 RETURN 让 mesh 流量天然放行。
        let rules = restore_rules();
        assert!(rules.contains(&format!("-A {} -d 10.0.0.0/8 -j RETURN", "F50MGR_EXTERNAL")));
    }

    #[test]
    fn coexists_with_easytier_no_tproxy_on_tun() {
        // TPROXY 不能匹配 EasyTier TUN（tun*、tun_et 等），否则 F50 自身的 mesh 流量会被吃掉。
        let rules = restore_rules();
        for line in rules.lines() {
            if !line.contains("TPROXY") {
                continue;
            }
            assert!(
                !line.contains(" -i tun"),
                "TPROXY 不应匹配 tun 接口: {line}"
            );
        }
    }

    #[test]
    fn coexists_with_easytier_local_chain_empty() {
        // LOCAL_CHAIN 必须保持空规则集（仅创建链 + OUTPUT 跳转），
        // 否则 F50 本机进程到 mesh peer 的出向会被 mark/TPROXY 接管。
        let rules = restore_rules();
        let local_jump = format!("-A OUTPUT -j {}", "F50MGR_LOCAL");
        assert!(rules.contains(&local_jump), "OUTPUT 必须跳到 LOCAL_CHAIN");
        // LOCAL_CHAIN 内不应有任何 -A 规则
        let local_rule_prefix = format!("-A {}", "F50MGR_LOCAL ");
        let count = rules.matches(local_rule_prefix.as_str()).count();
        assert_eq!(count, 0, "LOCAL_CHAIN 必须为空，当前匹配 {count} 条规则");
    }

    #[test]
    fn easytier_lan_access_forward_accept_for_all_lan_ifaces() {
        // Android tetherctrl_FORWARD 末尾 catch-all DROP，必须在 FORWARD 顶部跳到 F50MGR_ET_FWD
        // 让所有 LAN 接口 ↔ tun_et 双向 forward 不被拦截。
        let rules = restore_rules();
        assert!(rules.contains("*filter"));
        assert!(rules.contains("-I FORWARD 1 -j F50MGR_ET_FWD"));
        for iface in ["br+", "ap+", "wlan+", "rndis+", "swlan+", "ncm+", "usb+"] {
            assert!(
                rules.contains(&format!(
                    "-A F50MGR_ET_FWD -i {iface} -o tun_et -m conntrack --ctstate NEW,ESTABLISHED,RELATED -j ACCEPT"
                )),
                "缺少 {iface} → tun_et ACCEPT"
            );
            assert!(
                rules.contains(&format!(
                    "-A F50MGR_ET_FWD -i tun_et -o {iface} -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT"
                )),
                "缺少 tun_et → {iface} ACCEPT"
            );
        }
    }

    #[test]
    fn easytier_lan_access_masquerade_on_tun_et() {
        // 让热点客户端能访问 mesh：nat POSTROUTING -o tun_et -j MASQUERADE
        let rules = restore_rules();
        assert!(rules.contains("-A POSTROUTING -j F50MGR_ET_NAT"));
        assert!(rules.contains("-A F50MGR_ET_NAT -o tun_et -j MASQUERADE"));
        // MASQUERADE 必须仅限 tun_et 接口，避免把其他出口的源地址也改写
        for line in rules.lines() {
            if !line.contains("MASQUERADE") {
                continue;
            }
            assert!(
                line.contains("-o tun_et"),
                "MASQUERADE 必须限定 -o tun_et: {line}"
            );
        }
    }

    #[test]
    fn divert_chain_scoped_to_lan_ifaces() {
        // 无接口限定时，Mihomo 经 sipa_eth0 收到代理服务器回包时 -m socket 会命中
        // → mark 0x1000000 → lo policy route → Mihomo 自身连接断裂（WiFi 客户端无网）
        let rules = restore_rules();
        for line in rules.lines() {
            if !line.contains(&format!("-j {}", "F50MGR_DIVERT")) {
                continue;
            }
            assert!(
                line.contains(" -i "),
                "DIVERT 规则缺少 -i 接口限定（防 sipa_eth0 泄漏）: {line}"
            );
        }
    }

    #[test]
    fn tproxy_has_explicit_on_ip() {
        // 显式 --on-ip 192.168.0.1 防止内核 fallback 到非 Mihomo 绑定的接口 IP
        let rules = restore_rules();
        for line in rules.lines() {
            if !line.contains("TPROXY") {
                continue;
            }
            assert!(
                line.contains("--on-ip 192.168.0.1"),
                "TPROXY 规则缺少 --on-ip 192.168.0.1: {line}"
            );
        }
    }
}
