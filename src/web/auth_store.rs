use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
    path::Path,
};

use anyhow::{bail, Context, Result};

pub const MANAGEMENT_KEY_FILE: &str = "/data/adb/f50mgr/config/auth.key";
const KEY_BYTES: usize = 32;

#[derive(Debug, PartialEq, Eq)]
pub struct EnsureResult {
    pub secret: String,
    pub created: bool,
}

pub fn read_management_key() -> Result<String> {
    read_management_key_at(Path::new(MANAGEMENT_KEY_FILE))
}

pub fn ensure_management_key() -> Result<EnsureResult> {
    ensure_management_key_at(Path::new(MANAGEMENT_KEY_FILE), fill_os_random)
}

pub fn reset_management_key() -> Result<String> {
    reset_management_key_at(Path::new(MANAGEMENT_KEY_FILE), fill_os_random)
}

fn ensure_management_key_at(
    path: &Path,
    fill_random: impl FnMut(&mut [u8]) -> std::io::Result<()>,
) -> Result<EnsureResult> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                bail!("管理密钥不是普通文件: {}", path.display());
            }
            let secret = read_management_key_at(path)?;
            set_root_only_mode(path)?;
            Ok(EnsureResult {
                secret,
                created: false,
            })
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let secret = reset_management_key_at(path, fill_random)?;
            Ok(EnsureResult {
                secret,
                created: true,
            })
        }
        Err(err) => Err(err).with_context(|| format!("检查管理密钥失败: {}", path.display())),
    }
}

fn reset_management_key_at(
    path: &Path,
    mut fill_random: impl FnMut(&mut [u8]) -> std::io::Result<()>,
) -> Result<String> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if !metadata.file_type().is_file() {
            bail!("管理密钥不是普通文件: {}", path.display());
        }
    }
    let mut bytes = [0_u8; KEY_BYTES];
    fill_random(&mut bytes).context("读取系统随机源失败")?;
    let secret = hex_encode(&bytes);
    crate::subscription::write_atomic(path, format!("{secret}\n").as_bytes())?;
    set_root_only_mode(path)?;
    Ok(secret)
}

fn read_management_key_at(path: &Path) -> Result<String> {
    let mut file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("打开管理密钥失败: {}", path.display()))?;
    if !file
        .metadata()
        .with_context(|| format!("读取管理密钥元数据失败: {}", path.display()))?
        .file_type()
        .is_file()
    {
        bail!("管理密钥不是普通文件: {}", path.display());
    }
    let mut content = String::new();
    file.by_ref()
        .take(256)
        .read_to_string(&mut content)
        .with_context(|| format!("读取管理密钥失败: {}", path.display()))?;
    let secret = content.trim();
    if secret.len() != KEY_BYTES * 2 || !secret.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("管理密钥格式无效，必须是 256-bit 十六进制值");
    }
    Ok(secret.to_ascii_lowercase())
}

fn set_root_only_mode(path: &Path) -> Result<()> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .with_context(|| format!("打开管理密钥以修正权限失败: {}", path.display()))?;
    let result = unsafe { libc::fchmod(file.as_raw_fd(), 0o600) };
    if result != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("设置管理密钥权限失败: {}", path.display()));
    }
    Ok(())
}

fn fill_os_random(bytes: &mut [u8]) -> std::io::Result<()> {
    File::open("/dev/urandom")?.read_exact(bytes)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests;
