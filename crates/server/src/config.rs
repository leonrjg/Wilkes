use std::path::PathBuf;

pub trait ConfigEnv {
    fn var(&self, key: &str) -> Option<String>;
}

pub struct RawConfigInputs {
    pub args: Vec<String>,
    pub env: Box<dyn ConfigEnv>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub data_dir: PathBuf,
    pub dist_dir: PathBuf,
}

fn env_or_default(env: &dyn ConfigEnv, key: &str, default: &str) -> String {
    env.var(key).unwrap_or_else(|| default.to_string())
}

pub fn parse_config_from(args: &[String], env: &dyn ConfigEnv) -> Config {
    let mut host = env_or_default(env, "WILKES_HOST", "127.0.0.1");
    let mut port: u16 = env
        .var("WILKES_PORT")
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000);
    let mut data_dir = PathBuf::from(env_or_default(env, "WILKES_DATA_DIR", "/data"));
    let mut dist_dir = PathBuf::from(env_or_default(env, "WILKES_DIST_DIR", "./dist"));

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                if let Some(v) = args.get(i + 1) {
                    host = v.clone();
                    i += 1;
                }
            }
            "--port" => {
                if let Some(v) = args.get(i + 1) {
                    if let Ok(p) = v.parse() {
                        port = p;
                    }
                    i += 1;
                }
            }
            "--data-dir" => {
                if let Some(v) = args.get(i + 1) {
                    data_dir = PathBuf::from(v);
                    i += 1;
                }
            }
            "--dist-dir" => {
                if let Some(v) = args.get(i + 1) {
                    dist_dir = PathBuf::from(v);
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }

    Config {
        host,
        port,
        data_dir,
        dist_dir,
    }
}

pub fn parse_config() -> Config {
    struct StdEnv;

    impl ConfigEnv for StdEnv {
        fn var(&self, key: &str) -> Option<String> {
            std::env::var(key).ok()
        }
    }

    let args: Vec<String> = std::env::args().collect();
    parse_config_from(&args, &StdEnv)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestEnv(std::collections::HashMap<String, String>);

    impl ConfigEnv for TestEnv {
        fn var(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    #[test]
    fn parse_config_defaults() {
        let env = TestEnv(Default::default());
        let args = vec!["bin".to_string()];
        let config = parse_config_from(&args, &env);
        assert_eq!(config.host, "127.0.0.1");
        assert_eq!(config.port, 2000);
        assert_eq!(config.data_dir, PathBuf::from("/data"));
        assert_eq!(config.dist_dir, PathBuf::from("./dist"));
    }
}
