use std::{
    ffi::CString,
    fs,
    os::unix::{ffi::OsStrExt, fs::{MetadataExt, PermissionsExt}},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};

use super::PreparedUpdate;

#[derive(Debug)]
pub struct InstalledUpdate {
    pub current_path: PathBuf,
    pub pending_path: PathBuf,
    pub backup_path: PathBuf,
    pub failed_path: PathBuf,
    pub prepared: PreparedUpdate,
}

impl InstalledUpdate {
    pub fn commit(&self) -> Result<()> { commit_candidate(self) }

    pub fn interrupted_update_pending(current: &Path) -> Result<bool> {
        interrupted_update_pending(current)
    }

    pub fn recover_interrupted_update(current: &Path) -> Result<bool> {
        recover_interrupted_update(current)
    }
}

pub fn activate_candidate(prepared: PreparedUpdate, current: &Path) -> Result<InstalledUpdate> {
    if prepared.candidate_path == current { bail!("Mihomo 候选路径不能等于当前路径") }
    let candidate_meta = regular_file(&prepared.candidate_path, "候选")?;
    let current_meta = regular_file(current, "当前")?;
    if candidate_meta.len() == 0 { bail!("Mihomo 候选文件为空") }
    set_owner(&prepared.candidate_path, current_meta.uid(), current_meta.gid())?;
    fs::set_permissions(&prepared.candidate_path, fs::Permissions::from_mode(0o755))?;

    let pending_path = current.with_extension("pending");
    ensure_absent(&pending_path, "待验证")?;
    let backup_path = current.with_extension("previous");
    let failed_path = current.with_extension("failed");
    fs::rename(current, &pending_path).context("暂存当前 Mihomo 失败")?;
    if let Err(err) = sync_parent(current) {
        fs::rename(&pending_path, current).context("暂存后目录同步失败且恢复当前 Mihomo 失败")?;
        let _ = sync_parent(current);
        return Err(err).context("暂存当前 Mihomo 后同步目录失败");
    }
    if let Err(err) = fs::rename(&prepared.candidate_path, current) {
        fs::rename(&pending_path, current).context("激活失败且恢复当前 Mihomo 失败")?;
        let _ = sync_parent(current);
        return Err(err).context("激活 Mihomo 候选失败");
    }
    if let Err(err) = sync_parent(current) {
        tracing::warn!(error = %err, "Mihomo 候选已激活但目录同步失败，继续执行健康验证");
    }
    Ok(InstalledUpdate {
        current_path: current.to_owned(),
        pending_path,
        backup_path,
        failed_path,
        prepared,
    })
}

fn commit_candidate(installed: &InstalledUpdate) -> Result<()> {
    regular_file(&installed.current_path, "已验证")?;
    regular_file(&installed.pending_path, "待提交")?;
    if let Ok(metadata) = fs::symlink_metadata(&installed.backup_path) {
        if !metadata.file_type().is_file() {
            bail!("Mihomo 上一确认版本路径不是普通文件: {}", installed.backup_path.display());
        }
    }
    fs::rename(&installed.pending_path, &installed.backup_path)
        .context("提交已验证 Mihomo 版本失败")?;
    if let Err(err) = sync_parent(&installed.current_path) {
        tracing::warn!(error = %err, "Mihomo 更新已提交但目录同步失败");
    }
    Ok(())
}

pub fn rollback_candidate(installed: &InstalledUpdate) -> Result<()> {
    regular_file(&installed.current_path, "激活后")?;
    regular_file(&installed.pending_path, "回滚")?;
    fs::rename(&installed.current_path, &installed.failed_path)
        .context("保存失败 Mihomo 版本失败")?;
    if let Err(err) = fs::rename(&installed.pending_path, &installed.current_path) {
        fs::rename(&installed.failed_path, &installed.current_path)
            .context("回滚失败且恢复候选 Mihomo 失败")?;
        return Err(err).context("恢复上一版 Mihomo 失败");
    }
    sync_parent(&installed.current_path)
}

fn interrupted_update_pending(current: &Path) -> Result<bool> {
    match fs::symlink_metadata(current.with_extension("pending")) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).context("检查 Mihomo 更新事务标记失败"),
    }
}

fn recover_interrupted_update(current: &Path) -> Result<bool> {
    let pending = current.with_extension("pending");
    if !interrupted_update_pending(current)? {
        regular_file(current, "当前")?;
        return Ok(false);
    }
    regular_file(&pending, "待恢复")?;
    let failed = current.with_extension("failed");
    match fs::symlink_metadata(current) {
        Ok(metadata) => {
            if !metadata.file_type().is_file() {
                bail!("Mihomo 当前路径不是普通文件: {}", current.display());
            }
            fs::rename(current, &failed).context("保存中断更新的未验证 Mihomo 失败")?;
            if let Err(err) = fs::rename(&pending, current) {
                fs::rename(&failed, current).context("恢复中断更新失败且恢复候选 Mihomo 失败")?;
                return Err(err).context("恢复中断更新前的 Mihomo 失败");
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::rename(&pending, current).context("恢复中断更新前的 Mihomo 失败")?;
        }
        Err(err) => return Err(err).context("检查当前 Mihomo 失败"),
    }
    sync_parent(current)?;
    Ok(true)
}

fn regular_file(path: &Path, label: &str) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("读取 Mihomo {label}文件失败: {}", path.display()))?;
    if !metadata.file_type().is_file() {
        bail!("Mihomo {label}路径不是普通文件: {}", path.display());
    }
    Ok(metadata)
}

fn ensure_absent(path: &Path, label: &str) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => bail!("Mihomo {label}路径已存在: {}", path.display()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("检查 Mihomo {label}路径失败")),
    }
}

fn set_owner(path: &Path, uid: u32, gid: u32) -> Result<()> {
    let current = fs::metadata(path)?;
    if current.uid() == uid && current.gid() == gid { return Ok(()) }
    let path = CString::new(path.as_os_str().as_bytes()).context("Mihomo 路径包含 NUL")?;
    if unsafe { libc::chown(path.as_ptr(), uid, gid) } != 0 {
        return Err(std::io::Error::last_os_error()).context("设置 Mihomo 候选所有者失败");
    }
    Ok(())
}

fn sync_parent(path: &Path) -> Result<()> {
    let parent = path.parent().context("Mihomo 路径缺少父目录")?;
    fs::File::open(parent)?.sync_all().context("同步 Mihomo 目录失败")
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::{fs::{symlink, PermissionsExt}, prelude::MetadataExt}};

    use anyhow::Result;

    use super::*;
    use crate::update::UpdateSource;

    #[test]
    fn activate_keeps_old_binary_pending_until_commit() -> Result<()> {
        let dir = temp_dir("commit");
        fs::create_dir_all(&dir)?;
        let current = dir.join("mihomo");
        let candidate = dir.join("mihomo.candidate");
        fs::write(&current, b"old")?;
        fs::write(&candidate, b"new")?;
        fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700))?;
        let current_meta = fs::metadata(&current)?;

        let installed = activate_candidate(prepared(candidate), &current)?;

        assert_eq!(fs::read(&current)?, b"new");
        assert_eq!(fs::read(&installed.pending_path)?, b"old");
        assert!(!installed.backup_path.exists());
        let active_meta = fs::metadata(&current)?;
        assert_eq!(active_meta.permissions().mode() & 0o7777, 0o755);
        assert_eq!((active_meta.uid(), active_meta.gid()), (current_meta.uid(), current_meta.gid()));

        commit_candidate(&installed)?;
        assert_eq!(fs::read(&current)?, b"new");
        assert_eq!(fs::read(&installed.backup_path)?, b"old");
        assert!(!installed.pending_path.exists());
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn rollback_restores_pending_binary_and_preserves_failed_candidate() -> Result<()> {
        let dir = temp_dir("rollback");
        fs::create_dir_all(&dir)?;
        let current = dir.join("mihomo");
        let candidate = dir.join("mihomo.candidate");
        fs::write(&current, b"old")?;
        fs::write(&candidate, b"new")?;

        let installed = activate_candidate(prepared(candidate), &current)?;
        rollback_candidate(&installed)?;

        assert_eq!(fs::read(&current)?, b"old");
        assert_eq!(fs::read(&installed.failed_path)?, b"new");
        assert!(!installed.pending_path.exists());
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn recovery_restores_pending_when_current_is_missing() -> Result<()> {
        let dir = temp_dir("recover-first-rename");
        fs::create_dir_all(&dir)?;
        let current = dir.join("mihomo");
        let pending = dir.join("mihomo.pending");
        fs::write(&pending, b"old")?;

        assert!(recover_interrupted_update(&current)?);

        assert_eq!(fs::read(&current)?, b"old");
        assert!(!pending.exists());
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn recovery_rejects_unverified_current_and_restores_pending() -> Result<()> {
        let dir = temp_dir("recover-second-rename");
        fs::create_dir_all(&dir)?;
        let current = dir.join("mihomo");
        let pending = dir.join("mihomo.pending");
        let failed = dir.join("mihomo.failed");
        fs::write(&current, b"new")?;
        fs::write(&pending, b"old")?;

        assert!(recover_interrupted_update(&current)?);

        assert_eq!(fs::read(&current)?, b"old");
        assert_eq!(fs::read(&failed)?, b"new");
        assert!(!pending.exists());
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn activate_rejects_symlink_candidate_without_touching_current() -> Result<()> {
        let dir = temp_dir("symlink");
        fs::create_dir_all(&dir)?;
        let current = dir.join("mihomo");
        let target = dir.join("target");
        let candidate = dir.join("mihomo.candidate");
        fs::write(&current, b"old")?;
        fs::write(&target, b"evil")?;
        symlink(&target, &candidate)?;

        assert!(activate_candidate(prepared(candidate), &current).is_err());
        assert_eq!(fs::read(&current)?, b"old");
        assert_eq!(fs::read(&target)?, b"evil");
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    fn prepared(candidate_path: PathBuf) -> PreparedUpdate {
        PreparedUpdate {
            candidate_path,
            version: "v1.19.29".into(),
            source: UpdateSource::GitHub,
            sha256: "a".repeat(64),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("f50mgr-install-{name}-{}", std::process::id()))
    }
}
