use std::{net::IpAddr, path::Path};

use serde::Deserialize;

use crate::{FunError, RouteTable};

const MAX_CONCURRENCY: usize = 4096;
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    #[serde(default)]
    pub project: ProjectMetadata,
    #[serde(default)]
    pub dev: DevConfig,
    #[serde(default)]
    pub functions: FunctionConfig,
    #[serde(default)]
    pub routes: Vec<RouteConfig>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectMetadata {
    #[serde(default = "default_project_name")]
    pub name: String,
    #[serde(default = "default_project_version")]
    pub version: String,
}

impl Default for ProjectMetadata {
    fn default() -> Self {
        Self {
            name: default_project_name(),
            version: default_project_version(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DevConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_watch")]
    pub watch: bool,
}

impl Default for DevConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            watch: default_watch(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionConfig {
    #[serde(default = "default_functions_directory")]
    pub directory: String,
    #[serde(default = "default_timeout")]
    pub default_timeout_ms: u64,
    #[serde(default = "default_concurrency")]
    pub max_concurrency: usize,
    #[serde(default = "default_body_limit")]
    pub max_request_body_bytes: usize,
    #[serde(default = "default_body_limit")]
    pub max_response_body_bytes: usize,
}

impl Default for FunctionConfig {
    fn default() -> Self {
        Self {
            directory: default_functions_directory(),
            default_timeout_ms: default_timeout(),
            max_concurrency: default_concurrency(),
            max_request_body_bytes: default_body_limit(),
            max_response_body_bytes: default_body_limit(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteConfig {
    pub path: String,
    pub function: String,
    #[serde(default)]
    pub methods: Vec<String>,
}

impl ProjectConfig {
    pub fn from_file(path: &Path) -> Result<Self, FunError> {
        let contents = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&contents)
            .map_err(|error| FunError::Config(format!("{}: {error}", path.display())))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), FunError> {
        if self.project.name.trim().is_empty() || self.project.version.trim().is_empty() {
            return Err(FunError::Config(
                "project.name and project.version must not be empty".into(),
            ));
        }
        let host: IpAddr = self
            .dev
            .host
            .parse()
            .map_err(|_| FunError::Config("dev.host must be an IP address".into()))?;
        if !host.is_loopback() {
            return Err(FunError::Config(
                "dev.host must be a loopback address (127.0.0.1 or ::1)".into(),
            ));
        }
        if self.dev.port == 0 {
            return Err(FunError::Config(
                "dev.port must be between 1 and 65535".into(),
            ));
        }
        if self.functions.directory.is_empty() {
            return Err(FunError::Config(
                "functions.directory must not be empty".into(),
            ));
        }
        if self.functions.default_timeout_ms == 0 {
            return Err(FunError::Config(
                "functions.default_timeout_ms must be greater than zero".into(),
            ));
        }
        if self.functions.max_concurrency == 0 || self.functions.max_concurrency > MAX_CONCURRENCY {
            return Err(FunError::Config(format!(
                "functions.max_concurrency must be between 1 and {MAX_CONCURRENCY}"
            )));
        }
        if self.functions.max_request_body_bytes == 0
            || self.functions.max_response_body_bytes == 0
            || self.functions.max_request_body_bytes > MAX_BODY_BYTES
            || self.functions.max_response_body_bytes > MAX_BODY_BYTES
        {
            return Err(FunError::Config(format!(
                "request and response body limits must be between 1 and {MAX_BODY_BYTES} bytes"
            )));
        }
        RouteTable::new(&self.routes)?;
        Ok(())
    }
}

fn default_project_name() -> String {
    "my-functions".into()
}

fn default_project_version() -> String {
    "0.1.0".into()
}

fn default_host() -> String {
    "127.0.0.1".into()
}

fn default_port() -> u16 {
    3000
}

fn default_watch() -> bool {
    true
}

fn default_functions_directory() -> String {
    "functions".into()
}

fn default_timeout() -> u64 {
    5000
}

fn default_concurrency() -> usize {
    64
}

fn default_body_limit() -> usize {
    1024 * 1024
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_loopback_and_bounded() {
        let config: ProjectConfig = toml::from_str("").unwrap();
        config.validate().unwrap();
        assert_eq!(config.dev.host, "127.0.0.1");
        assert_eq!(config.functions.max_concurrency, 64);
        assert!(config.dev.watch);
    }

    #[test]
    fn rejects_public_bind_address_and_unknown_fields() {
        let public: ProjectConfig = toml::from_str("[dev]\nhost = \"0.0.0.0\"").unwrap();
        assert!(public.validate().is_err());
        assert!(toml::from_str::<ProjectConfig>("[dev]\nporrt = 9").is_err());
    }

    #[test]
    fn rejects_unbounded_limits() {
        let config: ProjectConfig =
            toml::from_str("[functions]\nmax_concurrency = 999999").unwrap();
        assert!(config.validate().is_err());
        let config: ProjectConfig =
            toml::from_str("[functions]\nmax_request_body_bytes = 999999999").unwrap();
        assert!(config.validate().is_err());
    }
}
