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
        let raw_url = env::var("VALETUDO_URL").map_err(|_| {
            "VALETUDO_URL is required (e.g. http://valetudo-dreame_vacuum_r2492b.local)".to_string()
        })?;
        // Fail fast on a malformed address rather than surfacing a confusing
        // connection error on every subsequent request.
        let valetudo_url = validate_base_url(&raw_url)?;

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

/// Reject an unusable `VALETUDO_URL` at startup rather than surfacing a
/// confusing connection error on every subsequent request.
///
/// A missing scheme is the common mistake, so a bare `host` or `host:port` is
/// accepted and assumed to be http. The normalised form is rebuilt from the
/// parsed URL rather than by trimming slashes off the input: `http://` trimmed
/// naively becomes `http:/`, which parses as a host called "http".
fn validate_base_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();

    if trimmed.is_empty() {
        return Err("VALETUDO_URL is empty".to_string());
    }

    let candidate = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };

    let parsed = url::Url::parse(&candidate)
        .map_err(|e| format!("VALETUDO_URL {raw:?} is not valid: {e}"))?;

    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!(
                "VALETUDO_URL must use http or https, got {other:?} in {raw:?}"
            ))
        }
    }

    let authority = parsed
        .host_str()
        .map(|h| match parsed.port() {
            Some(port) => format!("{h}:{port}"),
            None => h.to_string(),
        })
        .filter(|a| !a.is_empty())
        .ok_or_else(|| {
            format!(
                "VALETUDO_URL {raw:?} has no host. Use a hostname like \
                 http://valetudo-dreame_vacuum_r2492b.local or an address like \
                 http://192.168.0.46"
            )
        })?;

    // Keep only scheme://authority. Any path in the input is dropped, because
    // callers concatenate "/api/v2/robot/..." onto the result and a leftover
    // path or trailing slash would produce a double slash.
    Ok(format!("{}://{authority}", parsed.scheme()))
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
    fn accepts_hostname_and_ip_forms() {
        assert_eq!(
            validate_base_url("http://vacuum.local").unwrap(),
            "http://vacuum.local"
        );
        assert_eq!(
            validate_base_url("192.168.0.46").unwrap(),
            "http://192.168.0.46",
            "a bare host should default to http"
        );
        assert_eq!(
            validate_base_url("192.168.0.46:80").unwrap(),
            "http://192.168.0.46",
            "the default port is canonicalised away, which is equivalent"
        );
        assert_eq!(
            validate_base_url("http://192.168.0.46:8080").unwrap(),
            "http://192.168.0.46:8080",
            "a non-default port must be preserved"
        );
        assert_eq!(
            validate_base_url("  http://vacuum.local  ").unwrap(),
            "http://vacuum.local",
            "surrounding whitespace should be tolerated"
        );
    }

    #[test]
    fn normalises_away_path_and_trailing_slash() {
        // Callers concatenate "/api/v2/robot/..." so the base must not keep a
        // path or a trailing slash, or the joined URL gets a double slash.
        assert_eq!(
            validate_base_url("https://vacuum.local/").unwrap(),
            "https://vacuum.local"
        );
        assert_eq!(
            validate_base_url("https://vacuum.local:8080/some/path/").unwrap(),
            "https://vacuum.local:8080"
        );
    }

    #[test]
    fn rejects_unusable_urls() {
        assert!(validate_base_url("").is_err());
        assert!(validate_base_url("   ").is_err());
        assert!(validate_base_url("ftp://vacuum").is_err());
        assert!(validate_base_url("file:///etc/passwd").is_err());
        // Regression: naive slash trimming turned "http://" into "http:/",
        // which parses as a host literally named "http".
        assert!(validate_base_url("http://").is_err());
        assert!(validate_base_url("http:///").is_err());
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
