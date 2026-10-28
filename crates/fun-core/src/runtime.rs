use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::FunError;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeKind {
    #[default]
    Rust,
    #[serde(rename = "nodejs")]
    NodeJs,
    #[serde(rename = "python3")]
    Python3,
    #[serde(rename = "go1.x")]
    Go1x,
    Provided,
}

impl RuntimeKind {
    pub fn identifier(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::NodeJs => "nodejs",
            Self::Python3 => "python3",
            Self::Go1x => "go1.x",
            Self::Provided => "provided",
        }
    }
}

impl fmt::Display for RuntimeKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.identifier())
    }
}

impl FromStr for RuntimeKind {
    type Err = FunError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "rust" => Ok(Self::Rust),
            "nodejs" => Ok(Self::NodeJs),
            "python3" => Ok(Self::Python3),
            "go1.x" => Ok(Self::Go1x),
            "provided" => Ok(Self::Provided),
            _ => Err(FunError::Config(format!(
                "unsupported function runtime `{value}`; expected rust, nodejs, python3, go1.x, or provided"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_displays_supported_runtime_identifiers() {
        for runtime in [
            RuntimeKind::Rust,
            RuntimeKind::NodeJs,
            RuntimeKind::Python3,
            RuntimeKind::Go1x,
            RuntimeKind::Provided,
        ] {
            assert_eq!(runtime.to_string().parse::<RuntimeKind>().unwrap(), runtime);
        }
        assert!("nodejs22.x".parse::<RuntimeKind>().is_err());
    }
}
