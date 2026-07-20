use std::{
    collections::{HashMap, VecDeque},
    fs::File,
    io::Read,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

pub const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const SESSION_ABSOLUTE_TIMEOUT: Duration = Duration::from_secs(12 * 60 * 60);
pub const MAX_SESSIONS: usize = 8;
pub const FAILURE_WINDOW: Duration = Duration::from_secs(60);
pub const MAX_FAILURES_PER_IP: usize = 5;
pub const MAX_FAILURES_GLOBAL: usize = 30;
const MIN_MANAGEMENT_KEY_BYTES: usize = 16;
const TOKEN_BYTES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    WeakManagementKey,
    InvalidCredentials,
    RateLimited { retry_after_secs: u64 },
    RandomSource,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginSession {
    pub session_id: String,
    pub csrf_nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedSession {
    pub csrf_nonce: String,
}

pub struct AuthCore {
    management_key: Vec<u8>,
    state: Mutex<AuthState>,
}

#[derive(Default)]
struct AuthState {
    sessions: Vec<Session>,
    failures_by_ip: HashMap<IpAddr, VecDeque<Instant>>,
    global_failures: VecDeque<Instant>,
}

struct Session {
    session_id: String,
    csrf_nonce: String,
    created_at: Instant,
    last_seen: Instant,
}

impl AuthCore {
    pub fn new(management_key: &[u8]) -> Result<Self, AuthError> {
        if management_key.len() < MIN_MANAGEMENT_KEY_BYTES {
            return Err(AuthError::WeakManagementKey);
        }
        Ok(Self {
            management_key: management_key.to_vec(),
            state: Mutex::new(AuthState::default()),
        })
    }

    pub fn login(&self, source: IpAddr, presented_key: &[u8]) -> Result<LoginSession, AuthError> {
        self.login_at_with_random(source, presented_key, Instant::now(), fill_os_random)
    }

    pub fn validate_session(&self, session_id: &str) -> Option<AuthenticatedSession> {
        self.validate_session_at(session_id, Instant::now())
    }

    pub fn validate_csrf(&self, session_id: &str, csrf_nonce: &str) -> bool {
        self.validate_csrf_at(session_id, csrf_nonce, Instant::now())
    }

    pub fn has_session(&self, session_id: &str) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        state.purge_expired_sessions(Instant::now());
        matching_session_index(&state.sessions, session_id).is_some()
    }

    pub fn logout(&self, session_id: &str) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let Some(index) = matching_session_index(&state.sessions, session_id) else {
            return false;
        };
        state.sessions.swap_remove(index);
        true
    }

    fn login_at_with_random(
        &self,
        source: IpAddr,
        presented_key: &[u8],
        now: Instant,
        mut fill_random: impl FnMut(&mut [u8]) -> std::io::Result<()>,
    ) -> Result<LoginSession, AuthError> {
        let mut state = self.state.lock().map_err(|_| AuthError::Unavailable)?;
        state.purge(now);
        if let Some(retry_after_secs) = state.retry_after_secs(source, now) {
            return Err(AuthError::RateLimited { retry_after_secs });
        }
        if !constant_time_eq(&self.management_key, presented_key) {
            state.record_failure(source, now);
            return Err(AuthError::InvalidCredentials);
        }

        let session_id = generate_unique_token(&state.sessions, &mut fill_random)?;
        let csrf_nonce = loop {
            let candidate = generate_token(&mut fill_random)?;
            if !constant_time_eq(candidate.as_bytes(), session_id.as_bytes()) {
                break candidate;
            }
        };
        state.failures_by_ip.remove(&source);
        state.purge_expired_sessions(now);
        if state.sessions.len() >= MAX_SESSIONS {
            let oldest = state
                .sessions
                .iter()
                .enumerate()
                .min_by_key(|(_, session)| session.created_at)
                .map(|(index, _)| index)
                .expect("会话达到上限时必须存在最旧项");
            state.sessions.swap_remove(oldest);
        }
        state.sessions.push(Session {
            session_id: session_id.clone(),
            csrf_nonce: csrf_nonce.clone(),
            created_at: now,
            last_seen: now,
        });
        Ok(LoginSession {
            session_id,
            csrf_nonce,
        })
    }

    fn validate_session_at(&self, session_id: &str, now: Instant) -> Option<AuthenticatedSession> {
        let mut state = self.state.lock().ok()?;
        state.purge_expired_sessions(now);
        let index = matching_session_index(&state.sessions, session_id)?;
        let session = &mut state.sessions[index];
        session.last_seen = now;
        Some(AuthenticatedSession {
            csrf_nonce: session.csrf_nonce.clone(),
        })
    }

    fn validate_csrf_at(&self, session_id: &str, csrf_nonce: &str, now: Instant) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        state.purge_expired_sessions(now);
        let Some(index) = matching_session_index(&state.sessions, session_id) else {
            return false;
        };
        if !constant_time_eq(
            state.sessions[index].csrf_nonce.as_bytes(),
            csrf_nonce.as_bytes(),
        ) {
            return false;
        }
        state.sessions[index].last_seen = now;
        true
    }
}

impl AuthState {
    fn purge(&mut self, now: Instant) {
        self.purge_expired_sessions(now);
        purge_failures(&mut self.global_failures, now);
        self.failures_by_ip.retain(|_, failures| {
            purge_failures(failures, now);
            !failures.is_empty()
        });
    }

    fn purge_expired_sessions(&mut self, now: Instant) {
        self.sessions.retain(|session| {
            now.saturating_duration_since(session.created_at) < SESSION_ABSOLUTE_TIMEOUT
                && now.saturating_duration_since(session.last_seen) < SESSION_IDLE_TIMEOUT
        });
    }

    fn retry_after_secs(&self, source: IpAddr, now: Instant) -> Option<u64> {
        let ip_retry = self
            .failures_by_ip
            .get(&source)
            .filter(|failures| failures.len() >= MAX_FAILURES_PER_IP)
            .and_then(|failures| retry_after_secs(failures, now));
        let global_retry = (self.global_failures.len() >= MAX_FAILURES_GLOBAL)
            .then(|| retry_after_secs(&self.global_failures, now))
            .flatten();
        ip_retry.into_iter().chain(global_retry).max()
    }

    fn record_failure(&mut self, source: IpAddr, now: Instant) {
        self.failures_by_ip
            .entry(source)
            .or_default()
            .push_back(now);
        self.global_failures.push_back(now);
    }
}

fn purge_failures(failures: &mut VecDeque<Instant>, now: Instant) {
    while failures
        .front()
        .is_some_and(|failure| now.saturating_duration_since(*failure) >= FAILURE_WINDOW)
    {
        failures.pop_front();
    }
}

fn retry_after_secs(failures: &VecDeque<Instant>, now: Instant) -> Option<u64> {
    let elapsed = now.saturating_duration_since(*failures.front()?);
    let remaining = FAILURE_WINDOW.saturating_sub(elapsed);
    Some(
        remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() > 0))
            .max(1),
    )
}

fn matching_session_index(sessions: &[Session], session_id: &str) -> Option<usize> {
    let mut matched = None;
    for (index, session) in sessions.iter().enumerate() {
        if constant_time_eq(session.session_id.as_bytes(), session_id.as_bytes()) {
            matched = Some(index);
        }
    }
    matched
}

fn generate_unique_token(
    sessions: &[Session],
    fill_random: &mut impl FnMut(&mut [u8]) -> std::io::Result<()>,
) -> Result<String, AuthError> {
    for _ in 0..16 {
        let candidate = generate_token(fill_random)?;
        if sessions
            .iter()
            .all(|session| !constant_time_eq(session.session_id.as_bytes(), candidate.as_bytes()))
        {
            return Ok(candidate);
        }
    }
    Err(AuthError::RandomSource)
}

fn generate_token(
    fill_random: &mut impl FnMut(&mut [u8]) -> std::io::Result<()>,
) -> Result<String, AuthError> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    fill_random(&mut bytes).map_err(|_| AuthError::RandomSource)?;
    Ok(hex_encode(&bytes))
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

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        let left_byte = left.get(index).copied().unwrap_or_default();
        let right_byte = right.get(index).copied().unwrap_or_default();
        difference |= usize::from(left_byte ^ right_byte);
    }
    std::hint::black_box(difference == 0)
}

#[cfg(test)]
mod tests;
