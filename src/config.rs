//! Load and validate [`runners.yaml`] into the types defined in [`crate::models`].

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::models::FleetConfig;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config not found at {0} — copy runners.yaml.example to runners.yaml")]
    NotFound(PathBuf),
    #[error("reading {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("parsing {path}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_yaml_ng::Error,
    },
    #[error("duplicate runner group name: {0}")]
    DuplicateGroup(String),
}

pub fn load(path: &Path) -> Result<FleetConfig, Error> {
    if !path.exists() {
        return Err(Error::NotFound(path.to_path_buf()));
    }
    let text = fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let cfg: FleetConfig = serde_yaml_ng::from_str(&text).map_err(|source| Error::Parse {
        path: path.to_path_buf(),
        source,
    })?;

    let mut seen: HashSet<&str> = HashSet::new();
    for g in &cfg.runners {
        if !seen.insert(&g.name) {
            return Err(Error::DuplicateGroup(g.name.clone()));
        }
    }
    Ok(cfg)
}
