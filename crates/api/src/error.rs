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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_api_error_formatting() {
        let err = ApiError::Search("something went wrong".to_string());
        assert_eq!(format!("{err}"), "Search error: something went wrong");

        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err = ApiError::from(io_err);
        assert!(format!("{err}").contains("IO error: file not found"));
    }
}
