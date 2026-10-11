use std::collections::HashMap;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const COOKIE: &str = "ccsw_session";
pub const SESSION_LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);
pub const PAIRING_LIFETIME: Duration = Duration::from_secs(5 * 60);

pub fn secret() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

fn digest(value: &str) -> [u8; 32] {
    Sha256::digest(value.as_bytes()).into()
}

#[derive(Clone)]
pub struct Session {
    pub id: String,
    pub csrf: String,
    pub expires: Instant,
    pub expires_at: i64,
}

pub struct Auth {
    pairing: Option<([u8; 32], Instant)>,
    sessions: HashMap<[u8; 32], Session>,
    attempts: (Instant, u32),
}

impl Auth {
    pub fn new() -> Self {
        Self {
            pairing: None,
            sessions: HashMap::new(),
            attempts: (Instant::now(), 0),
        }
    }

    pub fn rotate_pairing(&mut self) -> String {
        let code = secret();
        self.pairing = Some((digest(&code), Instant::now() + PAIRING_LIFETIME));
        code
    }

    pub fn pair(&mut self, code: &str, now: Instant) -> Result<(String, Session), &'static str> {
        if now.duration_since(self.attempts.0) >= Duration::from_secs(60) {
            self.attempts = (now, 0);
        }
        if self.attempts.1 >= 10 {
            return Err("pairing_rate_limited");
        }
        self.attempts.1 += 1;
        let valid = self.pairing.as_ref().is_some_and(|(expected, expires)| {
            now < *expires && code.len() == 64 && bool::from(expected.ct_eq(&digest(code)))
        });
        if !valid {
            return Err("pairing_invalid");
        }
        self.sessions.retain(|_, session| session.expires > now);
        if self.sessions.len() >= 32 {
            return Err("session_limit");
        }
        self.pairing = None;
        let token = secret();
        let session = Session {
            id: secret(),
            csrf: secret(),
            expires: now + SESSION_LIFETIME,
            expires_at: crate::model::now_unix() + SESSION_LIFETIME.as_secs() as i64,
        };
        self.sessions.insert(digest(&token), session.clone());
        Ok((token, session))
    }

    pub fn session(&mut self, token: &str) -> Option<Session> {
        let now = Instant::now();
        self.sessions.retain(|_, session| session.expires > now);
        self.sessions.get(&digest(token)).cloned()
    }

    pub fn valid(&self, session: &Session) -> bool {
        session.expires > Instant::now() && self.sessions.values().any(|s| s.id == session.id)
    }

    pub fn revoke(&mut self, session: &Session) {
        self.sessions.retain(|_, s| s.id != session.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_is_one_use_and_sessions_can_be_revoked() {
        let mut auth = Auth::new();
        let code = auth.rotate_pairing();
        let (token, session) = auth.pair(&code, Instant::now()).unwrap();
        assert!(auth.pair(&code, Instant::now()).is_err());
        assert!(auth.session(&token).is_some());
        assert!(auth.session(&session.csrf).is_none());
        auth.revoke(&session);
        assert!(!auth.valid(&session));
        assert!(auth.session(&token).is_none());
    }

    #[test]
    fn pairing_expires_and_repeated_guesses_are_limited() {
        let mut auth = Auth::new();
        let code = auth.rotate_pairing();
        assert!(auth.pair(&code, Instant::now() + PAIRING_LIFETIME).is_err());
        let mut auth = Auth::new();
        for _ in 0..10 {
            assert!(matches!(
                auth.pair("wrong", Instant::now()),
                Err("pairing_invalid")
            ));
        }
        assert!(matches!(
            auth.pair("wrong", Instant::now()),
            Err("pairing_rate_limited")
        ));
    }

    #[test]
    fn expired_sessions_are_not_accepted() {
        let mut auth = Auth::new();
        let code = auth.rotate_pairing();
        let (token, session) = auth.pair(&code, Instant::now()).unwrap();
        auth.sessions.get_mut(&digest(&token)).unwrap().expires = Instant::now();
        assert!(auth.session(&token).is_none());
        assert!(!auth.valid(&session));
    }
}
