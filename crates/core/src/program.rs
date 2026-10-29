//! Running a program a manifest names.
//!
//! The one place one starts, whatever it is for: a request whose output is
//! the response, or a download that writes files. So it is the one place that
//! asks whether it may — the user's allowlist, by the exact name the manifest
//! uses — and the one place that says what a program gets: its arguments as a
//! list with no shell to read them, no input, a working directory of the
//! caller's, and nothing of what it prints past what the caller can use.

use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::types::ProgramRun;

/// How much of a failed program's error output is reported. The end is where
/// a program says why it stopped.
const ERROR_LINES: usize = 20;
/// What is kept of output nobody reads but an error message.
const ERROR_BYTES: usize = 64 * 1024;

/// What the caller does with what the program prints.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Stdout {
    /// It is the answer, held to this many bytes.
    Keep { max_bytes: u64 },
    /// The answer is elsewhere, such as the files the program writes; only
    /// the end is kept, to say why a failed run failed.
    Discard,
}

/// Run `run` with `directory` as its working directory and `{output_dir}`,
/// and answer what it printed when `stdout` keeps it.
///
/// Refused before anything starts unless `allowed` names the program exactly
/// as the manifest does. A run that outlasts `timeout`, or whose future is
/// dropped, is killed. What a failing program says is redacted of the run's
/// secrets, since a program may echo what it was given.
pub(crate) async fn run(
    run: &ProgramRun,
    directory: &Path,
    allowed: &[String],
    stdout: Stdout,
    timeout: Option<Duration>,
) -> Result<Vec<u8>, String> {
    let program = run.program.as_str();
    if !allowed.iter().any(|name| name == program) {
        tracing::warn!(program, "run refused: the program is not allowed");
        return Err(format!(
            "'{program}' is not allowed to run. Allow it by name in the integration's settings to use it."
        ));
    }
    let output_dir = directory.to_string_lossy();
    let mut command = tokio::process::Command::new(program);
    command
        .args(run.args.iter().map(|arg| arg.write(&output_dir)))
        .envs(run.env.iter().map(|(name, value)| (name, value)))
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    // The arguments are not logged: one may hold a secret's value.
    tracing::info!(program, "running a manifest program");
    let mut child = command.spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => {
            format!("'{program}' is not installed, or is not on this application's PATH")
        }
        _ => format!("Could not start '{program}': {error}"),
    })?;
    let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(format!("'{program}' started without its output piped"));
    };
    let finished = async {
        let (printed, said) = tokio::join!(read_stdout(out, stdout), read_tail(err));
        let status = child
            .wait()
            .await
            .map_err(|error| format!("'{program}' could not be waited on: {error}"))?;
        Ok::<_, String>((status, printed?, said?))
    };
    let (status, printed, said) = match timeout {
        Some(limit) => tokio::time::timeout(limit, finished).await.map_err(|_| {
            format!(
                "'{program}' did not finish within {} seconds",
                limit.as_secs()
            )
        })??,
        None => finished.await?,
    };
    if !status.success() {
        let message = failure(program, status, &said, &printed);
        tracing::warn!(program, "manifest program failed: {message}");
        return Err(redact(message, &run.secrets));
    }
    match (stdout, printed) {
        (Stdout::Keep { max_bytes }, Printed::All(bytes)) => {
            if bytes.len() as u64 > max_bytes {
                return Err(format!(
                    "'{program}' printed more than the {max_bytes} byte limit"
                ));
            }
            Ok(bytes)
        }
        _ => Ok(Vec::new()),
    }
}

enum Printed {
    /// Everything, or the limit and one byte past it.
    All(Vec<u8>),
    Tail(Vec<u8>),
}

/// Read what the program prints to the end, so it never stops on a full pipe,
/// keeping what the caller wants of it.
async fn read_stdout(reader: impl AsyncRead + Unpin, stdout: Stdout) -> Result<Printed, String> {
    match stdout {
        Stdout::Keep { max_bytes } => {
            let mut kept = Vec::new();
            let mut reader = reader;
            let mut chunk = vec![0u8; 64 * 1024];
            loop {
                let read = reader
                    .read(&mut chunk)
                    .await
                    .map_err(|error| format!("Failed to read a program's output: {error}"))?;
                if read == 0 {
                    break;
                }
                // One byte past the limit is enough to know it was passed.
                let room = (max_bytes + 1).saturating_sub(kept.len() as u64) as usize;
                kept.extend_from_slice(&chunk[..read.min(room)]);
            }
            Ok(Printed::All(kept))
        }
        Stdout::Discard => read_tail(reader).await.map(Printed::Tail),
    }
}

/// The last [`ERROR_BYTES`] of a stream, read to its end.
async fn read_tail(mut reader: impl AsyncRead + Unpin) -> Result<Vec<u8>, String> {
    let mut tail = Vec::new();
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        let read = reader
            .read(&mut chunk)
            .await
            .map_err(|error| format!("Failed to read a program's output: {error}"))?;
        if read == 0 {
            return Ok(tail);
        }
        tail.extend_from_slice(&chunk[..read]);
        if tail.len() > ERROR_BYTES {
            tail.drain(..tail.len() - ERROR_BYTES);
        }
    }
}

/// Why a program failed, in its own words: the end of what it said on its
/// error stream, or of what it printed when it said nothing there.
fn failure(program: &str, status: ExitStatus, said: &[u8], printed: &Printed) -> String {
    let said = String::from_utf8_lossy(said);
    let words = match printed {
        _ if !said.trim().is_empty() => said.into_owned(),
        Printed::All(bytes) | Printed::Tail(bytes) => String::from_utf8_lossy(bytes).into_owned(),
    };
    let lines: Vec<&str> = words.lines().collect();
    let tail = lines[lines.len().saturating_sub(ERROR_LINES)..].join("\n");
    format!("'{program}' failed ({status}):\n{tail}")
}

/// `message` with every secret value in it masked, longest first so that a
/// value holding another is masked whole.
fn redact(mut message: String, secrets: &[String]) -> String {
    let mut secrets: Vec<&str> = secrets
        .iter()
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .collect();
    secrets
        .sort_unstable_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    secrets.dedup();
    for secret in secrets {
        message = message.replace(secret, "***");
    }
    message
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::types::ProgramArg;

    fn shell(script: &str) -> ProgramRun {
        ProgramRun {
            program: "/bin/sh".to_string(),
            args: vec![
                ProgramArg(vec!["-c".to_string()]),
                ProgramArg(vec![script.to_string()]),
            ],
            env: Vec::new(),
            secrets: Vec::new(),
        }
    }

    fn allowed() -> Vec<String> {
        vec!["/bin/sh".to_string()]
    }

    #[tokio::test]
    async fn what_a_request_prints_is_its_answer_up_to_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let keep = Stdout::Keep { max_bytes: 5 };
        let answer = run(&shell("printf hello"), dir.path(), &allowed(), keep, None)
            .await
            .unwrap();
        assert_eq!(answer, b"hello");
        let error = run(&shell("printf hello!"), dir.path(), &allowed(), keep, None)
            .await
            .unwrap_err();
        assert!(error.contains("more than the 5 byte limit"), "{error}");
    }

    #[tokio::test]
    async fn a_program_that_outlasts_its_time_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let error = run(
            &shell("/bin/sleep 30"),
            dir.path(),
            &allowed(),
            Stdout::Keep { max_bytes: 5 },
            Some(Duration::from_millis(200)),
        )
        .await
        .unwrap_err();
        assert!(error.contains("did not finish within"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_program_not_allowed_never_starts() {
        let dir = tempfile::tempdir().unwrap();
        let error = run(
            &shell("/usr/bin/touch started"),
            dir.path(),
            &["sh".to_string()],
            Stdout::Discard,
            None,
        )
        .await
        .unwrap_err();
        assert!(error.contains("'/bin/sh' is not allowed to run"), "{error}");
        assert!(!dir.path().join("started").exists());
    }

    #[tokio::test]
    async fn a_failure_is_told_in_the_programs_words_without_its_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let mut keyed = shell(r#"echo "bad key $KEY" >&2; exit 2"#);
        keyed.env.push(("KEY".to_string(), "s3cret".to_string()));
        keyed.secrets.push("s3cret".to_string());
        let error = run(&keyed, dir.path(), &allowed(), Stdout::Discard, None)
            .await
            .unwrap_err();
        assert!(error.contains("bad key ***"), "{error}");
        assert!(!error.contains("s3cret"), "{error}");
    }
}
