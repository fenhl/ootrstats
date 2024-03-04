use {
    std::{
        num::{
            NonZeroU8,
            NonZeroUsize,
        },
        path::PathBuf,
    },
    futures::stream::{
        FuturesUnordered,
        StreamExt as _,
    },
    if_chain::if_chain,
    serde::Deserialize,
    tokio::{
        process::Command,
        select,
        sync::mpsc,
        task::JoinHandle,
    },
    wheel::{
        fs,
        traits::AsyncCommandOutputExt as _,
    },
    ootrstats::{
        RandoSettings,
        RollOutput,
    },
    crate::SeedIdx,
};
#[cfg(windows)] use directories::UserDirs;
#[cfg(unix)] use std::path::Path;

pub(crate) enum Message {
    Init(String),
    Ready(u8),
    LocalSuccess {
        seed_idx: SeedIdx,
        /// present iff the `bench` parameter was set.
        instructions: Option<u64>,
        spoiler_log_path: PathBuf,
        ready: bool,
    },
    Failure {
        seed_idx: SeedIdx,
        /// present iff the `bench` parameter was set.
        instructions: Option<u64>,
        error_log: Vec<u8>,
        ready: bool,
    },
}

pub(crate) enum SupervisorMessage {
    Roll(SeedIdx),
}

#[derive(Deserialize)]
pub(crate) struct Config {
    pub(crate) name: String,
    #[serde(flatten)]
    pub(crate) kind: Kind,
}

fn make_neg_one() -> i8 { -1 }

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum Kind {
    #[serde(rename_all = "camelCase")]
    Local {
        base_rom_path: PathBuf,
        wsl_base_rom_path: Option<PathBuf>,
        #[serde(default = "make_neg_one")] // default to keeping one core free to avoid slowing down the supervisor too much
        cores: i8,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[error(transparent)] Roll(#[from] ootrstats::RollError),
    #[error(transparent)] Send(#[from] mpsc::error::SendError<(String, Message)>),
    #[error(transparent)] Task(#[from] tokio::task::JoinError),
    #[cfg(windows)]
    #[error("user folder not found")]
    MissingHomeDir,
}

impl Kind {
    async fn run(self, name: String, tx: mpsc::Sender<(String, Message)>, mut rx: mpsc::Receiver<SupervisorMessage>, rando_rev: git2::Oid, settings: RandoSettings, bench: bool) -> Result<(), Error> {
        match self {
            Self::Local { base_rom_path, wsl_base_rom_path, cores } => {
                tx.send((name.clone(), Message::Init(format!("cloning randomizer: determining repo path")))).await?;
                #[cfg(windows)] let repo_parent = UserDirs::new().ok_or(Error::MissingHomeDir)?.home_dir().join("git").join("github.com").join("OoTRandomizer").join("OoT-Randomizer").join("rev");
                #[cfg(unix)] let repo_parent = Path::new("/opt/git/github.com").join("OoTRandomizer").join("OoT-Randomizer").join("rev"); //TODO respect GITDIR envar and allow ~/git fallback
                let repo_path = repo_parent.join(rando_rev.to_string());
                tx.send((name.clone(), Message::Init(format!("checking if repo exists")))).await?;
                if !fs::exists(&repo_path).await? {
                    tx.send((name.clone(), Message::Init(format!("creating repo path")))).await?;
                    fs::create_dir_all(&repo_path).await?;
                    tx.send((name.clone(), Message::Init(format!("cloning randomizer: initializing repo")))).await?;
                    Command::new("git").arg("init").current_dir(&repo_path).check("git init").await?;
                    tx.send((name.clone(), Message::Init(format!("cloning randomizer: adding remote")))).await?;
                    Command::new("git").arg("remote").arg("add").arg("origin").arg("https://github.com/OoTRandomizer/OoT-Randomizer.git").current_dir(&repo_path).check("git remote add").await?;
                    tx.send((name.clone(), Message::Init(format!("cloning randomizer: fetching")))).await?;
                    Command::new("git").arg("fetch").arg("origin").arg(rando_rev.to_string()).arg("--depth=1").current_dir(&repo_path).check("git fetch").await?;
                    tx.send((name.clone(), Message::Init(format!("cloning randomizer: resetting")))).await?;
                    Command::new("git").arg("reset").arg("--hard").arg("FETCH_HEAD").current_dir(&repo_path).check("git reset").await?;
                }
                let mut first_seed_rolled = false;
                tx.send((name.clone(), Message::Ready(1))).await?; // on first roll, the randomizer decompresses the base rom, which is not reentrant
                let mut rando_tasks = FuturesUnordered::default();
                loop {
                    select! {
                        Some(res) = rando_tasks.next() => {
                            let () = res??;
                            if !first_seed_rolled {
                                let cores = NonZeroU8::try_from(u8::try_from(if cores <= 0 {
                                    std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN).get().try_into().unwrap_or(i8::MAX) + cores
                                } else {
                                    cores
                                }).unwrap_or(1)).unwrap_or(NonZeroU8::MIN).get();
                                tx.send((name.clone(), Message::Ready(cores))).await?;
                                first_seed_rolled = true;
                            }
                        }
                        Some(msg) = rx.recv() => match msg {
                            SupervisorMessage::Roll(seed_idx) => {
                                let tx = tx.clone();
                                let name = name.clone();
                                let base_rom_path = if_chain! {
                                    if cfg!(windows);
                                    if bench;
                                    if let Some(ref wsl_base_rom_path) = wsl_base_rom_path;
                                    then {
                                        wsl_base_rom_path.clone()
                                    } else {
                                        base_rom_path.clone()
                                    }
                                };
                                let repo_path = repo_path.clone();
                                let settings = settings.clone();
                                rando_tasks.push(tokio::spawn(async move {
                                    tx.send((name, match ootrstats::run_rando(&base_rom_path, &repo_path, &settings, bench).await? {
                                        RollOutput { instructions, log: Ok(spoiler_log_path) } => Message::LocalSuccess {
                                            ready: first_seed_rolled,
                                            seed_idx, instructions, spoiler_log_path,
                                        },
                                        RollOutput { instructions, log: Err(error_log) } => Message::Failure {
                                            ready: first_seed_rolled,
                                            seed_idx, instructions, error_log,
                                        },
                                    })).await?;
                                    Ok::<_, Error>(())
                                }));
                            }
                        },
                        else => break,
                    }
                }
                //TODO config option to automatically delete repo path (always/if it didn't already exist)
            }
        }
        Ok(())
    }
}

pub(crate) struct State {
    pub(crate) name: String,
    pub(crate) msg: Option<String>,
    pub(crate) ready: u8,
    pub(crate) running: u8,
    pub(crate) completed: u16,
    pub(crate) supervisor_tx: mpsc::Sender<SupervisorMessage>,
}

impl State {
    pub(crate) fn new(worker_tx: mpsc::Sender<(String, Message)>, name: String, kind: Kind, rando_rev: git2::Oid, settings: &RandoSettings, bench: bool) -> (JoinHandle<Result<(), Error>>, Self) {
        let (supervisor_tx, supervisor_rx) = mpsc::channel(256);
        (
            tokio::spawn(kind.run(name.clone(), worker_tx, supervisor_rx, rando_rev, settings.clone(), bench)),
            Self {
                msg: None,
                ready: 0,
                running: 0,
                completed: 0,
                name, supervisor_tx,
            }
        )
    }

    pub(crate) async fn roll(&mut self, seed_idx: SeedIdx) -> Result<(), mpsc::error::SendError<SupervisorMessage>> {
        self.supervisor_tx.send(SupervisorMessage::Roll(seed_idx)).await?;
        self.ready -= 1;
        self.running += 1;
        Ok(())
    }
}
