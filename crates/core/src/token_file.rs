//! Bearer tokens kept in a file of their own.
//!
//! A credential lives beside the settings rather than inside them, because the
//! settings are served to any client of the HTTP API and a token in them would
//! be handed to whoever asked. One file per credential, readable by the owner
//! only.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;

/// A fresh token: two v4 UUIDs, 244 random bits.
pub fn generate() -> String {
    format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4())
}

/// The token stored at `path`. An empty file is an error rather than an absent
/// token: something wrote it, and an empty credential matches nothing.
pub fn read(path: &Path) -> anyhow::Result<String> {
    let mut file = OpenOptions::new().read(true).open(path)?;
    let mut token = String::new();
    file.read_to_string(&mut token)?;
    let token = token.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("Token file is empty: {}", path.display());
    }
    Ok(token)
}

/// The token at `path`, or `None` when no file is there.
pub fn read_if_present(path: &Path) -> anyhow::Result<Option<String>> {
    match read(path) {
        Ok(token) => Ok(Some(token)),
        Err(_) if !path.exists() => Ok(None),
        Err(error) => Err(error),
    }
}

/// The token at `path`, creating one the first time. A file that exists but
/// cannot be read is an error, never a reason to mint a replacement: that
/// would silently revoke every client holding the old one.
pub fn load_or_create(path: &Path) -> anyhow::Result<String> {
    if let Some(token) = read_if_present(path)? {
        return Ok(token);
    }
    let token = generate();
    write(path, &token)?;
    Ok(token)
}

/// Store `token` at `path`, readable and writable by the owner only.
pub fn write(path: &Path, token: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Remove the token at `path`; an absent file is already the wanted state.
pub fn remove(path: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_created_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        let first = load_or_create(&path).unwrap();
        let second = load_or_create(&path).unwrap();

        assert_eq!(first, second);
        assert!(first.len() >= 64);
        assert_eq!(read(&path).unwrap(), first);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn an_absent_file_is_no_token_and_an_empty_one_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        assert_eq!(read_if_present(&path).unwrap(), None);

        std::fs::write(&path, "\n").unwrap();
        assert!(read_if_present(&path).is_err());
        assert!(load_or_create(&path).is_err());

        remove(&path).unwrap();
        remove(&path).unwrap();
        assert_eq!(read_if_present(&path).unwrap(), None);
    }
}
