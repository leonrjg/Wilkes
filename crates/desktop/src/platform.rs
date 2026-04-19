use std::path::{Path, PathBuf};

use tauri::{AppHandle, Emitter, Manager, Runtime};
use wilkes_core::embed::worker::manager::WorkerPaths;

pub(crate) trait DesktopPlatform {
    fn app_config_dir(&self) -> anyhow::Result<PathBuf>;
    fn app_data_dir(&self) -> anyhow::Result<PathBuf>;
    fn emit(&self, name: &str, payload: serde_json::Value);
    fn open_target(&self, target: &str) -> Result<(), String>;
}

#[derive(Clone)]
pub(crate) struct TauriPlatform<R: Runtime = tauri::Wry>(pub(crate) AppHandle<R>);

impl<R: Runtime> DesktopPlatform for TauriPlatform<R> {
    fn app_config_dir(&self) -> anyhow::Result<PathBuf> {
        Ok(self.0.path().app_config_dir()?)
    }

    fn app_data_dir(&self) -> anyhow::Result<PathBuf> {
        Ok(self.0.path().app_data_dir()?)
    }

    fn emit(&self, name: &str, payload: serde_json::Value) {
        let _ = Emitter::emit(&self.0, name, &payload);
    }

    fn open_target(&self, target: &str) -> Result<(), String> {
        spawn_open_target(target)
    }
}

pub(crate) struct SystemDesktopPlatform;

impl DesktopPlatform for SystemDesktopPlatform {
    fn app_config_dir(&self) -> anyhow::Result<PathBuf> {
        Err(anyhow::anyhow!("app config directory unavailable"))
    }

    fn app_data_dir(&self) -> anyhow::Result<PathBuf> {
        Err(anyhow::anyhow!("app data directory unavailable"))
    }

    fn emit(&self, _name: &str, _payload: serde_json::Value) {}

    fn open_target(&self, target: &str) -> Result<(), String> {
        spawn_open_target(target)
    }
}

pub(crate) fn desktop_settings_path_from(config_dir: PathBuf) -> PathBuf {
    config_dir.join("settings.json")
}

pub(crate) fn desktop_settings_path<P: DesktopPlatform>(platform: &P) -> anyhow::Result<PathBuf> {
    let config = platform.app_config_dir()?;
    Ok(desktop_settings_path_from(config))
}

pub(crate) fn validate_open_target(target: &str) -> Result<(), String> {
    if target.starts_with("http://") || target.starts_with("https://") {
        return Ok(());
    }

    if !Path::new(target).exists() {
        return Err("Path does not exist".into());
    }
    Ok(())
}

pub(crate) fn opener_command() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "open"
    }
    #[cfg(target_os = "windows")]
    {
        "explorer"
    }
    #[cfg(target_os = "linux")]
    {
        "xdg-open"
    }
}

pub(crate) fn spawn_open_target(target: &str) -> Result<(), String> {
    std::process::Command::new(opener_command())
        .arg(target)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) fn build_startup_plan<P: DesktopPlatform>(
    platform: &P,
) -> anyhow::Result<DesktopStartupPlan> {
    let data_dir = platform.app_data_dir()?;
    let settings_path = desktop_settings_path(platform)?;
    let worker_paths = WorkerPaths::resolve(&data_dir);
    Ok(DesktopStartupPlan {
        data_dir,
        settings_path,
        worker_paths,
    })
}

pub(crate) struct DesktopStartupPlan {
    pub(crate) data_dir: PathBuf,
    pub(crate) settings_path: PathBuf,
    pub(crate) worker_paths: WorkerPaths,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    static OPEN_PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct MockPlatform {
        config_dir: Option<PathBuf>,
        data_dir: Option<PathBuf>,
    }

    impl DesktopPlatform for MockPlatform {
        fn app_config_dir(&self) -> anyhow::Result<PathBuf> {
            self.config_dir
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing config dir"))
        }

        fn app_data_dir(&self) -> anyhow::Result<PathBuf> {
            self.data_dir
                .clone()
                .ok_or_else(|| anyhow::anyhow!("missing data dir"))
        }

        fn emit(&self, _name: &str, _payload: serde_json::Value) {}

        fn open_target(&self, _target: &str) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn test_desktop_settings_path_from() {
        let dir = tempdir().unwrap();
        let settings = desktop_settings_path_from(dir.path().to_path_buf());
        assert_eq!(settings, dir.path().join("settings.json"));
    }

    #[test]
    fn test_desktop_settings_path_uses_platform_config_dir() {
        let dir = tempdir().unwrap();
        let platform = MockPlatform {
            config_dir: Some(dir.path().to_path_buf()),
            data_dir: None,
        };

        let settings = desktop_settings_path(&platform).unwrap();
        assert_eq!(settings, dir.path().join("settings.json"));
    }

    #[test]
    fn test_desktop_settings_path_propagates_platform_errors() {
        let platform = MockPlatform {
            config_dir: None,
            data_dir: None,
        };

        let err = desktop_settings_path(&platform).unwrap_err();
        assert!(err.to_string().contains("missing config dir"));
    }

    #[test]
    fn test_build_startup_plan_uses_platform_paths() {
        let data_dir = tempdir().unwrap();
        let config_dir = tempdir().unwrap();
        let platform = MockPlatform {
            config_dir: Some(config_dir.path().to_path_buf()),
            data_dir: Some(data_dir.path().to_path_buf()),
        };

        let plan = build_startup_plan(&platform).unwrap();
        assert_eq!(plan.data_dir, data_dir.path());
        assert_eq!(plan.settings_path, config_dir.path().join("settings.json"));
        assert_eq!(plan.worker_paths.data_dir, data_dir.path());
    }

    #[test]
    fn test_validate_open_target() {
        let dir = tempdir().unwrap();
        assert!(validate_open_target(&dir.path().display().to_string()).is_ok());
        assert_eq!(
            validate_open_target(&dir.path().join("missing").display().to_string()),
            Err("Path does not exist".into())
        );
        assert!(validate_open_target("https://doi.org/10.1000/xyz123").is_ok());
    }

    #[test]
    fn test_system_platform_reports_unavailable_dirs_and_noop_emit() {
        let platform = SystemDesktopPlatform;

        assert!(platform.app_config_dir().is_err());
        assert!(platform.app_data_dir().is_err());
        platform.emit("event", serde_json::json!({"ok": true}));
    }

    #[cfg(unix)]
    #[test]
    fn test_system_platform_open_target_uses_opener() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = OPEN_PATH_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let opener_name = opener_command();
        let opener = dir.path().join(opener_name);
        std::fs::write(&opener, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&opener).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&opener, perms).unwrap();

        let path = dir.path().join("target");
        std::fs::create_dir(&path).unwrap();

        let original_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", dir.path().display(), original_path);
        std::env::set_var("PATH", &new_path);

        let res = SystemDesktopPlatform.open_target(&path.display().to_string());
        std::env::set_var("PATH", original_path);

        assert!(res.is_ok());
    }

    #[test]
    fn test_tauri_platform_paths_and_emit_smoke() {
        let app = tauri::test::mock_app();
        let platform = TauriPlatform(app.handle().clone());

        let _ = platform.app_config_dir();
        let _ = platform.app_data_dir();
        platform.emit("desktop-test", serde_json::json!({"value": 1}));
    }

    #[cfg(unix)]
    #[test]
    fn test_tauri_platform_open_target_uses_opener() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = OPEN_PATH_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let opener_name = opener_command();
        let opener = dir.path().join(opener_name);
        std::fs::write(&opener, "#!/bin/sh\nexit 0\n").unwrap();
        let mut perms = std::fs::metadata(&opener).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&opener, perms).unwrap();

        let target = dir.path().join("folder");
        std::fs::create_dir(&target).unwrap();

        let app = tauri::test::mock_app();
        let platform = TauriPlatform(app.handle().clone());

        let original_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", dir.path().display(), original_path);
        std::env::set_var("PATH", &new_path);

        let res = platform.open_target(&target.display().to_string());
        std::env::set_var("PATH", original_path);

        assert!(res.is_ok());
    }
}
