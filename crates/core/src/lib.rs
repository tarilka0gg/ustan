pub mod ar;
pub mod backend;
pub mod desktop;
pub mod dirs;
pub mod elf;
pub mod fetch;
pub mod lnk;
pub mod manifest;
pub mod pe;
pub mod update;
pub mod register;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("manifest: {0}")]
    Manifest(String),
    #[error("{0}")]
    Format(String),
}

pub type Result<T> = std::result::Result<T, Error>;
