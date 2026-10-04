use std::env;

/// Runtime configuration, all sourced from environment variables so the
/// container needs no config file.
#[derive(Clone, Debug)]
pub struct Config {
    /// Base URL of the Valetudo instance, e.g. `http://192.0.2.46`.
    pub valetudo_url: String,
    /// Optional HTTP basic auth credentials for the Valetudo webserver.
    pub valetudo_username: Option<String>,
    pub valetudo_password: Option<String>,
    /// Address to bind the HTTP listener on.
    pub bind: String,
    /// Optional username required to reach *this* service. When set, a browser
    /// or curl will get a 401 without it. Valetudo's own basic auth is separate
    /// and always forwarded.
    pub web_username: Option<String>,
    pub web_password: Option<String>,
    /// How often to poll Valetudo for state as a fallback when SSE is
    /// unavailable. Milliseconds.
    pub poll_interval_ms: u64,
    pub request_timeout_secs: u64,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let valetudo_url = env::var("VALETUDO_URL")
            .map_err(|_| "VALETUDO_URL is required (e.g. http://192.0.2.46)".to_string())?;
        let valetudo_url = valetudo_url.trim_end_matches('/').to_string();

        let bind = env::var("BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

        // Reject a username without a password (or vice versa) rather than
        // silently serving an unprotected or unusable endpoint.
        let web_username = optional("WEB_USERNAME");
        let web_password = optional("WEB_PASSWORD");
        match (&web_username, &web_password) {
            (Some(_), None) => {
                return Err("WEB_USERNAME is set but WEB_PASSWORD is not".to_string())
            }
            (None, Some(_)) => {
                return Err("WEB_PASSWORD is set but WEB_USERNAME is not".to_string())
            }
            _ => {}
        }

        Ok(Self {
            valetudo_url,
            valetudo_username: optional("VALETUDO_USERNAME"),
            valetudo_password: optional("VALETUDO_PASSWORD"),
            bind,
            web_username,
            web_password,
            poll_interval_ms: parse_or("POLL_INTERVAL_MS", 2000),
            request_timeout_secs: parse_or("REQUEST_TIMEOUT_SECS", 10),
        })
    }

    /// True when this service requires its own basic auth in front of it.
    pub fn web_auth_enabled(&self) -> bool {
        self.web_username.is_some()
    }

    /// Check a `Authorization: Basic` header value against the configured
    /// web credentials. Returns true when no auth is configured.
    pub fn check_web_auth(&self, header: Option<&str>) -> bool {
        let (Some(expected_user), Some(expected_pass)) = (&self.web_username, &self.web_password)
        else {
            return true;
        };

        let Some(header) = header else {
            return false;
        };
        let Some(encoded) = header.strip_prefix("Basic ") else {
            return false;
        };
        let Ok(decoded) = base64_decode(encoded.trim()) else {
            return false;
        };
        let Ok(provided) = String::from_utf8(decoded) else {
            return false;
        };

        // Compare both parts in constant time to avoid leaking length or prefix
        // information through timing.
        let mut parts = provided.splitn(2, ':');
        let user = parts.next().unwrap_or_default();
        let pass = parts.next().unwrap_or_default();

        const_time_eq(user.as_bytes(), expected_user.as_bytes())
            && const_time_eq(pass.as_bytes(), expected_pass.as_bytes())
    }
}

fn optional(key: &str) -> Option<String> {
    env::var(key).ok().filter(|v| !v.trim().is_empty())
}

fn parse_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn base64_decode(input: &str) -> Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(input)
}

/// XOR-compare two byte slices without an early return on the first mismatch.
fn const_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn const_time_eq_matches_and_rejects() {
        assert!(const_time_eq(b"abc", b"abc"));
        assert!(!const_time_eq(b"abc", b"abd"));
        assert!(!const_time_eq(b"abc", b"ab"));
        assert!(const_time_eq(b"", b""));
    }

    #[test]
    fn web_auth_open_when_unconfigured() {
        let cfg = Config {
            valetudo_url: "http://x".into(),
            valetudo_username: None,
            valetudo_password: None,
            bind: "0.0.0.0:8080".into(),
            web_username: None,
            web_password: None,
            poll_interval_ms: 2000,
            request_timeout_secs: 10,
        };
        assert!(cfg.check_web_auth(None));
        assert!(cfg.check_web_auth(Some("Basic nonsense")));
    }
}
