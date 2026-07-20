use super::*;
use std::{io, net::Ipv4Addr};

const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

fn ip(index: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(192, 168, 0, index))
}

fn fill(mut seed: u8) -> impl FnMut(&mut [u8]) -> io::Result<()> {
    move |bytes| {
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = seed.wrapping_add(index as u8);
        }
        seed = seed.wrapping_add(1);
        Ok(())
    }
}

fn login_at(core: &AuthCore, source: IpAddr, now: Instant, seed: u8) -> LoginSession {
    core.login_at_with_random(source, KEY, now, fill(seed))
        .expect("正确管理密钥应登录成功")
}

#[test]
fn rejects_short_management_key() {
    assert_eq!(
        AuthCore::new(b"too-short").err(),
        Some(AuthError::WeakManagementKey)
    );
    assert!(AuthCore::new(KEY).is_ok());
}

#[test]
fn constant_time_comparison_checks_length_and_content() {
    assert!(constant_time_eq(b"same", b"same"));
    assert!(!constant_time_eq(b"same", b"samf"));
    assert!(!constant_time_eq(b"same", b"same-longer"));
    assert!(!constant_time_eq(b"same-longer", b"same"));
}

#[test]
fn login_issues_session_and_csrf_without_exposing_management_key() {
    let core = AuthCore::new(KEY).unwrap();
    let now = Instant::now();
    assert_eq!(
        core.login_at_with_random(ip(2), b"wrong-management-key", now, fill(1)),
        Err(AuthError::InvalidCredentials)
    );
    let login = login_at(&core, ip(2), now, 2);
    assert_eq!(login.session_id.len(), 64);
    assert_eq!(login.csrf_nonce.len(), 64);
    assert_ne!(login.session_id, login.csrf_nonce);
    assert!(!login.session_id.contains(std::str::from_utf8(KEY).unwrap()));
}

#[test]
fn session_expires_after_idle_timeout() {
    let core = AuthCore::new(KEY).unwrap();
    let now = Instant::now();
    let login = login_at(&core, ip(2), now, 3);
    assert!(core
        .validate_session_at(
            &login.session_id,
            now + SESSION_IDLE_TIMEOUT - Duration::from_secs(1)
        )
        .is_some());
    assert!(core
        .validate_session_at(
            &login.session_id,
            now + SESSION_IDLE_TIMEOUT * 2 - Duration::from_secs(2)
        )
        .is_some());
    let untouched = login_at(&core, ip(3), now, 4);
    assert!(core
        .validate_session_at(&untouched.session_id, now + SESSION_IDLE_TIMEOUT)
        .is_none());
}

#[test]
fn session_expires_at_absolute_timeout_even_when_active() {
    let core = AuthCore::new(KEY).unwrap();
    let now = Instant::now();
    let login = login_at(&core, ip(2), now, 5);
    for minute in (29..12 * 60).step_by(29) {
        assert!(core
            .validate_session_at(&login.session_id, now + Duration::from_secs(minute * 60))
            .is_some());
    }
    assert!(core
        .validate_session_at(&login.session_id, now + SESSION_ABSOLUTE_TIMEOUT)
        .is_none());
}

#[test]
fn ninth_login_evicts_oldest_session() {
    let core = AuthCore::new(KEY).unwrap();
    let now = Instant::now();
    let mut sessions = Vec::new();
    for index in 0..=MAX_SESSIONS {
        sessions.push(login_at(
            &core,
            ip(2),
            now + Duration::from_secs(index as u64),
            index as u8 + 10,
        ));
    }
    assert!(core
        .validate_session_at(&sessions[0].session_id, now + Duration::from_secs(20))
        .is_none());
    assert!(sessions[1..].iter().all(|session| core
        .validate_session_at(&session.session_id, now + Duration::from_secs(20))
        .is_some()));
}

#[test]
fn csrf_must_match_session_and_invalid_value_does_not_refresh_idle_time() {
    let core = AuthCore::new(KEY).unwrap();
    let now = Instant::now();
    let login = login_at(&core, ip(2), now, 30);
    assert!(!core.validate_csrf_at(
        &login.session_id,
        "wrong-csrf",
        now + SESSION_IDLE_TIMEOUT - Duration::from_secs(1)
    ));
    assert!(!core.validate_csrf_at(
        &login.session_id,
        &login.csrf_nonce,
        now + SESSION_IDLE_TIMEOUT
    ));
    let valid = login_at(&core, ip(3), now, 31);
    assert!(core.validate_csrf_at(
        &valid.session_id,
        &valid.csrf_nonce,
        now + Duration::from_secs(1)
    ));
}

#[test]
fn sixth_failure_from_same_ip_is_rate_limited() {
    let core = AuthCore::new(KEY).unwrap();
    let now = Instant::now();
    for attempt in 0..MAX_FAILURES_PER_IP {
        assert_eq!(
            core.login_at_with_random(ip(2), b"wrong", now, fill(attempt as u8)),
            Err(AuthError::InvalidCredentials)
        );
    }
    assert!(matches!(
        core.login_at_with_random(ip(2), KEY, now, fill(50)),
        Err(AuthError::RateLimited {
            retry_after_secs: 60
        })
    ));
    assert!(core.login_at_with_random(ip(3), KEY, now, fill(51)).is_ok());
}

#[test]
fn global_failure_limit_applies_across_source_ips() {
    let core = AuthCore::new(KEY).unwrap();
    let now = Instant::now();
    for attempt in 0..MAX_FAILURES_GLOBAL {
        assert_eq!(
            core.login_at_with_random(ip((attempt + 2) as u8), b"wrong", now, fill(attempt as u8)),
            Err(AuthError::InvalidCredentials)
        );
    }
    assert!(matches!(
        core.login_at_with_random(ip(250), KEY, now, fill(90)),
        Err(AuthError::RateLimited {
            retry_after_secs: 60
        })
    ));
}

#[test]
fn failure_window_expires_and_success_clears_source_failures() {
    let core = AuthCore::new(KEY).unwrap();
    let now = Instant::now();
    for attempt in 0..MAX_FAILURES_PER_IP {
        let _ = core.login_at_with_random(ip(2), b"wrong", now, fill(attempt as u8));
    }
    assert!(core
        .login_at_with_random(ip(2), KEY, now + FAILURE_WINDOW, fill(100))
        .is_ok());
    assert_eq!(
        core.login_at_with_random(ip(2), b"wrong", now + FAILURE_WINDOW, fill(101)),
        Err(AuthError::InvalidCredentials)
    );
}

#[test]
fn logout_invalidates_only_matching_session() {
    let core = AuthCore::new(KEY).unwrap();
    let now = Instant::now();
    let first = login_at(&core, ip(2), now, 110);
    let second = login_at(&core, ip(3), now, 120);
    assert!(core.logout(&first.session_id));
    assert!(!core.logout(&first.session_id));
    assert!(core.validate_session_at(&first.session_id, now).is_none());
    assert!(core.validate_session_at(&second.session_id, now).is_some());
}

#[test]
fn random_source_failure_does_not_create_session() {
    let core = AuthCore::new(KEY).unwrap();
    let now = Instant::now();
    assert_eq!(
        core.login_at_with_random(ip(2), KEY, now, |_| Err(io::Error::other("no entropy"))),
        Err(AuthError::RandomSource)
    );
}
