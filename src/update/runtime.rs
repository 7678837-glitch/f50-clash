use std::{
    io::Read,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};

use super::{FetchRequest, Fetcher, RedirectPolicy, VersionRunner};

pub struct HttpFetcher {
    timeout: Duration,
}

impl HttpFetcher {
    pub fn new(timeout: Duration) -> Self { Self { timeout } }
}

impl Fetcher for HttpFetcher {
    fn fetch(&self, request: &FetchRequest, limit: usize) -> Result<Vec<u8>> {
        let agent: ureq::Agent = agent_config(self.timeout, request.redirects).into();
        let mut call = agent.get(&request.url).header("User-Agent", "f50mgr-updater");
        if let Some(value) = &request.authorization {
            call = call.header("Authorization", value);
        }
        let mut response = call.call().with_context(|| format!("下载更新资源失败: {}", request.url))?;
        if response.status().is_redirection() {
            bail!("更新源返回了不允许的重定向");
        }
        response.body_mut().with_config().limit(limit as u64).read_to_vec()
            .with_context(|| format!("读取更新资源失败: {}", request.url))
    }
}

pub struct CommandVersionRunner {
    timeout: Duration,
}

impl CommandVersionRunner {
    pub fn new(timeout: Duration) -> Self { Self { timeout } }

    pub fn installed_version(&self, binary: &Path) -> Result<String> {
        let output = self.version_output(binary)?;
        output.split_whitespace()
            .map(|part| part.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.'))
            .find(|part| is_version_token(part))
            .map(str::to_owned)
            .context("Mihomo 版本输出不含 vMAJOR.MINOR.PATCH")
    }

    fn version_output(&self, binary: &Path) -> Result<String> {
        let mut child = Command::new(binary)
            .arg("-v")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("执行 Mihomo 版本检查失败: {}", binary.display()))?;
        let deadline = Instant::now() + self.timeout;
        let status = loop {
            if let Some(status) = child.try_wait().context("检查 Mihomo 候选版本进程失败")? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("Mihomo 候选版本检查超时");
            }
            thread::sleep(Duration::from_millis(20));
        };
        let mut output = Vec::new();
        if let Some(mut stdout) = child.stdout.take() { stdout.read_to_end(&mut output)?; }
        if let Some(mut stderr) = child.stderr.take() { stderr.read_to_end(&mut output)?; }
        if !status.success() { bail!("Mihomo 候选版本检查退出码异常: {status}") }
        Ok(String::from_utf8_lossy(&output).into_owned())
    }
}

impl VersionRunner for CommandVersionRunner {
    fn verify(&self, candidate: &Path, expected_version: &str) -> Result<()> {
        if self.installed_version(candidate)? != expected_version {
            bail!("Mihomo 候选版本与清单不一致");
        }
        Ok(())
    }
}

fn is_version_token(value: &str) -> bool {
    let Some(value) = value.strip_prefix('v') else { return false };
    let parts = value.split('.').collect::<Vec<_>>();
    parts.len() == 3 && parts.iter().all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

fn agent_config(timeout: Duration, redirects: RedirectPolicy) -> ureq::config::Config {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .https_only(true)
        .max_redirects(match redirects { RedirectPolicy::Deny => 0, RedirectPolicy::GithubHttps => 5 })
        .build()
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, time::Instant};

    use anyhow::Result;

    use super::*;

    #[test]
    fn http_policy_is_https_only_and_company_redirects_are_denied() {
        let timeout = Duration::from_secs(7);
        let company = agent_config(timeout, RedirectPolicy::Deny);
        let github = agent_config(timeout, RedirectPolicy::GithubHttps);
        assert!(company.https_only());
        assert_eq!(company.max_redirects(), 0);
        assert!(github.https_only());
        assert_eq!(github.max_redirects(), 5);
    }

    #[test]
    fn command_runner_checks_version_and_enforces_timeout() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("f50mgr-runner-{}", std::process::id()));
        fs::create_dir_all(&dir)?;
        let good = dir.join("good");
        fs::write(&good, "#!/bin/sh\necho 'Mihomo Meta v1.19.29 linux arm64'\n")?;
        fs::set_permissions(&good, fs::Permissions::from_mode(0o700))?;
        CommandVersionRunner::new(Duration::from_secs(5)).verify(&good, "v1.19.29")?;
        assert_eq!(
            CommandVersionRunner::new(Duration::from_secs(5)).installed_version(&good)?,
            "v1.19.29"
        );

        let slow = dir.join("slow");
        fs::write(&slow, "#!/bin/sh\nexec sleep 3\n")?;
        fs::set_permissions(&slow, fs::Permissions::from_mode(0o700))?;
        let started = Instant::now();
        assert!(CommandVersionRunner::new(Duration::from_millis(100)).verify(&slow, "v1").is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }
}
