use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error("invalid DbgAtlas id: {reason}")]
pub struct InvalidId {
    reason: &'static str,
}

impl InvalidId {
    fn new(reason: &'static str) -> Self {
        Self { reason }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Id(String);

impl Id {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidId> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(InvalidId::new("id must not be empty"));
        }
        if value.contains('/') || value.contains('\\') {
            return Err(InvalidId::new("id must not contain path separators"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Id {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for Id {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl FromStr for Id {
    type Err = InvalidId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Timestamp {
    pub unix_millis: u64,
}

impl Timestamp {
    pub fn now() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or(0);
        Self {
            unix_millis: millis.try_into().unwrap_or(u64::MAX),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceRef {
    pub root: PathBuf,
}

impl WorkspaceRef {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

macro_rules! define_ref {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name {
            pub id: Id,
        }

        impl $name {
            pub fn new(id: Id) -> Self {
                Self { id }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.id.fmt(formatter)
            }
        }
    };
}

define_ref!(TargetRef);
define_ref!(SessionRef);
define_ref!(RecordingRef);
define_ref!(ArtifactRef);
define_ref!(OperationRef);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorContext {
    pub code: String,
    pub message: String,
}

impl ErrorContext {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_rejects_empty_and_path_like_values() {
        assert!(Id::new("").is_err());
        assert!(Id::new("a/b").is_err());
        assert!(Id::new("a\\b").is_err());
    }

    #[test]
    fn id_accepts_plain_values() {
        let id = Id::new("artifact-001").unwrap();
        assert_eq!(id.as_str(), "artifact-001");
    }
}
