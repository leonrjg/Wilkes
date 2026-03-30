use thiserror::Error;

#[derive(Error, Debug)]
pub enum ApiError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Search error: {0}")]
    Search(String),
    #[error("Extract error: {0}")]
    Extract(String),
    #[error("Settings error: {0}")]
    Settings(String),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
