use {
    std::{
        num::{
            NonZeroU8,
            NonZeroUsize,
        },
        path::PathBuf,
    },
    async_proto::Protocol,
    bytes::Bytes,
    either::Either,
    futures::stream::{
        FuturesUnordered,
        StreamExt as _,
    },
    tokio::{
        select,
        process::Command,
        sync::mpsc,
    },
    wheel::{
        fs,
        traits::AsyncCommandOutputExt as _,
    },
    crate::{
        RandoSetup,
        RollOutput,
        SeedIdx,
    },
};
#[cfg(windows)] use directories::UserDirs;
#[cfg(unix)] use std::path::Path;

pub enum Message {
    Init(String),
    Ready(u8),
    Success {
        seed_idx: SeedIdx,
        /// present iff the `bench` parameter was set.
        instructions: Option<u64>,
        spoiler_log: Either<PathBuf, Bytes>,
        ready: bool,
    },
    Failure {
        seed_idx: SeedIdx,
        /// present iff the `bench` parameter was set.
        instructions: Option<u64>,
        error_log: Bytes,
        ready: bool,
    },
}

#[derive(Protocol)]
pub enum SupervisorMessage {
    Roll(SeedIdx),
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[error(transparent)] Roll(#[from] crate::RollError),
    #[error(transparent)] Send(#[from] mpsc::error::SendError<Message>),
    #[error(transparent)] Task(#[from] tokio::task::JoinError),
    #[cfg(windows)]
    #[error("user folder not found")]
    MissingHomeDir,
}

pub async fn work(tx: mpsc::Sender<Message>, mut rx: mpsc::Receiver<SupervisorMessage>, base_rom_path: PathBuf, cores: i8, rando_rev: git2::Oid, setup: RandoSetup, bench: bool) -> Result<(), Error> {
    let repo_path = match setup {
        RandoSetup::Normal { ref github_user, .. } => {
            tx.send(Message::Init(format!("cloning randomizer: determining repo path"))).await?;
            #[cfg(windows)] let repo_parent = UserDirs::new().ok_or(Error::MissingHomeDir)?.home_dir().join("git").join("github.com").join(github_user).join("OoT-Randomizer").join("rev");
            #[cfg(unix)] let repo_parent = Path::new("/opt/git/github.com").join(github_user).join("OoT-Randomizer").join("rev"); //TODO respect GITDIR envar and allow ~/git fallback
            let repo_path = repo_parent.join(rando_rev.to_string());
            tx.send(Message::Init(format!("checking if repo exists"))).await?;
            if !fs::exists(&repo_path).await? {
                tx.send(Message::Init(format!("creating repo path"))).await?;
                fs::create_dir_all(&repo_path).await?;
                tx.send(Message::Init(format!("cloning randomizer: initializing repo"))).await?;
                Command::new("git").arg("init").current_dir(&repo_path).check("git init").await?;
                tx.send(Message::Init(format!("cloning randomizer: adding remote"))).await?;
                Command::new("git").arg("remote").arg("add").arg("origin").arg(format!("https://github.com/{github_user}/OoT-Randomizer.git")).current_dir(&repo_path).check("git remote add").await?;
                tx.send(Message::Init(format!("cloning randomizer: fetching"))).await?;
                Command::new("git").arg("fetch").arg("origin").arg(rando_rev.to_string()).arg("--depth=1").current_dir(&repo_path).check("git fetch").await?;
                tx.send(Message::Init(format!("cloning randomizer: resetting"))).await?;
                Command::new("git").arg("reset").arg("--hard").arg("FETCH_HEAD").current_dir(&repo_path).check("git reset").await?;
            }
            repo_path
        }
        RandoSetup::Rsl { ref github_user, .. } => {
            tx.send(Message::Init(format!("cloning random settings script: determining repo path"))).await?;
            #[cfg(windows)] let repo_parent = UserDirs::new().ok_or(Error::MissingHomeDir)?.home_dir().join("git").join("github.com").join(github_user).join("plando-random-settings").join("rev");
            #[cfg(unix)] let repo_parent = Path::new("/opt/git/github.com").join(github_user).join("plando-random-settings").join("rev"); //TODO respect GITDIR envar and allow ~/git fallback
            let repo_path = repo_parent.join(rando_rev.to_string());
            tx.send(Message::Init(format!("checking if repo exists"))).await?;
            if !fs::exists(&repo_path).await? {
                tx.send(Message::Init(format!("creating repo path"))).await?;
                fs::create_dir_all(&repo_path).await?;
                tx.send(Message::Init(format!("cloning random settings script: initializing repo"))).await?;
                Command::new("git").arg("init").current_dir(&repo_path).check("git init").await?;
                tx.send(Message::Init(format!("cloning random settings script: adding remote"))).await?;
                Command::new("git").arg("remote").arg("add").arg("origin").arg(format!("https://github.com/{github_user}/plando-random-settings.git")).current_dir(&repo_path).check("git remote add").await?;
                tx.send(Message::Init(format!("cloning random settings script: fetching"))).await?;
                Command::new("git").arg("fetch").arg("origin").arg(rando_rev.to_string()).arg("--depth=1").current_dir(&repo_path).check("git fetch").await?;
                tx.send(Message::Init(format!("cloning random settings script: resetting"))).await?;
                Command::new("git").arg("reset").arg("--hard").arg("FETCH_HEAD").current_dir(&repo_path).check("git reset").await?;
            }
            tx.send(Message::Init(format!("copying base rom to RSL repo"))).await?;
            let rsl_base_rom_path = repo_path.join("data").join("oot-ntscu-1.0.z64");
            if cfg!(target_os = "windows") && bench {
                Command::new(crate::WSL).arg("cp").arg(&base_rom_path).arg("data/oot-ntscu-1.0.z64").current_dir(&repo_path).check("wsl cp").await?;
            } else {
                if !fs::exists(&rsl_base_rom_path).await? {
                    fs::copy(&base_rom_path, rsl_base_rom_path).await?;
                }
            }
            repo_path
        }
    };
    let mut first_seed_rolled = false;
    tx.send(Message::Ready(1)).await?; // on first roll, the randomizer decompresses the base rom, and the RSL script downloads and extracts the randomizer, neither of which are reentrant
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
                    tx.send(Message::Ready(cores)).await?;
                    first_seed_rolled = true;
                }
            }
            Some(msg) = rx.recv() => match msg {
                SupervisorMessage::Roll(seed_idx) => match setup {
                    RandoSetup::Normal { ref settings, .. } => {
                        let tx = tx.clone();
                        let base_rom_path = base_rom_path.clone();
                        let repo_path = repo_path.clone();
                        let settings = settings.clone();
                        rando_tasks.push(tokio::spawn(async move {
                            tx.send(match crate::run_rando(&base_rom_path, &repo_path, &settings, bench).await? {
                                RollOutput { instructions, log: Ok(spoiler_log_path) } => Message::Success {
                                    spoiler_log: Either::Left(spoiler_log_path),
                                    ready: first_seed_rolled,
                                    seed_idx, instructions,
                                },
                                RollOutput { instructions, log: Err(error_log) } => Message::Failure {
                                    ready: first_seed_rolled,
                                    seed_idx, instructions, error_log,
                                },
                            }).await?;
                            Ok::<_, Error>(())
                        }));
                    }
                    RandoSetup::Rsl { .. } => {
                        let tx = tx.clone();
                        let repo_path = repo_path.clone();
                        rando_tasks.push(tokio::spawn(async move {
                            tx.send(match crate::run_rsl(&repo_path, bench).await? {
                                RollOutput { instructions, log: Ok(spoiler_log_path) } => Message::Success {
                                    spoiler_log: Either::Left(spoiler_log_path),
                                    ready: first_seed_rolled,
                                    seed_idx, instructions,
                                },
                                RollOutput { instructions, log: Err(error_log) } => Message::Failure {
                                    ready: first_seed_rolled,
                                    seed_idx, instructions, error_log,
                                },
                            }).await?;
                            Ok(())
                        }));
                    }
                }
            },
            else => break,
        }
    }
    //TODO config option to automatically delete repo path (always/if it didn't already exist)
    Ok(())
}
