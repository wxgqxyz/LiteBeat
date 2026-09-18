use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    pub storage: StorageConfig,
    pub library: LibraryConfig,
    #[serde(default)]
    pub limits: LimitsConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_web_dir")]
    pub web_dir: PathBuf,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            web_dir: default_web_dir(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageConfig {
    pub data_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LibraryConfig {
    #[serde(default)]
    pub roots: Vec<LibraryRootConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LibraryRootConfig {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LimitsConfig {
    #[serde(default = "default_api_inflight")]
    pub api_inflight: usize,
    #[serde(default = "default_media_inflight")]
    pub media_inflight: usize,
    #[serde(default = "default_media_per_session")]
    pub media_per_session: usize,
    #[serde(default = "default_media_chunk_bytes")]
    pub media_chunk_bytes: usize,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            api_inflight: default_api_inflight(),
            media_inflight: default_media_inflight(),
            media_per_session: default_media_per_session(),
            media_chunk_bytes: default_media_chunk_bytes(),
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("invalid server.bind: {0}")]
    InvalidBind(#[from] std::net::AddrParseError),
    #[error("server.bind must use a non-zero port")]
    ZeroPort,
    #[error("storage.data_dir must not be empty")]
    EmptyDataDir,
    #[error("library root '{0}' does not exist or is not a directory")]
    InvalidLibraryRoot(String),
    #[error("server.web_dir does not exist or is not a directory: {0}")]
    InvalidWebDir(PathBuf),
    #[error("{0} must be greater than zero")]
    NonPositiveLimit(&'static str),
}

impl Config {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let text = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    pub fn validate(&self) -> Result<SocketAddr, ConfigError> {
        let address: SocketAddr = self.server.bind.parse()?;
        if address.port() == 0 {
            return Err(ConfigError::ZeroPort);
        }
        if self.storage.data_dir.as_os_str().is_empty() {
            return Err(ConfigError::EmptyDataDir);
        }
        if !self.server.web_dir.is_dir() {
            return Err(ConfigError::InvalidWebDir(self.server.web_dir.clone()));
        }
        let web_dir = std::fs::canonicalize(&self.server.web_dir)
            .map_err(|_| ConfigError::InvalidWebDir(self.server.web_dir.clone()))?;
        let data_dir = std::fs::canonicalize(&self.storage.data_dir)
            .unwrap_or_else(|_| self.storage.data_dir.clone());
        let mut root_paths = Vec::with_capacity(self.library.roots.len());
        let mut root_names = std::collections::HashSet::new();
        for root in &self.library.roots {
            if root.name.trim().is_empty() || !root_names.insert(&root.name) {
                return Err(ConfigError::InvalidLibraryRoot(root.name.clone()));
            }
            if !root.path.is_dir() {
                return Err(ConfigError::InvalidLibraryRoot(root.name.clone()));
            }
            let root_path = std::fs::canonicalize(&root.path)
                .map_err(|_| ConfigError::InvalidLibraryRoot(root.name.clone()))?;
            if web_dir.starts_with(&root_path)
                || root_path.starts_with(&web_dir)
                || data_dir.starts_with(&root_path)
                || root_path.starts_with(&data_dir)
            {
                return Err(ConfigError::InvalidLibraryRoot(root.name.clone()));
            }
            root_paths.push(root_path);
        }
        if web_dir.starts_with(&data_dir) || data_dir.starts_with(&web_dir) {
            return Err(ConfigError::InvalidWebDir(self.server.web_dir.clone()));
        }
        for (index, path) in root_paths.iter().enumerate() {
            if root_paths[..index]
                .iter()
                .any(|other| path.starts_with(other) || other.starts_with(path))
            {
                return Err(ConfigError::InvalidLibraryRoot(
                    self.library.roots[index].name.clone(),
                ));
            }
        }
        for (name, value) in [
            ("limits.api_inflight", self.limits.api_inflight),
            ("limits.media_inflight", self.limits.media_inflight),
            ("limits.media_per_session", self.limits.media_per_session),
            ("limits.media_chunk_bytes", self.limits.media_chunk_bytes),
        ] {
            if value == 0 {
                return Err(ConfigError::NonPositiveLimit(name));
            }
        }
        Ok(address)
    }
}

fn default_bind() -> String {
    "127.0.0.1:8090".into()
}
fn default_web_dir() -> PathBuf {
    "web/dist".into()
}
fn default_api_inflight() -> usize {
    32
}
fn default_media_inflight() -> usize {
    32
}
fn default_media_per_session() -> usize {
    4
}
fn default_media_chunk_bytes() -> usize {
    32 * 1024
}
