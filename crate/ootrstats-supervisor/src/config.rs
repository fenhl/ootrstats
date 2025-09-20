use {
    std::{
        path::PathBuf,
        sync::Arc,
    },
    bytesize::ByteSize,
    serde::Deserialize,
    wheel::fs,
};
#[cfg(windows)] use directories::ProjectDirs;
#[cfg(unix)] use xdg::BaseDirectories;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub(crate) log: bool,
    pub(crate) stats_dir: Option<PathBuf>,
    pub workers: Vec<Worker>,
}

fn make_5gib() -> ByteSize { ByteSize::gib(5) }
fn make_neg_one() -> i8 { -1 }
fn make_five() -> f64 { 5.0 }
fn make_true() -> bool { true }

#[derive(Deserialize)]
pub struct Worker {
    pub name: Arc<str>,
    #[serde(flatten)]
    pub(crate) kind: WorkerKind,
    #[serde(default = "make_true")]
    pub(crate) bench: bool,
    #[serde(default = "make_5gib")]
    pub(crate) min_disk: ByteSize,
    #[serde(default = "make_five")]
    pub(crate) min_disk_percent: f64,
    pub(crate) min_disk_mount_points: Option<Vec<PathBuf>>,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum WorkerKind {
    #[serde(rename_all = "camelCase")]
    Local {
        base_rom_path: PathBuf,
        wsl_distro: Option<String>,
        #[serde(default = "make_neg_one")] // default to keeping one core free to avoid slowing down the supervisor too much
        cores: i8,
    },
    #[serde(rename_all = "camelCase")]
    WebSocket {
        #[serde(default = "make_true")]
        tls: bool,
        hostname: String,
        password: String,
        wsl_distro: Option<String>,
        #[serde(default)]
        priority_users: Vec<String>,
        #[serde(default)]
        hide_reboot: bool,
        #[serde(default)]
        hide_sleep: bool,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[cfg(unix)]
    #[error("config file not found")]
    MissingConfigFile,
    #[cfg(windows)]
    #[error("user folder not found")]
    MissingHomeDir,
}

impl Config {
    pub async fn load() -> Result<Self, Error> {
        #[cfg(unix)] {
            if let Some(config_path) = BaseDirectories::new().find_config_file("ootrstats.json") {
                Ok(fs::read_json(config_path).await?)
            } else {
                Err(Error::MissingConfigFile)
            }
        }
        #[cfg(windows)] {
            Ok(fs::read_json(ProjectDirs::from("net", "Fenhl", "ootrstats").ok_or(Error::MissingHomeDir)?.config_dir().join("config.json")).await?)
        }
    }
}
