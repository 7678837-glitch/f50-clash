# f50mgr

f50mgr is a Magisk module for ZTE F50 / MU300 mobile hotspots. It runs Mihomo,
optionally joins an EasyTier mesh, maintains transparent-proxy rules, and serves
a mobile-friendly local dashboard at `http://192.168.0.1:14180`.

[中文](README.md) · [Deployment guide](docs/deployment.md) ·
[Security policy](SECURITY.md)

## Read this first

> **Only ZTE F50 / MU300 devices are supported.** The current build and device
> tests target Android 13, ARM64, and Magisk root. Do not install it on another
> model without porting and validating the device-specific rules.

> The module changes iptables, policy routing, and DNS. Keep a recoverable
> backup before installation or migration from `box_for_root`.

> Port `14180` uses management-key login, an HttpOnly session cookie, and CSRF
> checks. Mihomo on `14193` uses the same key. Both remain trusted-LAN services;
> never expose them to the public Internet. Both currently use plaintext HTTP;
> keep untrusted clients off the management LAN or enforce client isolation.

## Highlights

- Mihomo process supervision, health checks, self-recovery, and transparent proxying.
- Responsive device, cellular, temperature, traffic, and split-routing diagnostics.
- URL or YAML subscription import with F50-specific DNS and port normalization.
- Bundled MetaCubeXD dashboard and optional EasyTier v2.4.5 mesh status/control.
- A hardened Mihomo update pipeline with official GitHub releases, an optional
  company HTTPS fallback, validation, fail-open activation, and rollback. The
  first public release hard-disables unattended activation until an offline
  signing trust root is configured; there is no unsigned bypass.
- Reversible, backup-first migration from `box_for_root` without deleting old data.
- Pinned third-party assets, deterministic Magisk ZIPs, SHA-256 checksums, and SPDX SBOMs.

## Install a release

Download `f50mgr-YYYYMMDD.N.zip` and its `.sha256` file from
[GitHub Releases](https://github.com/xjoker/f50-clash/releases), then run:

```bash
# Linux (use `shasum -a 256 -c` on macOS)
sha256sum -c f50mgr-YYYYMMDD.N.zip.sha256
adb push f50mgr-YYYYMMDD.N.zip /sdcard/Download/f50mgr.zip
adb shell "su 0 -c 'magisk --install-module /sdcard/Download/f50mgr.zip'"
adb reboot
```

Save the management key printed during first installation. After boot, open
`http://192.168.0.1:14180/` and sign in with that key. If it is lost, run
`adb shell "su 0 -c '/data/adb/f50mgr/bin/f50mgr auth reset'"`. The detailed
operational guide is maintained in Chinese in [docs/deployment.md](docs/deployment.md).

## Build from source

Requirements: Git, Docker, curl, zip, unzip, and network access. A release build
requires a clean worktree.

```bash
git clone https://github.com/xjoker/f50-clash.git
cd f50-clash
./build.sh
```

The outputs are `release/f50mgr-<VERSION>.zip` and its SHA-256 checksum. The
build container, Rust toolchain, Android NDK, third-party versions, and downloaded
assets are pinned and verified before packaging.

## Security and privacy

Real credentials belong only in `/data/adb/f50mgr/config/secrets.toml` with mode
`0600`. Never publish migration backups or logs containing IMEI, ICCID, IMSI,
phone numbers, subscription URLs, or EasyTier secrets. Report vulnerabilities
privately as described in [SECURITY.md](SECURITY.md).

## Contributing and license

See [CONTRIBUTING.md](CONTRIBUTING.md) and
[CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Copyright (C) 2026 xjoker. Project
code is licensed under [GPL-3.0-only](LICENSE); bundled components retain their
own licenses as documented in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
