//! ACP client for Wilkes's "Ask the documents" chat pane.
//!
//! One integration mechanism (ACP over stdio), one context-injection
//! mechanism (`context::build_context_block`, pushed on every turn), and one
//! permission boundary (`session::ChatSession`, read-only), shared by the
//! supported CLI backends. See docs/chat-agent-integration-spec.md.

pub mod context;
pub mod mcp;
pub mod reader;
pub mod search;
pub mod session;

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use serde_json::Value;
use wilkes_core::types::AgentBackend;

#[derive(Clone, Debug)]
pub struct ResolvedLaunchSpec {
    pub command: PathBuf,
    pub args: Vec<String>,
}

/// Availability of a backend's ACP adapter. The single launch mechanism is
/// `npx -y <package>@<version>`, so a backend is `available` once the node/npx
/// toolchain is present and the pinned adapter is already materialized in npm's
/// npx cache. When the toolchain is present but the adapter has not been fetched
/// yet, `installable` is true and the app offers an explicit pre-warm.
#[derive(Clone, Debug)]
pub struct BackendAvailability {
    pub available: bool,
    pub installable: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct NpmPackageSpec {
    pub package: &'static str,
    /// Pinned version so a cached copy always satisfies `npx` without a registry
    /// round-trip -- deterministic and offline after the first fetch.
    pub version: &'static str,
    pub bin: &'static str,
    pub args: &'static [&'static str],
}

static AVAILABILITY_CACHE: OnceLock<Mutex<HashMap<AgentBackend, BackendAvailability>>> =
    OnceLock::new();

/// Npm package metadata for each backend's ACP adapter.
pub fn package_spec(backend: AgentBackend) -> NpmPackageSpec {
    match backend {
        AgentBackend::ClaudeCode => NpmPackageSpec {
            package: "@agentclientprotocol/claude-agent-acp",
            version: "0.55.0",
            bin: "claude-agent-acp",
            args: &[],
        },
        AgentBackend::Codex => NpmPackageSpec {
            package: "@agentclientprotocol/codex-acp",
            version: "1.1.0",
            bin: "codex-acp",
            args: &[],
        },
        AgentBackend::Nanocoder => NpmPackageSpec {
            package: "@nanocollective/nanocoder",
            version: "1.28.1",
            bin: "nanocoder",
            args: &["--acp"],
        },
    }
}

pub fn label(backend: AgentBackend) -> &'static str {
    match backend {
        AgentBackend::ClaudeCode => "Claude Code",
        AgentBackend::Codex => "Codex",
        AgentBackend::Nanocoder => "Nanocoder",
    }
}

/// Shown next to a disabled backend in the agent selector.
pub fn auth_note(backend: AgentBackend) -> &'static str {
    match backend {
        AgentBackend::ClaudeCode => "Install the Claude Code ACP, then run `claude` once to log in",
        AgentBackend::Codex => "Install the Codex ACP, then run `codex` once to log in",
        AgentBackend::Nanocoder => "Install the Nanocoder ACP and configure a provider",
    }
}

/// Availability of the backend's ACP adapter, cached per backend. `refresh`
/// re-probes the npx cache (e.g. after a pre-warm or a Recheck); otherwise a
/// cached result is returned. The result is never used to build a launch path
/// -- launch is always resolved fresh via `resolve_launch_spec`, so a pruned
/// npx cache can't leave a stale absolute path behind.
pub fn probe_backend_availability(backend: AgentBackend, refresh: bool) -> BackendAvailability {
    let cache = AVAILABILITY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if !refresh {
        if let Some(cached) = cache.lock().unwrap().get(&backend).cloned() {
            return cached;
        }
    }

    let availability = probe_backend_availability_uncached(backend);
    cache.lock().unwrap().insert(backend, availability.clone());
    availability
}

/// The single launch mechanism: `npx -y <package>@<version> [args…]`. npx
/// resolves (and, if absent, fetches) the pinned adapter at exec time, so the
/// launch always reflects on-disk state now rather than at last probe.
pub fn resolve_launch_spec(backend: AgentBackend) -> anyhow::Result<ResolvedLaunchSpec> {
    let npx = which::which("npx").map_err(|_| anyhow::anyhow!("npx was not found on PATH"))?;
    Ok(ResolvedLaunchSpec {
        command: npx,
        args: npx_args(package_spec(backend)),
    })
}

fn npx_args(spec: NpmPackageSpec) -> Vec<String> {
    let mut args = vec![
        "-y".to_string(),
        format!("{}@{}", spec.package, spec.version),
    ];
    args.extend(spec.args.iter().map(|arg| arg.to_string()));
    args
}

/// Pre-warm npm's npx cache for the pinned adapter without running the adapter
/// itself. `npm exec --package <pkg> -- node -e ""` materializes the package
/// into `<cache>/_npx/<hash>/node_modules` and exits immediately, so the next
/// `npx` launch reuses it offline.
pub async fn install_backend_adapter(backend: AgentBackend) -> anyhow::Result<BackendAvailability> {
    let spec = package_spec(backend);
    if which::which("npx").is_err() {
        anyhow::bail!("npx was not found on PATH");
    }
    let package = format!("{}@{}", spec.package, spec.version);
    let output = tokio::task::spawn_blocking(move || {
        Command::new("npm")
            .args([
                "exec",
                "--yes",
                "--package",
                &package,
                "--",
                "node",
                "-e",
                "process.exit(0)",
            ])
            .output()
    })
    .await??;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        anyhow::bail!(
            "failed to download {}@{}{}",
            spec.package,
            spec.version,
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
    }

    let availability = probe_backend_availability(backend, true);
    if availability.available {
        Ok(availability)
    } else {
        anyhow::bail!(
            "downloaded {}@{}, but it is not available: {}",
            spec.package,
            spec.version,
            availability
                .unavailable_reason
                .unwrap_or_else(|| "unknown reason".to_string())
        )
    }
}

fn probe_backend_availability_uncached(backend: AgentBackend) -> BackendAvailability {
    if which::which("node").is_err() {
        return unavailable(false, "Node.js was not found on PATH");
    }
    if which::which("npx").is_err() {
        return unavailable(false, "npx was not found on PATH");
    }

    let spec = package_spec(backend);
    if adapter_cached(spec) {
        BackendAvailability {
            available: true,
            installable: false,
            unavailable_reason: None,
        }
    } else {
        // Toolchain is present but the pinned adapter has not been fetched yet;
        // offer an explicit pre-warm rather than downloading on first send.
        unavailable(
            true,
            format!("{}@{} is not downloaded yet", spec.package, spec.version),
        )
    }
}

fn unavailable(installable: bool, reason: impl Into<String>) -> BackendAvailability {
    BackendAvailability {
        available: false,
        installable,
        unavailable_reason: Some(reason.into()),
    }
}

/// Read-only check for whether the pinned adapter version is already
/// materialized in npm's npx cache (`<cache>/_npx/<hash>/node_modules`). The
/// `<hash>` is content-addressed per invocation, so scan every hashed entry.
fn adapter_cached(spec: NpmPackageSpec) -> bool {
    let Ok(cache) = npm_cache_dir() else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(cache.join("_npx")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let pkg_dir = package_dir(&entry.path().join("node_modules"), spec.package);
        pinned_package_present(&pkg_dir, spec)
    })
}

/// True when `pkg_dir` holds the pinned version and its bin target exists.
fn pinned_package_present(pkg_dir: &std::path::Path, spec: NpmPackageSpec) -> bool {
    let Ok(text) = std::fs::read_to_string(pkg_dir.join("package.json")) else {
        return false;
    };
    let Ok(json) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    if json.get("version").and_then(Value::as_str) != Some(spec.version) {
        return false;
    }
    let bin_rel = json.get("bin").and_then(|bin| match bin {
        Value::String(path) => Some(path.as_str()),
        Value::Object(map) => map.get(spec.bin).and_then(Value::as_str),
        _ => None,
    });
    bin_rel.is_some_and(|rel| pkg_dir.join(rel).is_file())
}

fn npm_cache_dir() -> anyhow::Result<PathBuf> {
    let output = Command::new("npm")
        .args(["config", "get", "cache"])
        .output()?;
    if !output.status.success() {
        anyhow::bail!("{}", String::from_utf8_lossy(&output.stderr).trim());
    }
    let cache = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if cache.is_empty() || cache == "undefined" {
        anyhow::bail!("npm returned an empty cache path");
    }
    Ok(PathBuf::from(cache))
}

fn package_dir(node_modules_root: &std::path::Path, package: &str) -> PathBuf {
    package
        .split('/')
        .fold(node_modules_root.to_path_buf(), |path, part| {
            path.join(part)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn package_specs_include_all_backends() {
        let claude = package_spec(AgentBackend::ClaudeCode);
        assert_eq!(claude.package, "@agentclientprotocol/claude-agent-acp");
        assert_eq!(claude.bin, "claude-agent-acp");
        assert!(!claude.version.is_empty());

        let codex = package_spec(AgentBackend::Codex);
        assert_eq!(codex.package, "@agentclientprotocol/codex-acp");
        assert_eq!(codex.bin, "codex-acp");
        assert!(!codex.version.is_empty());

        let nanocoder = package_spec(AgentBackend::Nanocoder);
        assert_eq!(nanocoder.package, "@nanocollective/nanocoder");
        assert_eq!(nanocoder.bin, "nanocoder");
        assert_eq!(nanocoder.args, &["--acp"]);
        assert!(!nanocoder.version.is_empty());
        assert_eq!(label(AgentBackend::Nanocoder), "Nanocoder");
        assert!(!auth_note(AgentBackend::Nanocoder).is_empty());
    }

    #[test]
    fn npx_args_pin_the_version_and_forward_backend_args() {
        let codex = npx_args(package_spec(AgentBackend::Codex));
        assert_eq!(
            codex,
            vec![
                "-y".to_string(),
                "@agentclientprotocol/codex-acp@1.1.0".to_string()
            ]
        );

        let nano = npx_args(package_spec(AgentBackend::Nanocoder));
        assert_eq!(nano[0], "-y");
        assert_eq!(nano[1], "@nanocollective/nanocoder@1.28.1");
        assert_eq!(&nano[2..], &["--acp"]);
    }

    #[test]
    fn package_dir_handles_scoped_packages() {
        let root = PathBuf::from("/tmp/node_modules");
        assert_eq!(
            package_dir(&root, "@agentclientprotocol/codex-acp"),
            root.join("@agentclientprotocol").join("codex-acp")
        );
    }

    #[test]
    fn pinned_package_present_matches_version_and_bin() {
        let dir = tempdir().unwrap();
        let pkg = dir.path().join("@agentclientprotocol").join("codex-acp");
        std::fs::create_dir_all(pkg.join("dist")).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"version":"1.1.0","bin":{"codex-acp":"dist/index.js"}}"#,
        )
        .unwrap();
        std::fs::write(pkg.join("dist").join("index.js"), "").unwrap();

        assert!(pinned_package_present(
            &pkg,
            package_spec(AgentBackend::Codex)
        ));
    }

    #[test]
    fn pinned_package_present_rejects_version_mismatch() {
        let dir = tempdir().unwrap();
        let pkg = dir.path().join("@agentclientprotocol").join("codex-acp");
        std::fs::create_dir_all(pkg.join("dist")).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"version":"1.0.0","bin":{"codex-acp":"dist/index.js"}}"#,
        )
        .unwrap();
        std::fs::write(pkg.join("dist").join("index.js"), "").unwrap();

        assert!(!pinned_package_present(
            &pkg,
            package_spec(AgentBackend::Codex)
        ));
    }

    #[test]
    fn pinned_package_present_rejects_missing_bin_file() {
        let dir = tempdir().unwrap();
        let pkg = dir.path().join("@agentclientprotocol").join("codex-acp");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("package.json"),
            r#"{"version":"1.1.0","bin":{"codex-acp":"dist/index.js"}}"#,
        )
        .unwrap();

        assert!(!pinned_package_present(
            &pkg,
            package_spec(AgentBackend::Codex)
        ));
    }
}
