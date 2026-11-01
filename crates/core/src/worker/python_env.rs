use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::ipc::WorkerEvent;
use super::manager::WorkerPaths;
use crate::models::progress::EmbedProgress;

/// The Pythons an environment may be built on, newest first. An environment
/// takes the first that its requirements resolve on, because which Python a
/// Transformers release installs on is itself part of what a model needs: 4.32
/// has wheels only up to 3.11, 5.3 only from 3.10.
const PYTHON_VERSIONS: &[&str] = &["3.14", "3.13", "3.12", "3.11", "3.10", "3.9"];

/// 2.3.0 is the first Sentence Transformers that takes `trust_remote_code`, and
/// the worker loads every model with it. A Transformers older than any such
/// release is refused by the resolver, as a conflict in the packages' own
/// metadata.
const CUSTOM_CODE_LOADER: &str = "sentence-transformers>=2.3.0";

/// What an environment has to be able to import to be worth keeping. The
/// resolver only knows what packages declare, and a declared-compatible set can
/// still fail here.
const SMOKE_TEST: &str = "import torch, transformers, sentence_transformers";

/// How much of a failed setup step's output its error carries. Enough for the
/// conflict the resolver explains at the end of a failed step.
const SETUP_ERROR_LINES: usize = 20;

/// Written last, so an environment without it never finished being built.
const READY_MARKER: &str = ".ready";

/// A removal renames to this first, so a half-deleted environment can never be
/// picked up by the next start.
const TRASH_PREFIX: &str = ".trash-";

/// Where the virtualenv lived when it was built by `pip` on the system Python.
const LEGACY_VENV: &str = "sbert_venv";

/// `models.json` is rewritten whole, and two installers may finish together.
static MODEL_RECORDS: Mutex<()> = Mutex::new(());

/// Environments being built right now. An unfinished directory is exactly what
/// the sweep removes, and a context is created for every workspace opened, so
/// the sweep has to be able to tell a build in progress from one that died.
static BUILDING: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// Marks a directory as being built for as long as this is held, which includes
/// a build dropped by a cancel.
struct BuildGuard(PathBuf);

impl BuildGuard {
    fn new(venv_dir: &Path) -> Self {
        BUILDING.lock().unwrap().insert(venv_dir.to_path_buf());
        Self(venv_dir.to_path_buf())
    }
}

impl Drop for BuildGuard {
    fn drop(&mut self) {
        BUILDING.lock().unwrap().remove(&self.0);
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
#[cfg(any(test, windows))]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(windows)]
fn windows_creation_flags() -> u32 {
    CREATE_NO_WINDOW
}

#[cfg_attr(not(windows), allow(dead_code))]
#[cfg(not(windows))]
fn windows_creation_flags() -> u32 {
    0
}

#[cfg(windows)]
fn suppress_windows_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    command.creation_flags(windows_creation_flags());
}

#[cfg(not(windows))]
fn suppress_windows_console(_command: &mut Command) {}

/// One minor release line of Transformers. Patch releases share an environment,
/// so models saved days apart do not each cost one.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct TransformersSeries {
    major: u32,
    minor: u32,
}

impl TransformersSeries {
    /// Reads the series out of a version as a model declares it, which is often
    /// an unreleased one such as `4.47.0.dev0`.
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        let mut parts = trimmed.split('.');
        let mut next_number = |name: &str| {
            parts
                .next()
                .ok_or_else(|| format!("missing {name} version in '{trimmed}'"))?
                .parse::<u32>()
                .map_err(|e| format!("invalid {name} version in '{trimmed}': {e}"))
        };
        let major = next_number("major")?;
        let minor = next_number("minor")?;
        Ok(Self { major, minor })
    }

    fn requirement(&self) -> String {
        format!("transformers~={}.{}.0", self.major, self.minor)
    }
}

impl std::fmt::Display for TransformersSeries {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// What a model needs beyond the base environment. A request carries `None`
/// when it needs nothing beyond it.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct EnvNeeds {
    /// `None` keeps the base environment's Transformers.
    pub transformers: Option<TransformersSeries>,
    /// Packages the model's own code imports.
    pub packages: BTreeSet<String>,
}

impl EnvNeeds {
    fn describe(&self) -> String {
        let transformers = self
            .transformers
            .map(|series| format!("Transformers {series}"));
        let packages = self.packages.iter().cloned().collect::<Vec<_>>().join(", ");
        match (transformers, packages.is_empty()) {
            (Some(transformers), true) => transformers,
            (Some(transformers), false) => format!("{transformers} with {packages}"),
            (None, _) => packages,
        }
    }
}

/// An environment as it was resolved. Outlives the environment, so that one
/// removed and needed again comes back with the same versions.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct EnvSpec {
    needs: Option<EnvNeeds>,
    /// The Python it was resolved for: a minor version, or the interpreter
    /// `WILKES_PYTHON` names.
    python: String,
    lock: String,
    created_at: String,
    last_used_at: Option<String>,
}

/// Which environment a custom model settled on. `declared` is kept to notice a
/// repository that has moved on to another Transformers.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ModelEnv {
    pub(crate) declared: Option<TransformersSeries>,
    pub(crate) needs: Option<EnvNeeds>,
}

/// One built environment, as the settings show it.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PythonEnvInfo {
    pub id: String,
    pub is_base: bool,
    pub transformers: String,
    pub python: String,
    pub packages: Vec<String>,
    pub size_bytes: u64,
    pub models: Vec<String>,
    pub last_used_at: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PythonEnvInventory {
    pub envs: Vec<PythonEnvInfo>,
    /// What everything under the Python directory occupies, with the files
    /// environments share counted once. Larger than the true figure on Windows,
    /// which does not expose file identity to stable Rust.
    pub total_bytes: u64,
}

fn specs_dir(paths: &WorkerPaths) -> PathBuf {
    paths.python_root().join("specs")
}

fn envs_dir(paths: &WorkerPaths) -> PathBuf {
    paths.python_root().join("envs")
}

fn models_path(paths: &WorkerPaths) -> PathBuf {
    paths.python_root().join("models.json")
}

fn spec_path(paths: &WorkerPaths, id: &str) -> PathBuf {
    specs_dir(paths).join(format!("{id}.json"))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("[python-setup] {} has no parent", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("[python-setup] Cannot create {}: {e}", parent.display()))?;
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| format!("[python-setup] Cannot serialize {}: {e}", path.display()))?;
    let temp = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| format!("[python-setup] Cannot write {}: {e}", path.display()))?;
    std::fs::write(temp.path(), json)
        .map_err(|e| format!("[python-setup] Cannot write {}: {e}", path.display()))?;
    temp.persist(path)
        .map_err(|e| format!("[python-setup] Cannot write {}: {e}", path.display()))?;
    Ok(())
}

fn read_spec(paths: &WorkerPaths, id: &str) -> Result<Option<EnvSpec>, String> {
    let path = spec_path(paths, id);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "[python-setup] Cannot read {}: {e}",
                path.display()
            ))
        }
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("[python-setup] Cannot parse {}: {e}", path.display()))
}

fn base_requirements(paths: &WorkerPaths) -> Result<String, String> {
    let text = std::fs::read_to_string(&paths.requirements_path).map_err(|e| {
        format!(
            "[python-setup] Cannot read {}: {e}",
            paths.requirements_path.display()
        )
    })?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n")
}

/// The package a requirements line installs, or `None` for an option line.
fn requirement_name(line: &str) -> Option<&str> {
    if line.starts_with('-') {
        return None;
    }
    line.split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
        .next()
}

/// The Transformers the base environment provides, read from its pin.
pub(crate) fn base_transformers(paths: &WorkerPaths) -> Result<TransformersSeries, String> {
    let requirements = base_requirements(paths)?;
    let line = requirements
        .lines()
        .find(|line| requirement_name(line) == Some("transformers"))
        .ok_or_else(|| "[python-setup] requirements.txt does not pin Transformers".to_string())?;
    let version = line
        .strip_prefix("transformers==")
        .filter(|version| !version.contains(';'))
        .ok_or_else(|| format!("[python-setup] Transformers is not pinned by '{line}'"))?;
    TransformersSeries::parse(version).map_err(|e| format!("[python-setup] {e}"))
}

/// A package name as it comes from a model's source reaches a requirements
/// file, where anything beginning with a dash would be an option to the
/// installer rather than a package.
fn validate_package(name: &str) -> Result<(), String> {
    let valid = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if valid {
        Ok(())
    } else {
        Err(format!(
            "This model asks for a package named '{name}', which is not a package name."
        ))
    }
}

/// The requirements of the environment for `needs`. A Transformers other than
/// the base one takes no pin from the base: they describe the base release, one
/// would rule out Pythons the model could run on, and a package installed for
/// no reason can break the environment outright, as `peft` does below
/// Transformers 4.42. Its lock is what keeps it the same from then on.
fn env_requirements(base: &str, needs: Option<&EnvNeeds>) -> String {
    let Some(needs) = needs else {
        return base.to_string();
    };
    let mut lines: Vec<String> = match needs.transformers {
        None => base.lines().map(str::to_string).collect(),
        Some(series) => base
            .lines()
            .filter(|line| line.starts_with('-'))
            .map(str::to_string)
            .chain([
                "torch".to_string(),
                series.requirement(),
                CUSTOM_CODE_LOADER.to_string(),
            ])
            .collect(),
    };
    lines.extend(needs.packages.iter().cloned());
    lines.join("\n") + "\n"
}

/// An environment is identified by what goes into it. Models that need the same
/// thing share one, and an update to the pins is simply a different environment.
fn env_id(paths: &WorkerPaths, requirements: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(requirements.as_bytes());
    if let Some(python) = &paths.python_override {
        digest.update(python.as_os_str().as_encoded_bytes());
    }
    digest.finalize()[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn venv_python(venv_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        venv_dir.join("Scripts").join("python.exe")
    } else {
        venv_dir.join("bin").join("python3")
    }
}

fn is_ready(venv_dir: &Path) -> bool {
    venv_dir.join(READY_MARKER).exists() && venv_python(venv_dir).exists()
}

/// `uv`, kept to the app's own directory: its packages, its interpreters, and
/// neither a system Python nor the user's uv configuration.
fn uv_command(paths: &WorkerPaths) -> Command {
    let root = paths.python_root();
    let mut command = Command::new(&paths.uv_bin);
    suppress_windows_console(&mut command);
    command
        .env("UV_CACHE_DIR", root.join("cache"))
        .env("UV_PYTHON_INSTALL_DIR", root.join("interpreters"))
        .env("UV_PYTHON_BIN_DIR", root.join("bin"))
        .env("UV_NO_CONFIG", "1")
        .env("UV_LINK_MODE", "hardlink")
        .env("UV_NO_PROGRESS", "1")
        .env_remove("VIRTUAL_ENV")
        .env_remove("UV_PYTHON");
    command
}

/// Runs a subprocess, forwarding each line of stdout and stderr to tracing.
/// Returns an error string if the process fails to spawn or exits non-zero,
/// ending in what the process said, since that is the reason it failed.
async fn run_setup_step(mut command: Command, label: &str) -> Result<(), String> {
    tracing::info!("[python-setup] {label}");
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("[python-setup] Failed to start {label}: {e}"))?;

    let (line_tx, mut line_rx) = mpsc::channel::<String>(64);

    if let Some(stdout) = child.stdout.take() {
        let tx = line_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while reader
                .read_line(&mut line)
                .await
                .map(|n| n > 0)
                .unwrap_or(false)
            {
                let _ = tx.send(line.trim_end().to_string()).await;
                line.clear();
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let tx = line_tx.clone();
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr);
            let mut line = String::new();
            while reader
                .read_line(&mut line)
                .await
                .map(|n| n > 0)
                .unwrap_or(false)
            {
                let _ = tx.send(line.trim_end().to_string()).await;
                line.clear();
            }
        });
    }

    drop(line_tx);
    let mut last_lines = std::collections::VecDeque::with_capacity(SETUP_ERROR_LINES);
    while let Some(line) = line_rx.recv().await {
        if !line.is_empty() {
            tracing::info!("[python-setup] {line}");
            if last_lines.len() == SETUP_ERROR_LINES {
                last_lines.pop_front();
            }
            last_lines.push_back(line);
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| format!("[python-setup] {label} wait failed: {e}"))?;
    if !status.success() {
        return Err(format!(
            "[python-setup] {label} failed (exit code {:?}):\n{}",
            status.code(),
            Vec::from(last_lines).join("\n")
        ));
    }

    Ok(())
}

/// Tell whoever is waiting what the setup is doing. A notification the next one
/// supersedes, so a full channel only loses this one.
fn report(reply: &mpsc::Sender<WorkerEvent>, message: String) {
    let event = WorkerEvent::Progress(EmbedProgress::Setup(message));
    if let Err(e) = reply.try_send(event) {
        tracing::trace!("dropping setup progress: {e}");
    }
}

/// The Pythons to try: the one `WILKES_PYTHON` names, or every version the app
/// can install. The string goes to `uv` as written.
fn python_choices(paths: &WorkerPaths) -> Vec<String> {
    match &paths.python_override {
        Some(python) => vec![python.display().to_string()],
        None => PYTHON_VERSIONS.iter().map(|v| v.to_string()).collect(),
    }
}

fn select_python(command: &mut Command, paths: &WorkerPaths, python: &str, flag: &str) {
    if paths.python_override.is_some() {
        command.arg("--python").arg(python);
    } else {
        command
            .arg(flag)
            .arg(python)
            .args(["--python-preference", "only-managed"]);
    }
}

/// The reason nothing resolved. The newest and oldest Python usually fail for
/// different reasons, and only together do they show which one is decisive.
fn rejection(what: &str, failures: &[String]) -> String {
    let mut reasons: Vec<&String> = Vec::new();
    for reason in [failures.first(), failures.last()].into_iter().flatten() {
        if !reasons.contains(&reason) {
            reasons.push(reason);
        }
    }
    format!(
        "{what} cannot be installed on {} {} with any Python this app can use.\n{}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        reasons
            .iter()
            .map(|reason| reason.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Find a Python the requirements resolve on, newest first, and lock them to it.
/// Runs before anything is installed, so that a refusal costs seconds and no
/// disk. Wheels only: building an old package from source would run its build
/// scripts to arrive at the same refusal minutes later.
async fn resolve_env(
    paths: &WorkerPaths,
    id: &str,
    requirements: &str,
    needs: Option<&EnvNeeds>,
    reply: &mpsc::Sender<WorkerEvent>,
) -> Result<EnvSpec, String> {
    let workspace = tempfile::Builder::new()
        .prefix(id)
        .tempdir_in({
            std::fs::create_dir_all(paths.python_root()).map_err(|e| {
                format!(
                    "[python-setup] Cannot create {}: {e}",
                    paths.python_root().display()
                )
            })?;
            paths.python_root()
        })
        .map_err(|e| format!("[python-setup] Cannot create a resolve directory: {e}"))?;
    let input = workspace.path().join("requirements.in");
    let output = workspace.path().join("lock.txt");
    std::fs::write(&input, requirements)
        .map_err(|e| format!("[python-setup] Cannot write {}: {e}", input.display()))?;

    let what = needs.map_or_else(|| "The base environment".to_string(), EnvNeeds::describe);
    report(reply, format!("Checking what {what} needs"));

    let mut failures = Vec::new();
    for python in python_choices(paths) {
        let mut command = uv_command(paths);
        command
            .args(["pip", "compile"])
            .arg(&input)
            .arg("--output-file")
            .arg(&output)
            .args(["--only-binary", ":all:"])
            .args(["--index-strategy", "unsafe-best-match"])
            .args([
                "--no-header",
                "--no-annotate",
                "--emit-index-url",
                "--quiet",
            ]);
        select_python(&mut command, paths, &python, "--python-version");
        tracing::info!("[python-setup] Trying Python {python}");
        match run_setup_step(command, "Resolve").await {
            Ok(()) => {
                let lock = std::fs::read_to_string(&output)
                    .map_err(|e| format!("[python-setup] Cannot read {}: {e}", output.display()))?;
                return Ok(EnvSpec {
                    needs: needs.cloned(),
                    python,
                    lock,
                    created_at: chrono::Utc::now().to_rfc3339(),
                    last_used_at: None,
                });
            }
            Err(e) => failures.push(e),
        }
    }
    Err(rejection(&what, &failures))
}

async fn install_env(
    paths: &WorkerPaths,
    spec: &EnvSpec,
    venv_dir: &Path,
    reply: &mpsc::Sender<WorkerEvent>,
) -> Result<(), String> {
    report(reply, format!("Installing Python {}", spec.python));
    let mut venv = uv_command(paths);
    venv.arg("venv").arg(venv_dir).args(["--clear", "--quiet"]);
    select_python(&mut venv, paths, &spec.python, "--python");
    run_setup_step(venv, "Create virtualenv").await?;

    let lock = venv_dir.join("lock.txt");
    std::fs::write(&lock, &spec.lock)
        .map_err(|e| format!("[python-setup] Cannot write {}: {e}", lock.display()))?;
    let python = venv_python(venv_dir);

    report(reply, "Installing packages".to_string());
    let mut install = uv_command(paths);
    install
        .args(["pip", "install", "--python"])
        .arg(&python)
        .arg("-r")
        .arg(&lock)
        .args(["--no-deps", "--quiet"])
        .args(["--only-binary", ":all:"])
        .args(["--index-strategy", "unsafe-best-match"]);
    run_setup_step(install, "Install packages").await?;

    report(reply, "Testing the environment".to_string());
    let mut smoke = Command::new(&python);
    suppress_windows_console(&mut smoke);
    smoke.args(["-c", SMOKE_TEST]).env_remove("PYTHONPATH");
    run_setup_step(smoke, "Import test").await?;

    std::fs::write(venv_dir.join(READY_MARKER), "")
        .map_err(|e| format!("[python-setup] Cannot mark environment ready: {e}"))
}

/// Ensures the environment for `needs`, the base one for `None`, is built, and
/// returns its Python. What runs a model is always one of these: anything that
/// did not finish, for any reason, leaves no directory to be mistaken for one.
pub(crate) async fn ensure_env(
    paths: &WorkerPaths,
    needs: Option<&EnvNeeds>,
    reply: &mpsc::Sender<WorkerEvent>,
) -> Result<PathBuf, String> {
    if let Some(needs) = needs {
        for package in &needs.packages {
            validate_package(package)?;
        }
    }
    let requirements = env_requirements(&base_requirements(paths)?, needs);
    let id = env_id(paths, &requirements);
    let venv_dir = envs_dir(paths).join(&id);
    let python = venv_python(&venv_dir);

    if is_ready(&venv_dir) {
        touch_spec(paths, &id);
        return Ok(python);
    }

    let spec = match read_spec(paths, &id)? {
        Some(spec) => spec,
        None => {
            let spec = resolve_env(paths, &id, &requirements, needs, reply).await?;
            write_json(&spec_path(paths, &id), &spec)?;
            spec
        }
    };

    tracing::info!("[python-setup] Building environment {id}");
    let _building = BuildGuard::new(&venv_dir);
    if let Err(e) = install_env(paths, &spec, &venv_dir, reply).await {
        if let Err(remove) = std::fs::remove_dir_all(&venv_dir) {
            tracing::warn!(
                "[python-setup] Could not remove the failed environment {}: {remove}",
                venv_dir.display()
            );
        }
        let what = needs.map_or_else(|| "The base environment".to_string(), EnvNeeds::describe);
        return Err(format!(
            "{what} installed on Python {} but does not work.\n{e}",
            spec.python
        ));
    }

    touch_spec(paths, &id);
    if needs.is_none() {
        remove_legacy_venv(paths);
    }
    tracing::info!("[python-setup] Environment {id} ready.");
    Ok(python)
}

fn touch_spec(paths: &WorkerPaths, id: &str) {
    let updated = read_spec(paths, id).and_then(|spec| match spec {
        Some(spec) => write_json(
            &spec_path(paths, id),
            &EnvSpec {
                last_used_at: Some(chrono::Utc::now().to_rfc3339()),
                ..spec
            },
        ),
        None => Ok(()),
    });
    if let Err(e) = updated {
        tracing::warn!("[python-setup] Could not record the use of {id}: {e}");
    }
}

/// The environment `pip` built, which nothing can use once a base one exists.
fn remove_legacy_venv(paths: &WorkerPaths) {
    let legacy = paths.data_dir.join(LEGACY_VENV);
    if legacy.exists() {
        tracing::info!("[python-setup] Removing {}", legacy.display());
        if let Err(e) = std::fs::remove_dir_all(&legacy) {
            tracing::warn!("[python-setup] Could not remove {}: {e}", legacy.display());
        }
    }
}

fn read_model_records(paths: &WorkerPaths) -> Result<BTreeMap<String, ModelEnv>, String> {
    let path = models_path(paths);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => {
            return Err(format!(
                "[python-setup] Cannot read {}: {e}",
                path.display()
            ))
        }
    };
    serde_json::from_str(&text)
        .map_err(|e| format!("[python-setup] Cannot parse {}: {e}", path.display()))
}

pub(crate) fn recorded_model_env(
    paths: &WorkerPaths,
    model_id: &str,
) -> Result<Option<ModelEnv>, String> {
    Ok(read_model_records(paths)?.remove(model_id))
}

pub(crate) fn record_model_env(
    paths: &WorkerPaths,
    model_id: &str,
    record: ModelEnv,
) -> Result<(), String> {
    let _guard = MODEL_RECORDS.lock().unwrap();
    let mut records = read_model_records(paths)?;
    records.insert(model_id.to_string(), record);
    write_json(&models_path(paths), &records)
}

/// Forget a removed model. Its environment is left to the user, since other
/// models may share it; the spec goes if the environment is already gone and
/// nothing is left to rebuild it for.
pub(crate) fn forget_model_env(paths: &WorkerPaths, model_id: &str) -> Result<(), String> {
    let _guard = MODEL_RECORDS.lock().unwrap();
    let mut records = read_model_records(paths)?;
    if records.remove(model_id).is_none() {
        return Ok(());
    }
    write_json(&models_path(paths), &records)?;
    remove_orphaned_specs(paths, &records)
}

fn remove_orphaned_specs(
    paths: &WorkerPaths,
    records: &BTreeMap<String, ModelEnv>,
) -> Result<(), String> {
    let base = base_requirements(paths)?;
    let mut kept: HashSet<String> = records
        .values()
        .map(|record| env_id(paths, &env_requirements(&base, record.needs.as_ref())))
        .collect();
    kept.insert(env_id(paths, &base));
    for id in spec_ids(paths)? {
        if !kept.contains(&id) && !envs_dir(paths).join(&id).exists() {
            let path = spec_path(paths, &id);
            std::fs::remove_file(&path)
                .map_err(|e| format!("[python-setup] Cannot remove {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

fn spec_ids(paths: &WorkerPaths) -> Result<Vec<String>, String> {
    let dir = specs_dir(paths);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("[python-setup] Cannot read {}: {e}", dir.display())),
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|e| format!("[python-setup] Cannot read {}: {e}", dir.display()))?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
                ids.push(stem.to_string());
            }
        }
    }
    Ok(ids)
}

/// Add up the files under `path`, skipping any already in `seen`. Symlinks are
/// not followed: a virtualenv's Python is a link into the interpreters.
fn add_up(path: &Path, seen: &mut HashSet<(u64, u64)>) -> u64 {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(e) => {
            tracing::debug!("Could not measure {}: {e}", path.display());
            return 0;
        }
    };
    if metadata.is_dir() {
        return match std::fs::read_dir(path) {
            Ok(entries) => entries
                .filter_map(|entry| match entry {
                    Ok(entry) => Some(entry.path()),
                    Err(e) => {
                        tracing::debug!("Could not read an entry of {}: {e}", path.display());
                        None
                    }
                })
                .map(|child| add_up(&child, seen))
                .sum(),
            Err(e) => {
                tracing::debug!("Could not read {}: {e}", path.display());
                0
            }
        };
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !seen.insert((metadata.dev(), metadata.ino())) {
            return 0;
        }
    }
    metadata.len()
}

/// Every built environment, with what uses it.
pub fn python_env_inventory(paths: &WorkerPaths) -> Result<PythonEnvInventory, String> {
    let base = base_requirements(paths)?;
    let base_id = env_id(paths, &base);
    let records = read_model_records(paths)?;
    let mut envs = Vec::new();
    for id in spec_ids(paths)? {
        let venv_dir = envs_dir(paths).join(&id);
        if !is_ready(&venv_dir) {
            continue;
        }
        let Some(spec) = read_spec(paths, &id)? else {
            continue;
        };
        let transformers = spec
            .lock
            .lines()
            .find_map(|line| line.strip_prefix("transformers=="))
            .ok_or_else(|| format!("[python-setup] The lock of {id} has no Transformers"))?
            .to_string();
        let models = records
            .iter()
            .filter(|(_, record)| {
                env_id(paths, &env_requirements(&base, record.needs.as_ref())) == id
            })
            .map(|(model, _)| model.clone())
            .collect();
        envs.push(PythonEnvInfo {
            is_base: id == base_id,
            transformers,
            python: spec.python.clone(),
            packages: spec
                .needs
                .map(|needs| needs.packages.into_iter().collect())
                .unwrap_or_default(),
            size_bytes: add_up(&venv_dir, &mut HashSet::new()),
            models,
            last_used_at: spec.last_used_at,
            id,
        });
    }
    envs.sort_by(|a, b| b.is_base.cmp(&a.is_base).then(a.id.cmp(&b.id)));
    Ok(PythonEnvInventory {
        total_bytes: add_up(&paths.python_root(), &mut HashSet::new()),
        envs,
    })
}

fn is_env_id(id: &str) -> bool {
    id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit())
}

/// Remove environments, and then what only they were keeping: the shared
/// package cache, without which a removal would free almost nothing, and any
/// Python no environment is built on any more. The base environment can be
/// named too: it is rebuilt from its lock the next time it is needed, as any
/// other is.
///
/// The caller stops whatever is running in them first.
pub async fn remove_python_envs(paths: &WorkerPaths, ids: &[String]) -> Result<(), String> {
    for id in ids {
        if !is_env_id(id) {
            return Err(format!("'{id}' is not a Python environment"));
        }
        let venv_dir = envs_dir(paths).join(id);
        let trash = envs_dir(paths).join(format!("{TRASH_PREFIX}{id}"));
        std::fs::rename(&venv_dir, &trash)
            .map_err(|e| format!("Cannot remove {}: {e}", venv_dir.display()))?;
        std::fs::remove_dir_all(&trash)
            .map_err(|e| format!("Cannot remove {}: {e}", trash.display()))?;
    }
    remove_orphaned_specs(paths, &read_model_records(paths)?)?;

    let mut clean = uv_command(paths);
    clean.args(["cache", "clean", "--quiet"]);
    run_setup_step(clean, "Clean package cache").await?;

    if paths.python_override.is_some() {
        return Ok(());
    }
    let mut in_use = HashSet::new();
    for id in spec_ids(paths)? {
        if is_ready(&envs_dir(paths).join(&id)) {
            if let Some(spec) = read_spec(paths, &id)? {
                in_use.insert(spec.python);
            }
        }
    }
    for python in PYTHON_VERSIONS
        .iter()
        .filter(|python| !in_use.contains(**python))
    {
        let mut uninstall = uv_command(paths);
        uninstall.args(["python", "uninstall", "--quiet", python]);
        run_setup_step(uninstall, &format!("Uninstall Python {python}")).await?;
    }
    Ok(())
}

/// Clear what an interrupted run left behind: unfinished environments, removals
/// that did not finish, and the virtualenv `pip` built once its replacement
/// exists. Finished environments are never touched here, however unused.
pub fn sweep_python_envs(paths: &WorkerPaths) {
    let dir = envs_dir(paths);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!("[python-setup] Cannot read {}: {e}", dir.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !is_ready(&path) && !BUILDING.lock().unwrap().contains(&path) {
            tracing::info!("[python-setup] Removing unfinished {}", path.display());
            if let Err(e) = std::fs::remove_dir_all(&path) {
                tracing::warn!("[python-setup] Could not remove {}: {e}", path.display());
            }
        }
    }
    match base_requirements(paths) {
        Ok(base) => {
            if is_ready(&dir.join(env_id(paths, &base))) {
                remove_legacy_venv(paths);
            }
        }
        Err(e) => tracing::warn!("{e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{tempdir, TempDir};

    #[test]
    fn test_windows_creation_flags_shape() {
        #[cfg(windows)]
        assert_eq!(windows_creation_flags(), CREATE_NO_WINDOW);

        #[cfg(not(windows))]
        assert_eq!(windows_creation_flags(), 0);
    }

    const SHIPPED_REQUIREMENTS: &str = include_str!("../../../worker/requirements.txt");

    fn series(raw: &str) -> TransformersSeries {
        TransformersSeries::parse(raw).unwrap()
    }

    fn needs(transformers: Option<&str>, packages: &[&str]) -> EnvNeeds {
        EnvNeeds {
            transformers: transformers.map(series),
            packages: packages.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn paths_in(dir: &Path) -> WorkerPaths {
        let requirements_path = dir.join("requirements.txt");
        std::fs::write(&requirements_path, SHIPPED_REQUIREMENTS).unwrap();
        WorkerPaths {
            python_override: None,
            python_package_dir: dir.to_path_buf(),
            requirements_path,
            uv_bin: dir.join("uv"),
            worker_bin: dir.join("worker"),
            data_dir: dir.to_path_buf(),
        }
    }

    fn base_id(paths: &WorkerPaths) -> String {
        env_id(paths, &base_requirements(paths).unwrap())
    }

    fn id_of(paths: &WorkerPaths, wanted: &EnvNeeds) -> String {
        let base = base_requirements(paths).unwrap();
        env_id(paths, &env_requirements(&base, Some(wanted)))
    }

    #[test]
    fn a_series_is_read_from_the_version_a_model_declares() {
        assert_eq!(series("4.47.0.dev0"), series("4.47.1"));
        assert_eq!(series("4.47").to_string(), "4.47");
        assert!(TransformersSeries::parse("4").is_err());
        assert!(TransformersSeries::parse("latest.stable").is_err());
    }

    #[test]
    fn the_base_transformers_is_read_from_the_shipped_pin() {
        let dir = tempdir().unwrap();
        assert_eq!(base_transformers(&paths_in(dir.path())), Ok(series("5.22")));
    }

    #[test]
    fn a_requirements_file_that_does_not_pin_transformers_is_an_error() {
        let dir = tempdir().unwrap();
        let paths = paths_in(dir.path());

        std::fs::write(&paths.requirements_path, "torch==2.8.0\n").unwrap();
        assert!(base_transformers(&paths)
            .unwrap_err()
            .contains("does not pin"));

        std::fs::write(&paths.requirements_path, "transformers>=4.0\n").unwrap();
        assert!(base_transformers(&paths)
            .unwrap_err()
            .contains("not pinned"));

        std::fs::write(
            &paths.requirements_path,
            "transformers==5.0.0 ; python_version < \"3.11\"\n",
        )
        .unwrap();
        assert!(base_transformers(&paths)
            .unwrap_err()
            .contains("not pinned"));
    }

    #[test]
    fn another_transformers_takes_no_pin_from_the_base() {
        let dir = tempdir().unwrap();
        let base = base_requirements(&paths_in(dir.path())).unwrap();

        let requirements = env_requirements(&base, Some(&needs(Some("4.47"), &[])));

        assert!(requirements.starts_with("--extra-index-url"));
        assert!(requirements.contains("\ntorch\n"));
        assert!(requirements.contains("transformers~=4.47.0\n"));
        assert!(requirements.contains("sentence-transformers>=2.3.0\n"));
        for package in [
            "peft",
            "einops",
            "accelerate",
            "tokenizers",
            "huggingface-hub",
        ] {
            assert!(!requirements.contains(package), "{package} came along");
        }
        assert!(!requirements.contains('#'));
    }

    #[test]
    fn extra_packages_join_whichever_environment_they_need() {
        let dir = tempdir().unwrap();
        let base = base_requirements(&paths_in(dir.path())).unwrap();

        let on_base = env_requirements(&base, Some(&needs(None, &["timm"])));
        assert!(on_base.contains("transformers==5.22.1\n"));
        assert!(on_base.ends_with("timm\n"));

        let on_series = env_requirements(&base, Some(&needs(Some("4.47"), &["timm"])));
        assert!(on_series.ends_with("timm\n"));
    }

    #[test]
    fn an_environment_is_identified_by_what_goes_into_it() {
        let dir = tempdir().unwrap();
        let paths = paths_in(dir.path());

        let series_id = id_of(&paths, &needs(Some("4.47"), &[]));
        assert_eq!(series_id, id_of(&paths, &needs(Some("4.47"), &[])));
        assert_ne!(series_id, id_of(&paths, &needs(Some("4.46"), &[])));
        assert_ne!(series_id, id_of(&paths, &needs(Some("4.47"), &["timm"])));
        assert_ne!(series_id, base_id(&paths));
        assert!(is_env_id(&series_id));

        let elsewhere = WorkerPaths {
            python_override: Some(PathBuf::from("/opt/python")),
            ..paths.clone()
        };
        assert_ne!(base_id(&elsewhere), base_id(&paths));
    }

    #[test]
    fn a_comment_in_the_requirements_is_not_a_new_environment() {
        let dir = tempdir().unwrap();
        let paths = paths_in(dir.path());
        let before = base_id(&paths);

        let commented = format!("# a note\n{SHIPPED_REQUIREMENTS}");
        std::fs::write(&paths.requirements_path, commented).unwrap();

        assert_eq!(base_id(&paths), before);
    }

    #[test]
    fn only_a_package_name_can_reach_the_requirements_file() {
        assert!(validate_package("timm").is_ok());
        assert!(validate_package("scikit_learn.extra-2").is_ok());
        assert!(validate_package("--index-url=https://evil.example").is_err());
        assert!(validate_package("a b").is_err());
        assert!(validate_package("pkg @ https://evil.example/x.whl").is_err());
        assert!(validate_package("").is_err());
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, content: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, content).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A `uv` that does the bookkeeping of the real one. Every call is appended
    /// to `uv.log`; a Python listed in `unresolvable` fails to resolve with the
    /// file's contents, and `broken` makes the import test fail.
    #[cfg(unix)]
    fn write_fake_uv(dir: &Path) {
        let script = r#"#!/bin/sh
here="__DIR__"
echo "$*" >> "$here/uv.log"
case "$1 $2" in
    "pip compile")
        python=""
        output=""
        while [ $# -gt 0 ]; do
            case "$1" in
                --output-file) output="$2"; shift ;;
                --python-version) python="$2"; shift ;;
            esac
            shift
        done
        if [ -f "$here/unresolvable-$python" ]; then
            cat "$here/unresolvable-$python" 1>&2
            exit 1
        fi
        printf 'torch==2.11.0\ntransformers==4.47.1\n' > "$output"
        ;;
    "pip install")
        ;;
    "cache clean"|"python uninstall")
        ;;
    venv*)
        mkdir -p "$2/bin"
        if [ -f "$here/broken" ]; then
            printf '#!/bin/sh\necho "ImportError: nope" 1>&2\nexit 1\n' > "$2/bin/python3"
        else
            printf '#!/bin/sh\nexit 0\n' > "$2/bin/python3"
        fi
        chmod +x "$2/bin/python3"
        ;;
esac
exit 0
"#
        .replace("__DIR__", dir.to_str().unwrap());
        write_executable(&dir.join("uv"), &script);
    }

    #[cfg(unix)]
    fn uv_calls(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("uv.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[cfg(unix)]
    fn scene() -> (TempDir, WorkerPaths, mpsc::Sender<WorkerEvent>) {
        let dir = tempdir().unwrap();
        write_fake_uv(dir.path());
        let paths = paths_in(dir.path());
        let (reply, _) = mpsc::channel(64);
        (dir, paths, reply)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_environment_takes_the_newest_python_that_resolves() {
        let (dir, paths, reply) = scene();
        for python in ["3.14", "3.13"] {
            std::fs::write(
                dir.path().join(format!("unresolvable-{python}")),
                "no wheels",
            )
            .unwrap();
        }
        let wanted = needs(Some("4.39"), &[]);

        let python = ensure_env(&paths, Some(&wanted), &reply).await.unwrap();

        let id = id_of(&paths, &wanted);
        assert!(python.starts_with(paths.python_root().join("envs").join(&id)));
        assert_eq!(read_spec(&paths, &id).unwrap().unwrap().python, "3.12");
        let calls = uv_calls(dir.path());
        assert!(calls
            .iter()
            .any(|call| call.starts_with("venv") && call.contains("--python 3.12")));
        let install = calls.iter().find(|c| c.starts_with("pip install")).unwrap();
        assert!(install.contains("--no-deps"));
        assert!(install.contains("--only-binary :all:"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_finished_environment_is_not_built_again() {
        let (dir, paths, reply) = scene();

        ensure_env(&paths, None, &reply).await.unwrap();
        let calls = uv_calls(dir.path()).len();
        ensure_env(&paths, None, &reply).await.unwrap();

        assert_eq!(uv_calls(dir.path()).len(), calls);
        assert!(read_spec(&paths, &base_id(&paths))
            .unwrap()
            .unwrap()
            .last_used_at
            .is_some());
    }

    /// The whole contract of a refusal: it names what was asked for and where,
    /// and gives the resolver's reason, before anything was installed.
    #[cfg(unix)]
    #[tokio::test]
    async fn what_cannot_resolve_on_any_python_is_refused_with_the_reason() {
        let (dir, paths, reply) = scene();
        for python in PYTHON_VERSIONS {
            let reason = format!("no solution on {python}");
            std::fs::write(dir.path().join(format!("unresolvable-{python}")), reason).unwrap();
        }

        let err = ensure_env(&paths, Some(&needs(Some("4.26"), &["timm"])), &reply)
            .await
            .unwrap_err();

        assert!(err.contains("Transformers 4.26 with timm"), "{err}");
        assert!(err.contains(std::env::consts::OS), "{err}");
        assert!(err.contains("no solution on 3.14"), "{err}");
        assert!(err.contains("no solution on 3.9"), "{err}");
        assert!(!err.contains("no solution on 3.11"), "{err}");
        assert!(!paths.python_root().join("envs").exists(), "disk was used");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_environment_that_fails_its_import_test_is_not_kept() {
        let (dir, paths, reply) = scene();
        std::fs::write(dir.path().join("broken"), "").unwrap();
        let wanted = needs(Some("4.39"), &[]);

        let err = ensure_env(&paths, Some(&wanted), &reply).await.unwrap_err();

        assert!(err.contains("does not work"), "{err}");
        assert!(err.contains("ImportError: nope"), "{err}");
        assert!(!paths
            .python_root()
            .join("envs")
            .join(id_of(&paths, &wanted))
            .exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_package_name_that_is_not_one_stops_before_anything_runs() {
        let (dir, paths, reply) = scene();

        let err = ensure_env(&paths, Some(&needs(None, &["--index-url=x"])), &reply)
            .await
            .unwrap_err();

        assert!(err.contains("not a package name"), "{err}");
        assert!(uv_calls(dir.path()).is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_interpreter_wilkes_python_names_is_the_only_one_tried() {
        let (dir, mut paths, reply) = scene();
        paths.python_override = Some(PathBuf::from("/opt/my/python"));

        ensure_env(&paths, None, &reply).await.unwrap();

        let calls = uv_calls(dir.path());
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.starts_with("pip compile"))
                .count(),
            1
        );
        assert!(calls.iter().all(|c| !c.contains("only-managed")));
        assert!(calls[0].contains("--python /opt/my/python"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_inventory_says_what_each_environment_is_for() {
        let (_dir, paths, reply) = scene();
        let wanted = needs(Some("4.47"), &["timm"]);
        ensure_env(&paths, None, &reply).await.unwrap();
        ensure_env(&paths, Some(&wanted), &reply).await.unwrap();
        for (model, env) in [("org/old", Some(wanted.clone())), ("org/plain", None)] {
            let record = ModelEnv {
                declared: None,
                needs: env,
            };
            record_model_env(&paths, model, record).unwrap();
        }

        let inventory = python_env_inventory(&paths).unwrap();

        assert_eq!(inventory.envs.len(), 2);
        let base = &inventory.envs[0];
        assert!(base.is_base);
        assert_eq!(base.models, ["org/plain"]);
        let own = &inventory.envs[1];
        assert!(!own.is_base);
        assert_eq!(own.transformers, "4.47.1");
        assert_eq!(own.python, "3.14");
        assert_eq!(own.packages, ["timm"]);
        assert_eq!(own.models, ["org/old"]);
    }

    /// Removal frees what the environment was keeping, and the lock survives
    /// for as long as a model still needs it, so what comes back is the same.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_removed_environment_comes_back_from_its_lock() {
        let (dir, paths, reply) = scene();
        let wanted = needs(Some("4.47"), &[]);
        ensure_env(&paths, Some(&wanted), &reply).await.unwrap();
        let record = ModelEnv {
            declared: Some(series("4.47")),
            needs: Some(wanted.clone()),
        };
        record_model_env(&paths, "org/old", record).unwrap();
        let id = id_of(&paths, &wanted);

        remove_python_envs(&paths, std::slice::from_ref(&id))
            .await
            .unwrap();

        let env = paths.python_root().join("envs").join(&id);
        assert!(!env.exists());
        let calls = uv_calls(dir.path());
        assert!(calls.iter().any(|c| c.starts_with("cache clean")));
        assert!(calls
            .iter()
            .any(|c| c.contains("python uninstall --quiet 3.14")));

        std::fs::remove_file(dir.path().join("uv.log")).unwrap();
        ensure_env(&paths, Some(&wanted), &reply).await.unwrap();

        assert!(is_ready(&env));
        assert!(
            uv_calls(dir.path())
                .iter()
                .all(|c| !c.starts_with("pip compile")),
            "it was resolved again rather than rebuilt from its lock"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_python_still_in_use_is_kept_when_another_environment_goes() {
        let (dir, paths, reply) = scene();
        let wanted = needs(Some("4.47"), &[]);
        ensure_env(&paths, None, &reply).await.unwrap();
        ensure_env(&paths, Some(&wanted), &reply).await.unwrap();

        remove_python_envs(&paths, &[id_of(&paths, &wanted)])
            .await
            .unwrap();

        let calls = uv_calls(dir.path());
        assert!(!calls
            .iter()
            .any(|c| c.contains("python uninstall --quiet 3.14")));
        assert!(calls
            .iter()
            .any(|c| c.contains("python uninstall --quiet 3.13")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn only_an_environment_id_reaches_the_filesystem() {
        let (_dir, paths, _reply) = scene();

        let err = remove_python_envs(&paths, &["../../settings".to_string()])
            .await
            .unwrap_err();

        assert!(err.contains("not a Python environment"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn forgetting_a_model_drops_a_spec_nothing_can_rebuild_for() {
        let (_dir, paths, reply) = scene();
        let wanted = needs(Some("4.47"), &[]);
        ensure_env(&paths, Some(&wanted), &reply).await.unwrap();
        let record = ModelEnv {
            declared: None,
            needs: Some(wanted.clone()),
        };
        record_model_env(&paths, "org/old", record).unwrap();
        let id = id_of(&paths, &wanted);

        // The environment is still there, so the spec stays with it.
        forget_model_env(&paths, "org/old").unwrap();
        assert!(read_spec(&paths, &id).unwrap().is_some());
        assert_eq!(
            recorded_model_env(&paths, "org/old")
                .unwrap()
                .map(|r| r.declared),
            None
        );

        remove_python_envs(&paths, std::slice::from_ref(&id))
            .await
            .unwrap();
        assert!(read_spec(&paths, &id).unwrap().is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_sweep_clears_leftovers_and_leaves_finished_environments() {
        let (dir, paths, reply) = scene();
        let unused = needs(Some("4.47"), &[]);
        ensure_env(&paths, Some(&unused), &reply).await.unwrap();
        let envs = paths.python_root().join("envs");
        let unfinished = envs.join("0123456789abcdef");
        let trash = envs.join(".trash-fedcba9876543210");
        std::fs::create_dir_all(&unfinished).unwrap();
        std::fs::create_dir_all(&trash).unwrap();
        let building = envs.join("aaaaaaaaaaaaaaaa");
        std::fs::create_dir_all(&building).unwrap();
        let guard = BuildGuard::new(&building);
        let legacy = dir.path().join(LEGACY_VENV);
        std::fs::create_dir_all(&legacy).unwrap();

        sweep_python_envs(&paths);

        assert!(!unfinished.exists());
        assert!(!trash.exists());
        assert!(building.exists(), "a build in progress was swept away");
        drop(guard);
        assert!(is_ready(&envs.join(id_of(&paths, &unused))));
        assert!(legacy.exists(), "kept until a base environment replaces it");

        ensure_env(&paths, None, &reply).await.unwrap();
        assert!(!legacy.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_failed_step_ends_in_what_it_said() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("chatty.sh");
        write_executable(
            &script,
            "#!/bin/sh\nfor i in $(seq 1 100); do echo line-$i; done\nexit 7\n",
        );

        let err = run_setup_step(Command::new(&script), "chatty")
            .await
            .unwrap_err();

        assert!(err.contains("exit code Some(7)"));
        assert!(err.contains("line-100"));
        assert!(!err.contains("line-1\n"), "the whole output came back");
    }

    #[tokio::test]
    async fn a_step_that_cannot_start_says_so() {
        let dir = tempdir().unwrap();
        let err = run_setup_step(Command::new(dir.path().join("missing")), "absent")
            .await
            .unwrap_err();
        assert!(err.contains("Failed to start absent"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_step_that_succeeds_is_quiet_about_it() {
        let dir = tempdir().unwrap();
        let script = dir.path().join("echo.sh");
        write_executable(&script, "#!/bin/sh\necho out\necho err 1>&2\nexit 0\n");

        run_setup_step(Command::new(&script), "echo-step")
            .await
            .unwrap();
    }
}
