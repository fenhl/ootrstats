use {
    std::net::{
        IpAddr,
        Ipv4Addr,
    },
    serde::Deserialize,
    wheel::fs,
};
#[cfg(windows)] use directories::ProjectDirs;
#[cfg(unix)] use xdg::BaseDirectories;

fn make_default_address() -> IpAddr {
    Ipv4Addr::new(127, 0, 0, 1).into()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Config {
    pub(crate) password: String,
    #[serde(default = "make_default_address")]
    pub(crate) address: IpAddr,
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
    pub(crate) async fn load() -> Result<Self, Error> {
        #[cfg(unix)] {
            if let Some(config_path) = BaseDirectories::new().find_config_file("ootrstats-worker-daemon.json") {
                Ok(fs::read_json(config_path).await?)
            } else {
                Err(Error::MissingConfigFile)
            }
        }
        #[cfg(windows)] {
            Ok(fs::read_json(ProjectDirs::from("net", "Fenhl", "ootrstats").ok_or(Error::MissingHomeDir)?.config_dir().join("worker-daemon.json")).await?)
        }
    }
}
