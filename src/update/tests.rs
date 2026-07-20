use std::{
    cell::RefCell, collections::HashMap, fs, io::Write,
    os::unix::fs::PermissionsExt, path::{Path, PathBuf},
};

use anyhow::{bail, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use flate2::{write::GzEncoder, Compression};
use sha2::{Digest, Sha256};

use super::*;

const LATEST: &str = "https://api.github.com/repos/MetaCubeX/mihomo/releases/latest";

struct FakeFetcher {
    replies: HashMap<String, Result<Vec<u8>, String>>,
    requests: RefCell<Vec<FetchRequest>>,
}

impl Fetcher for FakeFetcher {
    fn fetch(&self, request: &FetchRequest, limit: usize) -> Result<Vec<u8>> {
        self.requests.borrow_mut().push(request.clone());
        match self.replies.get(&request.url) {
            Some(Ok(body)) if body.len() <= limit => Ok(body.clone()),
            Some(Ok(_)) => bail!("response exceeds limit"),
            Some(Err(message)) => bail!(message.clone()),
            None => bail!("unexpected URL: {}", request.url),
        }
    }
}

#[derive(Default)]
struct FakeRunner(RefCell<Vec<(PathBuf, String)>>);

impl VersionRunner for FakeRunner {
    fn verify(&self, candidate: &Path, expected: &str) -> Result<()> {
        assert!(is_aarch64_elf(&fs::read(candidate)?));
        self.0.borrow_mut().push((candidate.into(), expected.into()));
        Ok(())
    }
}

#[test]
fn github_release_prepares_verified_candidate() -> Result<()> {
    let dir = temp_dir("github");
    let version = "v1.19.14";
    let asset_url = "https://github.com/MetaCubeX/mihomo/releases/download/v1.19.14/mihomo-android-arm64-v8-v1.19.14.gz";
    let binary = elf_aarch64();
    let archive = gzip(&binary)?;
    let digest = sha256(&archive);
    let release = serde_json::json!({"tag_name":version,"assets":[{
        "name":format!("mihomo-android-arm64-v8-{version}.gz"),
        "browser_download_url":asset_url,"size":archive.len(),
        "digest":format!("sha256:{digest}")
    }]});
    let fetcher = fake([(LATEST, Ok(release.to_string().into_bytes())), (asset_url, Ok(archive))]);
    let runner = FakeRunner::default();

    let prepared = check_and_prepare(&fetcher, &runner, None, None, &dir)?.expect("candidate");

    assert_eq!(prepared.source, UpdateSource::GitHub);
    assert_eq!(prepared.version, version);
    assert_eq!(prepared.sha256, digest);
    assert_eq!(fs::read(&prepared.candidate_path)?, binary);
    assert_eq!(fs::metadata(&prepared.candidate_path)?.permissions().mode() & 0o777, 0o700);
    assert_eq!(runner.0.borrow().len(), 1);
    assert!(fetcher.requests.borrow().iter().all(|r| r.redirects == RedirectPolicy::GithubHttps));
    cleanup(&dir);
    Ok(())
}

#[test]
fn github_failure_uses_company_basic_auth_without_redirects() -> Result<()> {
    let dir = temp_dir("company");
    let base = "https://updates.example.test/root";
    let manifest_url = format!("{base}/mihomo/stable.json");
    let asset_url = "https://updates.example.test/files/mihomo.gz";
    let archive = gzip(&elf_aarch64())?;
    let manifest = company_manifest(asset_url, &sha256(&archive), archive.len());
    let fetcher = fake([
        (LATEST, Err("offline".into())),
        (&manifest_url, Ok(manifest.into_bytes())),
        (asset_url, Ok(archive)),
    ]);
    let company = CompanySource { base_url: base.into(), username: "alice".into(), password: "secret".into() };

    let prepared = check_and_prepare(&fetcher, &FakeRunner::default(), None, Some(&company), &dir)?
        .expect("company candidate");

    assert_eq!(prepared.source, UpdateSource::Company);
    let auth = format!("Basic {}", STANDARD.encode("alice:secret"));
    for request in &fetcher.requests.borrow()[1..] {
        assert_eq!(request.authorization.as_deref(), Some(auth.as_str()));
        assert_eq!(request.redirects, RedirectPolicy::Deny);
    }
    cleanup(&dir);
    Ok(())
}

#[test]
fn unsafe_or_corrupt_company_source_leaves_no_candidate() -> Result<()> {
    for (name, asset_url, digest, size) in [
        ("cross-origin", "https://evil.example/mihomo.gz", "0".repeat(64), 64),
        ("digest", "https://updates.example/mihomo.gz", "f".repeat(64), 64),
        ("oversize", "https://updates.example/mihomo.gz", "0".repeat(64), MAX_ASSET_BYTES + 1),
    ] {
        let dir = temp_dir(name);
        let base = "https://updates.example";
        let manifest_url = format!("{base}/mihomo/stable.json");
        let archive = gzip(&elf_aarch64())?;
        let manifest = company_manifest(asset_url, &digest, if name == "digest" { archive.len() } else { size });
        let fetcher = fake([
            (LATEST, Err("offline".into())),
            (&manifest_url, Ok(manifest.into_bytes())),
            (asset_url, Ok(archive)),
        ]);
        let company = CompanySource { base_url: base.into(), username: "u".into(), password: "p".into() };
        assert!(check_and_prepare(&fetcher, &FakeRunner::default(), None, Some(&company), &dir).is_err());
        assert!(!dir.join("mihomo.candidate").exists());
        cleanup(&dir);
    }
    let archive = gzip(b"123456789")?;
    assert!(decode_gzip_limited(&archive, 8).is_err());
    assert_eq!(decode_gzip_limited(&archive, 9)?, b"123456789");
    Ok(())
}

#[test]
fn github_corrupt_asset_without_company_returns_error() -> Result<()> {
    let dir = temp_dir("github-corrupt-no-company");
    let version = "v1.19.14";
    let asset_url = "https://github.com/MetaCubeX/mihomo/releases/download/v1.19.14/mihomo-android-arm64-v8-v1.19.14.gz";
    let archive = gzip(&elf_aarch64())?;
    let release = serde_json::json!({"tag_name":version,"assets":[{
        "name":format!("mihomo-android-arm64-v8-{version}.gz"),
        "browser_download_url":asset_url,"size":archive.len(),
        "digest":format!("sha256:{}", "0".repeat(64))
    }]});
    let fetcher = fake([
        (LATEST, Ok(release.to_string().into_bytes())),
        (asset_url, Ok(archive)),
    ]);

    assert!(check_and_prepare(&fetcher, &FakeRunner::default(), None, None, &dir).is_err());
    assert!(!dir.join("mihomo.candidate").exists());
    cleanup(&dir);
    Ok(())
}

#[test]
fn failed_sources_return_redacted_error() {
    let dir = temp_dir("failed-sources-redacted");
    let base = "https://private-updates.example/tenant";
    let manifest_url = format!("{base}/mihomo/stable.json");
    let username = "private-user";
    let password = "private-password";
    let basic = format!("Basic {}", STANDARD.encode(format!("{username}:{password}")));
    let fetcher = fake([
        (LATEST, Err("GitHub unavailable".into())),
        (
            &manifest_url,
            Err(format!("request failed: {manifest_url} {username} {password} {basic}")),
        ),
    ]);
    let company = CompanySource {
        base_url: base.into(),
        username: username.into(),
        password: password.into(),
    };

    let error = check_and_prepare(&fetcher, &FakeRunner::default(), None, Some(&company), &dir)
        .expect_err("两个来源都失败时必须返回错误")
        .to_string();

    for secret in [base, username, password, basic.as_str()] {
        assert!(!error.contains(secret), "错误信息泄露敏感字段: {error}");
    }
    assert!(!dir.join("mihomo.candidate").exists());
    cleanup(&dir);
}

#[test]
fn current_or_older_version_skips_asset_download() -> Result<()> {
    for (current, remote) in [("v1.19.14", "v1.19.14"), ("v2.0.0", "v1.19.14")] {
        let dir = temp_dir("old");
        let release = serde_json::json!({"tag_name":remote,"assets":[{
            "name":format!("mihomo-android-arm64-v8-{remote}.gz"),
            "browser_download_url":"https://github.com/unused.gz","size":1,
            "digest":format!("sha256:{}", "0".repeat(64))
        }]});
        let fetcher = fake([(LATEST, Ok(release.to_string().into_bytes()))]);
        assert!(check_and_prepare(&fetcher, &FakeRunner::default(), Some(current), None, &dir)?.is_none());
        assert_eq!(fetcher.requests.borrow().len(), 1);
        cleanup(&dir);
    }
    Ok(())
}

fn fake<const N: usize>(pairs: [(&str, Result<Vec<u8>, String>); N]) -> FakeFetcher {
    FakeFetcher { replies: pairs.into_iter().map(|(url, body)| (url.into(), body)).collect(), requests: RefCell::new(Vec::new()) }
}

fn company_manifest(asset_url: &str, digest: &str, size: usize) -> String {
    serde_json::json!({"schema_version":1,"version":"v1.19.14","asset_url":asset_url,
        "sha256":digest,"size":size,"arch":"android-arm64-v8"}).to_string()
}

fn elf_aarch64() -> Vec<u8> {
    let mut bytes = vec![0_u8; 64];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[18..20].copy_from_slice(&183_u16.to_le_bytes());
    bytes
}

fn is_aarch64_elf(bytes: &[u8]) -> bool {
    bytes.len() >= 20 && bytes[..4] == *b"\x7fELF" && bytes[4] == 2 && bytes[5] == 1
        && u16::from_le_bytes([bytes[18], bytes[19]]) == 183
}

fn gzip(bytes: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes)?;
    Ok(encoder.finish()?)
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn temp_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("f50mgr-update-{name}-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_nanos()))
}

fn cleanup(path: &Path) { let _ = fs::remove_dir_all(path); }
