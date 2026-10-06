pub mod ar;
pub mod autoupdate;
pub mod backend;
pub mod desktop;
pub mod discover;
pub mod config;
pub mod dirs;
pub mod elf;
pub mod fetch;
pub mod lnk;
pub mod manifest;
pub mod pe;
pub mod progress;
pub mod update;
pub mod register;
pub mod runner;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest: {0}")]
    Manifest(String),
    #[error("{0}")]
    Format(String),
    #[error("скасовано")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;
