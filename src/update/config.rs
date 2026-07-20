use std::{fs, io::ErrorKind, path::Path, time::Duration};

use anyhow::{bail, Context, Result};

use super::CompanySource;

#[derive(Debug, Clone)]
pub struct UpdateSettings {
    pub interval: Duration,
    pub timeout: Duration,
    pub company: Option<CompanySource>,
}

impl UpdateSettings {
    pub fn load(config: &Path, secrets: &Path) -> Result<Self> {
        let config_text = read_optional(config)?;
        let interval = section_value(&config_text, "mihomo", "version_check_interval")?
            .map(|value| parse_duration(&value))
            .transpose()?
            .unwrap_or(Duration::from_secs(6 * 3600));
        let timeout = section_value(&config_text, "download", "github_timeout_secs")?
            .map(|value| value.parse::<u64>().context("github_timeout_secs 必须是整数"))
            .transpose()?
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(30));
        if timeout.is_zero() || timeout > Duration::from_secs(120) {
            bail!("github_timeout_secs 必须在 1..=120 之间");
        }

        let company = match section_value(&config_text, "download", "company_base_url")? {
            None => None,
            Some(base_url) => {
                let base_url = base_url.trim_end_matches('/').to_owned();
                super::https_origin(&base_url)?;
                let secrets_text = read_optional(secrets)?;
                let username = section_value(&secrets_text, "download", "username")?
                    .filter(|value| !value.is_empty())
                    .context("公司更新源缺少 download.username")?;
                let password = section_value(&secrets_text, "download", "password")?
                    .filter(|value| !value.is_empty())
                    .context("公司更新源缺少 download.password")?;
                Some(CompanySource { base_url, username, password })
            }
        };
        Ok(Self { interval, timeout, company })
    }
}

fn read_optional(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err).with_context(|| format!("读取更新配置失败: {}", path.display())),
    }
}

fn section_value(content: &str, section: &str, key: &str) -> Result<Option<String>> {
    let mut active = false;
    for line in content.lines() {
        let structural = line.split('#').next().unwrap_or_default().trim();
        if structural.starts_with('[') && structural.ends_with(']') {
            active = structural == format!("[{section}]");
            continue;
        }
        if !active { continue }
        let Some((name, value)) = line.split_once('=') else { continue };
        if name.trim() == key {
            return parse_value(value).map(Some);
        }
    }
    Ok(None)
}

fn parse_value(value: &str) -> Result<String> {
    let value = value.trim_start();
    match value.chars().next() {
        Some('"') => parse_basic_string(value),
        Some('\'') => parse_literal_string(value),
        _ => Ok(value
            .split('#')
            .next()
            .unwrap_or_default()
            .trim()
            .to_owned()),
    }
}

fn parse_basic_string(value: &str) -> Result<String> {
    let mut chars = value[1..].chars();
    let mut parsed = String::new();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => {
                ensure_only_comment(chars.as_str())?;
                return Ok(parsed);
            }
            '\\' => {
                let escaped = chars.next().context("双引号字符串转义不完整")?;
                parsed.push(match escaped {
                    '"' => '"',
                    '\\' => '\\',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    _ => bail!("双引号字符串包含不支持的转义: \\{escaped}"),
                });
            }
            _ => parsed.push(ch),
        }
    }
    bail!("双引号字符串未闭合")
}

fn parse_literal_string(value: &str) -> Result<String> {
    let mut chars = value[1..].chars();
    let mut parsed = String::new();
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            ensure_only_comment(chars.as_str())?;
            return Ok(parsed);
        }
        parsed.push(ch);
    }
    bail!("单引号字符串未闭合")
}

fn ensure_only_comment(remainder: &str) -> Result<()> {
    let remainder = remainder.trim_start();
    if remainder.is_empty() || remainder.starts_with('#') {
        Ok(())
    } else {
        bail!("字符串结束后存在无效内容")
    }
}

fn parse_duration(value: &str) -> Result<Duration> {
    let (number, multiplier) = if let Some(value) = value.strip_suffix('h') {
        (value, 3600)
    } else if let Some(value) = value.strip_suffix('m') {
        (value, 60)
    } else if let Some(value) = value.strip_suffix('s') {
        (value, 1)
    } else {
        bail!("version_check_interval 必须以 h/m/s 结尾");
    };
    let seconds = number.parse::<u64>().context("version_check_interval 数值无效")?
        .checked_mul(multiplier).context("version_check_interval 溢出")?;
    if seconds == 0 || seconds > 7 * 24 * 3600 {
        bail!("version_check_interval 必须在 1 秒到 7 天之间");
    }
    Ok(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn settings_load_interval_timeout_and_private_company_credentials() -> Result<()> {
        let dir = temp_dir("full");
        fs::create_dir_all(&dir)?;
        let config = dir.join("f50mgr.toml");
        let secrets = dir.join("secrets.toml");
        fs::write(&config, "[mihomo]\nversion_check_interval = \"6h\"\n\n[download]\ngithub_timeout_secs = 30\ncompany_base_url = \"https://updates.example/root/\"\n")?;
        fs::write(
            &secrets,
            r#"[download]
username = "ali\"ce\\ops"
password = "abc#def"
"#,
        )?;

        let settings = UpdateSettings::load(&config, &secrets)?;

        assert_eq!(settings.interval, Duration::from_secs(6 * 3600));
        assert_eq!(settings.timeout, Duration::from_secs(30));
        let company = settings.company.expect("company source");
        assert_eq!(company.base_url, "https://updates.example/root");
        assert_eq!(company.username, r#"ali"ce\ops"#);
        assert_eq!(company.password, "abc#def");
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn settings_support_single_quoted_literal_credentials() -> Result<()> {
        let dir = temp_dir("literal");
        fs::create_dir_all(&dir)?;
        let config = dir.join("f50mgr.toml");
        let secrets = dir.join("secrets.toml");
        fs::write(
            &config,
            "[download]\ncompany_base_url = 'https://updates.example/root'\n",
        )?;
        fs::write(
            &secrets,
            "[download]\nusername = 'alice#ops'\npassword = 'abc\\def'\n",
        )?;

        let company = UpdateSettings::load(&config, &secrets)?
            .company
            .expect("company source");
        assert_eq!(company.base_url, "https://updates.example/root");
        assert_eq!(company.username, "alice#ops");
        assert_eq!(company.password, r"abc\def");
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn settings_reject_unclosed_quoted_string() -> Result<()> {
        let dir = temp_dir("malformed");
        fs::create_dir_all(&dir)?;
        let config = dir.join("f50mgr.toml");
        let secrets = dir.join("secrets.toml");
        fs::write(
            &config,
            "[download]\ncompany_base_url = \"https://updates.example/root\"\n",
        )?;
        fs::write(
            &secrets,
            "[download]\nusername = \"alice\npassword = \"private\"\n",
        )?;

        assert!(UpdateSettings::load(&config, &secrets).is_err());
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    #[test]
    fn settings_default_without_company_and_reject_invalid_boundaries() -> Result<()> {
        let dir = temp_dir("defaults");
        fs::create_dir_all(&dir)?;
        let config = dir.join("f50mgr.toml");
        let secrets = dir.join("secrets.toml");
        fs::write(&config, "[mihomo]\nauto_start = true\n")?;
        let settings = UpdateSettings::load(&config, &secrets)?;
        assert_eq!(settings.interval, Duration::from_secs(6 * 3600));
        assert_eq!(settings.timeout, Duration::from_secs(30));
        assert!(settings.company.is_none());

        fs::write(&config, "[mihomo]\nversion_check_interval = \"0h\"\n")?;
        assert!(UpdateSettings::load(&config, &secrets).is_err());
        fs::write(&config, "[download]\ncompany_base_url = \"http://updates.example\"\n")?;
        fs::write(&secrets, "[download]\nusername=\"u\"\npassword=\"p\"\n")?;
        assert!(UpdateSettings::load(&config, &secrets).is_err());
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("f50mgr-update-config-{name}-{}", std::process::id()))
    }
}
