use std::{
    fs,
    io::ErrorKind,
    path::Path,
    process::Command,
};

use anyhow::{Context, Result};
use tracing::{debug, warn};

const CACHE_FILE: &str = "/data/adb/f50mgr/data/device_id.txt";

const IMEI_PROPS: [&str; 8] = [
    "persist.radio.imei",
    "persist.vendor.radio.imei",
    "ril.gsm.imei",
    "ril.gsm.imei0",
    "ro.ril.oem.imei",
    "ro.ril.miui.imei0",
    "vendor.gsm.imei",
    "ro.serialno",
];

/// 读取并缓存设备唯一标识，优先 IMEI，兜底 serialno。
pub fn device_id() -> Result<String> {
    if let Some(cached) = read_cache(Path::new(CACHE_FILE))? {
        return Ok(cached);
    }
    let id = detect_device_id().context("未能从任何来源读取设备标识")?;
    write_cache(Path::new(CACHE_FILE), &id)?;
    Ok(id)
}

fn detect_device_id() -> Option<String> {
    for key in IMEI_PROPS {
        if let Some(v) = getprop(key) {
            if is_valid_id(&v) {
                debug!(key, "device id resolved from getprop");
                return Some(v);
            }
        }
    }
    if let Some(v) = service_call_imei() {
        debug!("device id resolved from iphonesubinfo");
        return Some(v);
    }
    None
}

fn read_cache(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(s) => {
            let trimmed = s.trim().to_owned();
            Ok(if trimmed.is_empty() { None } else { Some(trimmed) })
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("读取设备标识缓存失败: {}", path.display())),
    }
}

fn write_cache(path: &Path, id: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建缓存目录失败: {}", parent.display()))?;
    }
    fs::write(path, format!("{id}\n"))
        .with_context(|| format!("写入设备标识缓存失败: {}", path.display()))
}

fn getprop(name: &str) -> Option<String> {
    let output = Command::new("getprop").arg(name).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn is_valid_id(value: &str) -> bool {
    !value.is_empty()
        && !value.eq_ignore_ascii_case("unknown")
        && value.chars().any(|c| c.is_ascii_alphanumeric() && c != '0')
}

/// service call iphonesubinfo 1：root 下兜底取 IMEI。
fn service_call_imei() -> Option<String> {
    let output = Command::new("service")
        .args(["call", "iphonesubinfo", "1"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let imei = parse_iphonesubinfo_parcel(&body);
    match imei {
        Some(s) if is_valid_id(&s) => Some(s),
        Some(s) => {
            warn!(value = s.as_str(), "service call iphonesubinfo 输出无效");
            None
        }
        None => None,
    }
}

/// 解析 `service call` 的 Parcel hex dump，提取 UTF-16 LE 数字字符。
/// 每个 32-bit word 包含两个 16-bit 字符（低位先打印）。
fn parse_iphonesubinfo_parcel(body: &str) -> Option<String> {
    let mut chars = String::new();
    for line in body.lines() {
        let payload = match line.split_once(':') {
            Some((_, rest)) => rest,
            None => continue,
        };
        for word in payload.split_whitespace() {
            if word.len() != 8 || !word.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }
            let Ok(v) = u32::from_str_radix(word, 16) else {
                continue;
            };
            for short in [(v & 0xFFFF) as u16, ((v >> 16) & 0xFFFF) as u16] {
                if let Some(c) = char::from_u32(short as u32) {
                    if c.is_ascii_digit() {
                        chars.push(c);
                    }
                }
            }
        }
    }
    if (14..=16).contains(&chars.len()) {
        Some(chars)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iphonesubinfo_parcel_imei() {
        // IMEI 123456789012345，长度 15
        let dump = "Result: Parcel(\n  0x00000000: 00000000 0000000f 00320031 00340033 00360035 00380037\n  0x00000018: 00300039 00320031 00340033 00000035\n  )";
        let imei = parse_iphonesubinfo_parcel(dump);
        assert_eq!(imei.as_deref(), Some("123456789012345"));
    }

    #[test]
    fn rejects_empty_parcel() {
        let dump = "Result: Parcel(\n  0x00000000: 00000000 00000000\n  )";
        assert_eq!(parse_iphonesubinfo_parcel(dump), None);
    }

    #[test]
    fn ignores_non_hex_words_and_dots() {
        let dump = "Result: Parcel(\n  0x00000000: 00000000 0000000f 00320031 ........\n  )";
        // 只有一个 word 含数字 '12'，不足 14 位
        assert_eq!(parse_iphonesubinfo_parcel(dump), None);
    }

    #[test]
    fn unknown_string_invalid() {
        assert!(!is_valid_id("unknown"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("0000000"));
        assert!(is_valid_id("123456789012345"));
        assert!(is_valid_id("ABC123"));
    }
}
