use super::*;
use std::{fs, io, os::unix::fs::PermissionsExt, path::PathBuf};

fn temp_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!("f50mgr-auth-{label}-{}", std::process::id()))
}

fn fill(seed: u8) -> impl FnMut(&mut [u8]) -> io::Result<()> {
    move |bytes| {
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = seed.wrapping_add(index as u8);
        }
        Ok(())
    }
}

#[test]
fn ensure_creates_strong_root_only_key_and_preserves_existing_value() -> anyhow::Result<()> {
    let path = temp_path("ensure");
    let _ = fs::remove_file(&path);
    let created = ensure_management_key_at(&path, fill(1))?;
    assert!(created.created);
    assert_eq!(created.secret.len(), 64);
    assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);

    fs::set_permissions(&path, fs::Permissions::from_mode(0o644))?;
    let existing = ensure_management_key_at(&path, fill(99))?;
    assert!(!existing.created);
    assert_eq!(existing.secret, created.secret);
    assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn invalid_existing_key_fails_closed_without_overwrite() -> anyhow::Result<()> {
    let path = temp_path("invalid");
    fs::write(&path, "short\n")?;
    let error = ensure_management_key_at(&path, fill(2)).unwrap_err();
    assert!(error.to_string().contains("管理密钥"));
    assert_eq!(fs::read_to_string(&path)?, "short\n");
    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn reset_replaces_key_and_random_failure_leaves_existing_value() -> anyhow::Result<()> {
    let path = temp_path("reset");
    let _ = fs::remove_file(&path);
    let first = reset_management_key_at(&path, fill(3))?;
    let error =
        reset_management_key_at(&path, |_| Err(io::Error::other("no entropy"))).unwrap_err();
    assert!(error.to_string().contains("随机"));
    assert_eq!(read_management_key_at(&path)?, first);
    let second = reset_management_key_at(&path, fill(4))?;
    assert_ne!(second, first);
    fs::remove_file(path)?;
    Ok(())
}

#[test]
fn rejects_symbolic_link_key_file() -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;
    let target = temp_path("target");
    let link = temp_path("link");
    let _ = fs::remove_file(&target);
    let _ = fs::remove_file(&link);
    fs::write(&target, "0123456789abcdef0123456789abcdef\n")?;
    symlink(&target, &link)?;
    assert!(read_management_key_at(&link).is_err());
    fs::remove_file(link)?;
    fs::remove_file(target)?;
    Ok(())
}
