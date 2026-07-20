use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use flate2::read::GzDecoder;
use serde::Deserialize;
use sha2::{Digest, Sha256};

mod runtime;
pub use runtime::{CommandVersionRunner, HttpFetcher};
mod config;
pub use config::UpdateSettings;
mod scheduler;
pub use scheduler::{activation_pending, run_scheduler, update_snapshot, UpdateSnapshot};
pub(crate) use scheduler::{activation_failed, activation_started, activation_succeeded};
mod install;
pub use install::{activate_candidate, rollback_candidate, InstalledUpdate};

pub const MAX_ASSET_BYTES: usize = 64 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 1024 * 1024;
const GITHUB_LATEST: &str = "https://api.github.com/repos/MetaCubeX/mihomo/releases/latest";
const ARCH: &str = "android-arm64-v8";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateSource {
    GitHub,
    Company,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectPolicy {
    Deny,
    GithubHttps,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedUpdate {
    pub candidate_path: PathBuf,
    pub version: String,
    pub source: UpdateSource,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct CompanySource {
    pub base_url: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    pub url: String,
    pub authorization: Option<String>,
    pub redirects: RedirectPolicy,
}

pub trait Fetcher {
    fn fetch(&self, request: &FetchRequest, limit: usize) -> Result<Vec<u8>>;
}

pub trait VersionRunner {
    fn verify(&self, candidate: &Path, expected_version: &str) -> Result<()>;
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
    size: usize,
    digest: String,
}

#[derive(Deserialize)]
struct CompanyManifest {
    schema_version: u32,
    version: String,
    asset_url: String,
    sha256: String,
    size: usize,
    arch: String,
}

struct Artifact {
    version: String,
    url: String,
    size: usize,
    sha256: String,
    source: UpdateSource,
    authorization: Option<String>,
    redirects: RedirectPolicy,
}

pub fn check_and_prepare<F: Fetcher, R: VersionRunner>(
    fetcher: &F,
    runner: &R,
    current_version: Option<&str>,
    company: Option<&CompanySource>,
    run_dir: &Path,
) -> Result<Option<PreparedUpdate>> {
    if let Ok(artifact) = github_artifact(fetcher) {
        if !should_update(&artifact, current_version)? {
            return Ok(None);
        }
        if let Ok(prepared) = prepare_artifact(fetcher, runner, artifact, run_dir) {
            return Ok(Some(prepared));
        }
    }

    let Some(company) = company else {
        bail!("Mihomo GitHub 更新源检查失败，且未配置公司更新源");
    };
    let artifact = match company_artifact(fetcher, company) {
        Ok(artifact) => artifact,
        Err(_) => bail!("Mihomo GitHub 与公司更新源检查均失败"),
    };
    if !should_update(&artifact, current_version)? {
        return Ok(None);
    }
    match prepare_artifact(fetcher, runner, artifact, run_dir) {
        Ok(prepared) => Ok(Some(prepared)),
        Err(_) => bail!("Mihomo GitHub 与公司更新候选均未通过校验"),
    }
}

fn should_update(artifact: &Artifact, current_version: Option<&str>) -> Result<bool> {
    if current_version.is_some_and(|current| !is_newer(&artifact.version, current).unwrap_or(false)) {
        return Ok(false);
    }
    Ok(true)
}

fn prepare_artifact(
    fetcher: &impl Fetcher,
    runner: &impl VersionRunner,
    artifact: Artifact,
    run_dir: &Path,
) -> Result<PreparedUpdate> {
    let archive = fetcher.fetch(
        &FetchRequest {
            url: artifact.url.clone(),
            authorization: artifact.authorization,
            redirects: artifact.redirects,
        },
        artifact.size.saturating_add(1).min(MAX_ASSET_BYTES + 1),
    )?;
    if archive.len() != artifact.size {
        bail!("Mihomo 更新资产大小不匹配");
    }
    if sha256_hex(&archive) != artifact.sha256 {
        bail!("Mihomo 更新资产 SHA256 不匹配");
    }
    let binary = decode_gzip_limited(&archive, MAX_ASSET_BYTES)?;
    if !is_aarch64_elf(&binary) {
        bail!("Mihomo 更新资产不是 AArch64 ELF");
    }

    fs::create_dir_all(run_dir)
        .with_context(|| format!("创建更新候选目录失败: {}", run_dir.display()))?;
    let candidate_path = run_dir.join("mihomo.candidate");
    write_candidate(&candidate_path, &binary)?;
    if let Err(err) = runner.verify(&candidate_path, &artifact.version) {
        let _ = fs::remove_file(&candidate_path);
        return Err(err).context("Mihomo 候选版本自检失败");
    }
    Ok(PreparedUpdate {
        candidate_path,
        version: artifact.version,
        source: artifact.source,
        sha256: artifact.sha256,
    })
}

fn github_artifact(fetcher: &impl Fetcher) -> Result<Artifact> {
    let request = FetchRequest {
        url: GITHUB_LATEST.into(),
        authorization: None,
        redirects: RedirectPolicy::GithubHttps,
    };
    let body = fetcher.fetch(&request, MAX_METADATA_BYTES)?;
    let release: GithubRelease = serde_json::from_slice(&body).context("解析 GitHub release 失败")?;
    parse_version(&release.tag_name)?;
    let expected_name = format!("mihomo-{ARCH}-{}.gz", release.tag_name);
    let asset = release.assets.into_iter().find(|asset| asset.name == expected_name)
        .context("GitHub release 缺少 Android ARM64 资产")?;
    validate_https(&asset.browser_download_url)?;
    let sha256 = asset.digest.strip_prefix("sha256:").context("GitHub 资产缺少 SHA256 digest")?;
    validate_artifact(asset.size, sha256)?;
    Ok(Artifact {
        version: release.tag_name,
        url: asset.browser_download_url,
        size: asset.size,
        sha256: sha256.into(),
        source: UpdateSource::GitHub,
        authorization: None,
        redirects: RedirectPolicy::GithubHttps,
    })
}

fn company_artifact(fetcher: &impl Fetcher, source: &CompanySource) -> Result<Artifact> {
    let base = source.base_url.trim_end_matches('/');
    let origin = https_origin(base)?;
    let authorization = format!("Basic {}", STANDARD.encode(format!("{}:{}", source.username, source.password)));
    let request = FetchRequest {
        url: format!("{base}/mihomo/stable.json"),
        authorization: Some(authorization.clone()),
        redirects: RedirectPolicy::Deny,
    };
    let body = fetcher.fetch(&request, MAX_METADATA_BYTES)?;
    let manifest: CompanyManifest = serde_json::from_slice(&body).context("解析公司更新清单失败")?;
    if manifest.schema_version != 1 || manifest.arch != ARCH {
        bail!("公司更新清单 schema_version 或 arch 不受支持");
    }
    parse_version(&manifest.version)?;
    if https_origin(&manifest.asset_url)? != origin {
        bail!("公司更新资产必须与清单同源 HTTPS");
    }
    validate_artifact(manifest.size, &manifest.sha256)?;
    Ok(Artifact {
        version: manifest.version,
        url: manifest.asset_url,
        size: manifest.size,
        sha256: manifest.sha256,
        source: UpdateSource::Company,
        authorization: Some(authorization),
        redirects: RedirectPolicy::Deny,
    })
}

fn validate_artifact(size: usize, digest: &str) -> Result<()> {
    if size == 0 || size > MAX_ASSET_BYTES { bail!("Mihomo 更新资产大小无效") }
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("Mihomo 更新资产 SHA256 格式无效");
    }
    Ok(())
}

fn validate_https(url: &str) -> Result<()> { https_origin(url).map(|_| ()) }

fn https_origin(url: &str) -> Result<String> {
    let uri: ureq::http::Uri = url.parse().context("更新 URL 无效")?;
    if uri.scheme_str() != Some("https") { bail!("更新 URL 必须使用 HTTPS") }
    Ok(format!("https://{}", uri.authority().context("更新 URL 缺少主机")?))
}

fn is_newer(remote: &str, current: &str) -> Result<bool> {
    Ok(parse_version(remote)? > parse_version(current)?)
}

fn parse_version(value: &str) -> Result<(u64, u64, u64)> {
    let parts = value.strip_prefix('v').unwrap_or(value).split('.').collect::<Vec<_>>();
    if parts.len() != 3 { bail!("Mihomo 版本必须是 MAJOR.MINOR.PATCH") }
    Ok((parts[0].parse()?, parts[1].parse()?, parts[2].parse()?))
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_gzip_limited(compressed: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    GzDecoder::new(compressed).take(limit as u64 + 1).read_to_end(&mut output)?;
    if output.len() > limit { bail!("Mihomo 解压后超过大小上限") }
    Ok(output)
}

fn is_aarch64_elf(bytes: &[u8]) -> bool {
    bytes.len() >= 20 && bytes[..4] == *b"\x7fELF" && bytes[4] == 2 && bytes[5] == 1
        && u16::from_le_bytes([bytes[18], bytes[19]]) == 183
}

fn write_candidate(path: &Path, bytes: &[u8]) -> Result<()> {
    let temp = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o700).open(&temp)
        .with_context(|| format!("创建 Mihomo 候选临时文件失败: {}", temp.display()))?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        Ok(())
    })();
    if result.is_err() { let _ = fs::remove_file(&temp); }
    result.with_context(|| format!("写入 Mihomo 候选失败: {}", path.display()))
}

#[cfg(test)]
mod tests;
