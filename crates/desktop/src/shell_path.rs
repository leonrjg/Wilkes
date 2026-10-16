//! Finder launches do not inherit the user's shell PATH. Resolve it once before
//! the desktop starts threads, so dependency probes and children agree on it.

use std::ffi::{CStr, OsString};
use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};

const TIMEOUT: Duration = Duration::from_secs(2);
const MAX_OUTPUT: usize = 64 * 1024;
const PATH_COMMAND: &str = "printf '\\000WILKES_PATH\\000%s\\000' \"$PATH\"";

/// Call only during single-threaded desktop startup, before creating runtimes.
pub(super) fn initialize() -> anyhow::Result<()> {
    let shell = login_shell()?;
    let mut command = Command::new(shell);
    // Interactive login mode includes version managers configured in .zshrc.
    command.args(["-ilc", PATH_COMMAND]);
    let path = resolve_path(command, TIMEOUT)?;
    std::env::set_var("PATH", path);
    Ok(())
}

fn login_shell() -> anyhow::Result<PathBuf> {
    // Read the account's configured shell rather than assuming zsh or a Node
    // installation location. getpwuid_r does not borrow libc's shared buffer.
    let mut buffer = vec![0u8; 1024];
    loop {
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        // SAFETY: entry and buffer are writable for their declared sizes, and
        // result is checked before accessing the initialized account record.
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && buffer.len() < MAX_OUTPUT {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status)).context("read login shell");
        }
        if result.is_null() {
            bail!("No account record for the current user");
        }
        // SAFETY: getpwuid_r succeeded and returned a record. pw_shell is a
        // NUL-terminated string in buffer, copied before buffer is dropped.
        let shell = unsafe {
            let entry = entry.assume_init();
            if entry.pw_shell.is_null() {
                bail!("The account has no login shell");
            }
            OsString::from_vec(CStr::from_ptr(entry.pw_shell).to_bytes().to_vec())
        };
        let shell = PathBuf::from(shell);
        if !shell.is_absolute() {
            bail!("The account login shell is not an absolute path");
        }
        return Ok(shell);
    }
}

fn resolve_path(mut command: Command, timeout: Duration) -> anyhow::Result<OsString> {
    let (mut reader, writer) = UnixStream::pair().context("create shell output channel")?;
    reader.set_nonblocking(true)?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::null())
        .process_group(0);
    let mut child = command.spawn().context("start login shell")?;
    // Command retains its configured stdout descriptor; close the parent's copy.
    drop(command);
    let deadline = Instant::now() + timeout;
    let result = (|| {
        let mut output = Vec::new();
        loop {
            // Check exit first, then drain: all output from an exited shell is
            // already available, without waiting for inherited descriptors.
            let status = child.try_wait()?;
            let mut bytes = [0; 4096];
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(count) => {
                        if output.len() + count > MAX_OUTPUT {
                            bail!("Login shell output exceeded {MAX_OUTPUT} bytes");
                        }
                        output.extend_from_slice(&bytes[..count]);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => return Err(error).context("read login shell output"),
                }
            }
            if let Some(status) = status {
                if !status.success() {
                    bail!("Login shell exited with {status}");
                }
                return parse_path(&output);
            }
            if Instant::now() >= deadline {
                bail!("Login shell PATH lookup timed out");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    })();
    // Kill the entire group: shell startup files may leave descendants holding
    // stdout open. Reap the shell on success, timeout, and every error path.
    // SAFETY: the child was placed in its own group, whose ID is its process ID.
    unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
    let _ = child.wait();
    result
}

fn parse_path(output: &[u8]) -> anyhow::Result<OsString> {
    let marker = b"\0WILKES_PATH\0";
    let start = output
        .windows(marker.len())
        .position(|window| window == marker)
        .context("Login shell did not return a PATH marker")?
        + marker.len();
    let path = output[start..]
        .split(|byte| *byte == 0)
        .next()
        .filter(|path| !path.is_empty() && output[start + path.len()..].first() == Some(&0))
        .context("Login shell returned an empty or incomplete PATH")?;
    Ok(OsString::from_vec(path.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn script(source: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", source]);
        command
    }

    #[test]
    fn gui_environment_finds_node_and_npx_from_interactive_login_configuration() {
        let dir = tempdir().unwrap();
        let bin = dir.path().join("Node tools with spaces ü");
        std::fs::create_dir(&bin).unwrap();
        for name in ["node", "npx"] {
            let path = bin.join(name);
            std::fs::write(&path, "#!/bin/sh\nprintf 'found'\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(dir.path().join(".zprofile"), "export PATH=/usr/bin:/bin\n").unwrap();
        std::fs::write(
            dir.path().join(".zshrc"),
            "echo 'shell startup noise'\nexport PATH=\"$TEST_BIN:$PATH\"\n",
        )
        .unwrap();
        let mut command = Command::new("/bin/zsh");
        command
            .args(["-ilc", PATH_COMMAND])
            .env_clear()
            .env("HOME", dir.path())
            .env("ZDOTDIR", dir.path())
            .env("TEST_BIN", &bin)
            .env("PATH", "/usr/bin:/bin");
        let path = resolve_path(command, TIMEOUT).unwrap();
        assert_eq!(std::env::split_paths(&path).next().unwrap(), bin);
        for name in ["node", "npx"] {
            let output = Command::new(name).env("PATH", &path).output().unwrap();
            assert!(output.status.success());
            assert_eq!(output.stdout, b"found");
        }
    }

    #[test]
    fn path_preserves_non_utf8_bytes_and_ignores_shell_output() {
        let output = b"welcome\n\0WILKES_PATH\0/tmp/tools\xff:/usr/bin\0goodbye\n";
        assert_eq!(
            parse_path(output).unwrap(),
            OsString::from_vec(b"/tmp/tools\xff:/usr/bin".to_vec())
        );
    }

    #[test]
    fn rejects_missing_empty_and_incomplete_path() {
        for output in [
            b"/usr/bin".as_slice(),
            b"\0WILKES_PATH\0\0",
            b"\0WILKES_PATH\0/usr/bin",
        ] {
            assert!(parse_path(output).is_err());
        }
    }

    #[test]
    fn shell_failure_does_not_accept_a_partial_result() {
        let command = script("printf '\\000WILKES_PATH\\000/usr/bin\\000'; exit 1");
        let error = resolve_path(command, TIMEOUT).unwrap_err();
        assert!(error.to_string().contains("exited"));
    }

    #[test]
    fn shell_timeout_is_bounded_even_with_a_descendant_holding_stdout() {
        let started = Instant::now();
        let error =
            resolve_path(script("sleep 30 & wait"), Duration::from_millis(100)).unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn exited_shell_does_not_wait_for_background_stdout_to_close() {
        let started = Instant::now();
        let path = resolve_path(
            script("sleep 30 & printf '\\000WILKES_PATH\\000/usr/bin\\000'"),
            TIMEOUT,
        )
        .unwrap();
        assert_eq!(path, OsString::from("/usr/bin"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn excessive_startup_output_is_bounded() {
        let command = script("while :; do printf 'unbounded shell startup output'; done");
        let error = resolve_path(command, TIMEOUT).unwrap_err();
        assert!(error.to_string().contains("exceeded"));
    }

    #[test]
    fn missing_shell_is_reported() {
        assert!(
            resolve_path(Command::new("/nonexistent/wilkes-test-shell"), TIMEOUT)
                .unwrap_err()
                .to_string()
                .contains("start login shell")
        );
    }

    #[test]
    fn account_shell_is_an_absolute_executable_path() {
        let shell = login_shell().unwrap();
        assert!(shell.is_absolute());
        assert!(shell.is_file());
    }
}
