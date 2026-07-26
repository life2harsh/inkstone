use std::fmt;

#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub host: String,
    pub port: u16,
    pub dev_auth: bool,
    pub public_base_url: String,
    pub resend_api_key: Option<String>,
    pub resend_from_email: Option<String>,
    pub allowed_origins: Vec<String>,
    pub session_ttl_hours: i64,
    pub cookie_secure: bool,
    pub log_level: String,
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("database_url", &"[REDACTED]")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("dev_auth", &self.dev_auth)
            .field("public_base_url", &self.public_base_url)
            .field(
                "resend_api_key",
                &self.resend_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("resend_from_email", &self.resend_from_email)
            .field("allowed_origins", &self.allowed_origins)
            .field("session_ttl_hours", &self.session_ttl_hours)
            .field("cookie_secure", &self.cookie_secure)
            .field("log_level", &self.log_level)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let dev_auth = parse_bool("DEV_AUTH", false)?;
        let cookie_secure = parse_bool("COOKIE_SECURE", !dev_auth)?;
        let config = Self {
            database_url: std::env::var("DATABASE_URL")
                .unwrap_or_else(|_| "postgres://inkstone:inkstone@localhost:5432/inkstone".into()),
            host: std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".into()),
            port: std::env::var("PORT")
                .unwrap_or_else(|_| "8080".into())
                .parse()
                .map_err(|_| "PORT must be a valid u16".to_string())?,
            dev_auth,
            public_base_url: std::env::var("PUBLIC_BASE_URL").unwrap_or_else(|_| {
                if dev_auth {
                    "http://localhost:8080".into()
                } else {
                    String::new()
                }
            }),
            resend_api_key: optional_env("RESEND_API_KEY")?,
            resend_from_email: optional_env("RESEND_FROM_EMAIL")?,
            allowed_origins: std::env::var("ALLOWED_ORIGINS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect(),
            session_ttl_hours: std::env::var("SESSION_TTL_HOURS")
                .unwrap_or_else(|_| "720".into())
                .parse()
                .map_err(|_| "SESSION_TTL_HOURS must be an integer".to_string())?,
            cookie_secure,
            log_level: std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".into()),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.database_url.trim().is_empty() {
            return Err("DATABASE_URL cannot be empty".into());
        }
        if self.host.trim().is_empty() {
            return Err("HOST cannot be empty".into());
        }
        if self.session_ttl_hours <= 0 || self.session_ttl_hours > 24 * 365 {
            return Err("SESSION_TTL_HOURS must be between 1 and 8760".into());
        }
        if !self.dev_auth && self.allowed_origins.is_empty() {
            return Err("ALLOWED_ORIGINS is required when DEV_AUTH is false".into());
        }
        let public_url = reqwest::Url::parse(&self.public_base_url)
            .map_err(|_| "PUBLIC_BASE_URL must be an absolute HTTP(S) URL".to_string())?;
        if !matches!(public_url.scheme(), "http" | "https")
            || public_url.host_str().is_none()
            || public_url.query().is_some()
            || public_url.fragment().is_some()
        {
            return Err(
                "PUBLIC_BASE_URL must be an absolute HTTP(S) URL without a query or fragment"
                    .into(),
            );
        }
        if !self.dev_auth
            && public_url.scheme() != "https"
            && !is_loopback_http_origin(self.public_base_url.trim_end_matches('/'))
        {
            return Err("PUBLIC_BASE_URL must use HTTPS except on loopback".into());
        }
        if self.resend_api_key.is_some() != self.resend_from_email.is_some() {
            return Err("RESEND_API_KEY and RESEND_FROM_EMAIL must be configured together".into());
        }
        if !self.dev_auth && self.resend_api_key.is_none() {
            return Err("RESEND_API_KEY and RESEND_FROM_EMAIL are required in production".into());
        }
        if self.allowed_origins.iter().any(|origin| {
            !(origin.starts_with("https://")
                || (self.dev_auth && origin.starts_with("http://"))
                || is_loopback_http_origin(origin))
                || origin.ends_with('/')
        }) {
            return Err(
                "ALLOWED_ORIGINS entries must be HTTPS or loopback HTTP origins without a trailing slash"
                    .into(),
            );
        }
        if !self.cookie_secure
            && !self.dev_auth
            && !self
                .allowed_origins
                .iter()
                .all(|origin| is_loopback_http_origin(origin))
        {
            return Err(
                "COOKIE_SECURE may be false only for DEV_AUTH or loopback HTTP origins".into(),
            );
        }
        Ok(())
    }
}

fn is_loopback_http_origin(origin: &str) -> bool {
    let Some(authority) = origin.strip_prefix("http://") else {
        return false;
    };
    for host in ["localhost", "127.0.0.1", "[::1]"] {
        if authority == host {
            return true;
        }
        if let Some(port) = authority
            .strip_prefix(host)
            .and_then(|rest| rest.strip_prefix(':'))
        {
            return port.parse::<u16>().is_ok_and(|port| port != 0);
        }
    }
    false
}

fn parse_bool(name: &str, default: bool) -> Result<bool, String> {
    match std::env::var(name) {
        Ok(value) => value
            .parse()
            .map_err(|_| format!("{name} must be true or false")),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(error) => Err(format!("failed to read {name}: {error}")),
    }
}

fn optional_env(name: &str) -> Result<Option<String>, String> {
    match std::env::var(name) {
        Ok(value) if value.trim().is_empty() => Ok(None),
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(format!("failed to read {name}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    fn production_config() -> Config {
        Config {
            database_url: "postgres://user:secret@db/inkstone".into(),
            host: "0.0.0.0".into(),
            port: 8080,
            dev_auth: false,
            public_base_url: "https://inkstone.example".into(),
            resend_api_key: Some("re_secret".into()),
            resend_from_email: Some("Inkstone <setup@inkstone.example>".into()),
            allowed_origins: vec!["https://inkstone.example".into()],
            session_ttl_hours: 24,
            cookie_secure: true,
            log_level: "info".into(),
        }
    }

    #[test]
    fn debug_redacts_database_url() {
        let output = format!("{:?}", production_config());
        assert!(output.contains("[REDACTED]"));
        assert!(!output.contains("secret"));
        assert!(!output.contains("re_secret"));
    }

    #[test]
    fn production_configuration_fails_closed() {
        let mut config = production_config();
        config.cookie_secure = false;
        assert!(config.validate().is_err());
        config.cookie_secure = true;
        config.allowed_origins.clear();
        assert!(config.validate().is_err());
        config.allowed_origins = vec!["https://inkstone.example".into()];
        config.resend_api_key = None;
        assert!(config.validate().is_err());
        config.resend_from_email = None;
        assert!(config.validate().is_err());
    }

    #[test]
    fn insecure_cookies_are_limited_to_loopback_http_origins() {
        let mut config = production_config();
        config.cookie_secure = false;
        config.allowed_origins = vec!["http://localhost:8080".into(), "http://[::1]:8080".into()];
        assert!(config.validate().is_ok());

        config.allowed_origins = vec!["http://localhost.example:8080".into()];
        assert!(config.validate().is_err());
        config.allowed_origins = vec!["https://inkstone.example".into()];
        assert!(config.validate().is_err());
    }
}
