use {
    std::path::PathBuf,
    serde::Deserialize,
    wheel::fs,
    crate::worker,
};
#[cfg(windows)] use directories::ProjectDirs;
#[cfg(unix)] use xdg::BaseDirectories;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Config {
    #[serde(default)]
    pub(crate) log: bool,
    pub(crate) stats_dir: Option<PathBuf>,
    pub(crate) workers: Vec<worker::Config>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[cfg(unix)] #[error(transparent)] Xdg(#[from] xdg::BaseDirectoriesError),
    #[cfg(unix)]
    #[error("config file not found")]
    MissingConfigFile,
    #[cfg(windows)]
    #[error("user folder not found")]
    MissingHomeDir,
}

impl Config {
    pub(crate) async fn load() -> Result<Self, Error> {
        #[cfg(unix)] {
            if let Some(config_path) = BaseDirectories::new()?.find_config_file("ootrstats.json") {
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
