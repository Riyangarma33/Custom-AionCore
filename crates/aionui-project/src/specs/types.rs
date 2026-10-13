use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SpecError {
    #[error("spec not found: {id}")]
    SpecNotFound { id: String },

    #[error("spec already exists: {id}")]
    SpecAlreadyExists { id: String },

    #[error("task not found: {id}")]
    TaskNotFound { id: String },

    #[error("concurrent modification on file '{file}': disk mtime {disk_mtime} differs from expected {expected_mtime}")]
    ConcurrentModification {
        file: String,
        disk_mtime: i64,
        expected_mtime: i64,
    },

    #[error("containment violation: path '{path}' escapes workspace '{workspace}'")]
    ContainmentViolation {
        path: PathBuf,
        workspace: PathBuf,
    },

    #[error("invalid spec file name: {filename}")]
    InvalidFileName { filename: String },

    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("serialization error: {0}")]
    Serialization(String),
}

impl From<(PathBuf, std::io::Error)> for SpecError {
    fn from((path, source): (PathBuf, std::io::Error)) -> Self {
        SpecError::Io { path, source }
    }
}
