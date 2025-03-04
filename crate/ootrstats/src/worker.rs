use {
    std::{
        borrow::Cow,
        collections::HashMap,
        env,
        num::{
            NonZeroU8,
            NonZeroUsize,
        },
        path::PathBuf,
        time::Duration,
    },
    async_proto::Protocol,
    bytes::Bytes,
    directories::UserDirs,
    either::Either,
    futures::{
        future::{
            self,
            FutureExt as _,
        },
        stream::{
            FuturesUnordered,
            StreamExt as _,
        },
    },
    if_chain::if_chain,
    lazy_regex::regex_captures,
    rand::{
        prelude::*,
        rng,
    },
    semver::Version,
    tokio::{
        select,
        process::Command,
        sync::mpsc,
        time::{
            Instant,
            sleep_until,
        },
    },
    wheel::{
        fs,
        traits::AsyncCommandOutputExt as _,
    },
    crate::{
        OutputMode,
        RandoSetup,
        RollOutput,
        SeedIdx,
        gitdir,
    },
};
#[cfg(unix)] use std::io;

pub enum Message {
    Init(String),
    Ready(u8),
    Success {
        seed_idx: SeedIdx,
        /// present if the `bench` parameter was set and `perf` output was parsed successfully.
        instructions: Result<u64, Bytes>,
        rsl_instructions: Result<u64, Bytes>,
        spoiler_log: Either<PathBuf, Bytes>,
        patch: Option<Either<(Option<Option<String>>, PathBuf), (String, Bytes)>>,
    },
    Failure {
        seed_idx: SeedIdx,
        /// present if the `bench` parameter was set and `perf` output was parsed successfully.
        instructions: Result<u64, Bytes>,
        rsl_instructions: Result<u64, Bytes>,
        error_log: Bytes,
    },
}

#[derive(Debug, Protocol)]
pub enum SupervisorMessage {
    Roll(SeedIdx),
    Cancel(SeedIdx),
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)] CargoMetadata(#[from] cargo_metadata::Error),
    #[error(transparent)] Decompress(#[from] decompress::Error),
    #[error(transparent)] Env(#[from] env::VarError),
    #[error(transparent)] ParseGitHash(#[from] gix_hash::decode::Error),
    #[cfg(unix)] #[error(transparent)] ParseInt(#[from] std::num::ParseIntError),
    #[error(transparent)] Roll(#[from] crate::RollError),
    #[error(transparent)] Semver(#[from] semver::Error),
    #[error(transparent)] Send(#[from] mpsc::error::SendError<Message>),
    #[error(transparent)] Task(#[from] tokio::task::JoinError),
    #[error(transparent)] Utf8(#[from] std::string::FromUtf8Error),
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[error("user folder not found")]
    MissingHomeDir,
    #[error("failed to determine randomizer version from RSL script")]
    RslVersion,
}

/// If the worker is not ready to roll a new seed right now, this returns:
///
/// * The duration after which this function may be called again to recheck.
/// * The reason for the wait as a human-readable string.
async fn wait_ready(#[cfg_attr(not(windows), allow(unused))] priority_users: &[String]) -> Result<Option<(Duration, String)>, Error> {
    let mut wait = Duration::default();
    let mut message = String::default();
    #[cfg(unix)] match fs::read_to_string("/sys/class/thermal/thermal_zone0/temp").await {
        Ok(temp) => if temp.trim().parse::<i32>()? >= 80000 {
            let jitter = rng().random_range(0..10);
            let new_wait = Duration::from_secs(55 + jitter);
            if new_wait > wait {
                wait = new_wait;
                message = format!("waiting for CPU to cool down below 80°C");
            }
        }
        Err(wheel::Error::Io { inner, .. }) if inner.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    #[cfg(windows)] if !priority_users.is_empty() {
        // wait until no priority users are signed in
        let get_process = Command::new("pwsh").arg("-c").arg("Get-Process -IncludeUserName | Select-Object -Unique -Property UserName").check("pwsh Get-Process").await?;
        let get_process_stdout = String::from_utf8_lossy(&get_process.stdout);
        //TODO this checks the entire Get-Process output for the given username. Usernames appearing in the table header can cause false positives. Consider requesting XML output from pwsh and parsing that
        if let Some(priority_user) = priority_users.iter().find(|&priority_user| get_process_stdout.contains(priority_user)) {
            let jitter = rng().random_range(0..120);
            let new_wait = Duration::from_secs(14 * 60 + jitter);
            if new_wait > wait {
                wait = new_wait;
                message = format!("waiting for {priority_user} to sign out");
            }
        }
    }
    Ok(if wait > Duration::default() { Some((wait, message)) } else { None })
}

pub async fn work(tx: mpsc::Sender<Message>, mut rx: mpsc::Receiver<SupervisorMessage>, base_rom_path: PathBuf, cores: i8, wsl_distro: Option<String>, git_rev: gix_hash::ObjectId, setup: RandoSetup, output_mode: OutputMode, priority_users: &[String]) -> Result<(), Error> {
    let mut rsl_version = None;
    let mut use_rust_cli = false;
    let mut supports_unsalted_seeds = false;
    let (rando_github_user, rando_repo_name, rando_git_rev, mut rando_repo_path) = match setup {
        RandoSetup::Normal { ref github_user, ref repo, .. } => {
            tx.send(Message::Init(format!("cloning randomizer: determining repo path"))).await?;
            (
                Cow::Borrowed(&**github_user),
                Cow::Borrowed(&**repo),
                git_rev,
                gitdir().await?.join("github.com").join(github_user).join(repo).join("rev").join(git_rev.to_string()),
            )
        }
        RandoSetup::Rsl { ref github_user, ref repo, .. } => {
            tx.send(Message::Init(format!("cloning random settings script: determining repo path"))).await?;
            let repo_path = gitdir().await?.join("github.com").join(github_user).join(repo).join("rev").join(git_rev.to_string());
            tx.send(Message::Init(format!("checking if RSL repo exists"))).await?;
            if !fs::exists(&repo_path).await? {
                tx.send(Message::Init(format!("creating RSL repo path"))).await?;
                fs::create_dir_all(&repo_path).await?;
                tx.send(Message::Init(format!("cloning random settings script: initializing repo"))).await?;
                Command::new("git").arg("init").current_dir(&repo_path).check("git init").await?;
                tx.send(Message::Init(format!("cloning random settings script: adding remote"))).await?;
                Command::new("git").arg("remote").arg("add").arg("origin").arg(format!("https://github.com/{github_user}/{repo}.git")).current_dir(&repo_path).check("git remote add").await?;
                tx.send(Message::Init(format!("cloning random settings script: fetching"))).await?;
                Command::new("git").arg("fetch").arg("origin").arg(git_rev.to_string()).arg("--depth=1").current_dir(&repo_path).check("git fetch").await?;
                tx.send(Message::Init(format!("cloning random settings script: resetting"))).await?;
                Command::new("git").arg("reset").arg("--hard").arg("FETCH_HEAD").current_dir(&repo_path).check("git reset").await?;
            }
            tx.send(Message::Init(format!("copying base rom to RSL repo"))).await?;
            let rsl_data_dir = repo_path.join("data");
            let rsl_base_rom_path = rsl_data_dir.join("oot-ntscu-1.0.n64");
            if !fs::exists(&rsl_base_rom_path).await? {
                tx.send(Message::Init(format!("decompressing base rom"))).await?;
                fs::create_dir_all(rsl_data_dir).await?;
                fs::write(rsl_base_rom_path, decompress::decompress(&mut fs::read(&base_rom_path).await?)?).await?;
            }
            let python = crate::python().await?;
            rsl_version = Some(String::from_utf8(Command::new(&python)
                .arg("-c")
                .arg("import rslversion; print(rslversion.__version__, end='', flush=True)")
                .current_dir(&repo_path)
                .check(python.display().to_string()).await?
                .stdout
            )?);
            let mut rando_github_user = String::from_utf8(Command::new(&python)
                .arg("-c")
                .arg("import rslversion; print(rslversion.randomizer_repo, end='', flush=True)")
                .current_dir(&repo_path)
                .check(python.display().to_string()).await?
                .stdout
            )?;
            let rando_repo_slash = rando_github_user.find('/').ok_or(Error::RslVersion)?;
            let rando_repo_name = rando_github_user.split_off(rando_repo_slash + 1);
            rando_github_user.pop().expect("should end with slash found above");
            let randomizer_commit = String::from_utf8(Command::new(&python)
                .arg("-c")
                .arg("import rslversion; print(rslversion.randomizer_commit, end='', flush=True)")
                .current_dir(&repo_path)
                .check(python.display().to_string()).await?
                .stdout
            )?;
            (Cow::Owned(rando_github_user), Cow::Owned(rando_repo_name), randomizer_commit.parse()?, repo_path.join("randomizer"))
        }
    };
    tx.send(Message::Init(format!("checking if randomizer repo exists"))).await?;
    if !fs::exists(&rando_repo_path).await? {
        tx.send(Message::Init(format!("creating randomizer repo path"))).await?;
        fs::create_dir_all(&rando_repo_path).await?;
        tx.send(Message::Init(format!("cloning randomizer: initializing repo"))).await?;
        Command::new("git").arg("init").current_dir(&rando_repo_path).check("git init").await?;
        tx.send(Message::Init(format!("cloning randomizer: adding remote"))).await?;
        Command::new("git").arg("remote").arg("add").arg("origin").arg(format!("https://github.com/{rando_github_user}/{rando_repo_name}.git")).current_dir(&rando_repo_path).check("git remote add").await?;
        tx.send(Message::Init(format!("cloning randomizer: fetching"))).await?;
        Command::new("git").arg("fetch").arg("origin").arg(rando_git_rev.to_string()).arg("--depth=1").current_dir(&rando_repo_path).check("git fetch").await?;
        tx.send(Message::Init(format!("cloning randomizer: resetting"))).await?;
        Command::new("git").arg("reset").arg("--hard").arg("FETCH_HEAD").current_dir(&rando_repo_path).check("git reset").await?;
    }
    if !fs::exists(rando_repo_path.join("ZOOTDEC.z64")).await? {
        tx.send(Message::Init(format!("decompressing base rom"))).await?;
        fs::write(rando_repo_path.join("ZOOTDEC.z64"), decompress::decompress(&mut fs::read(&base_rom_path).await?)?).await?;
    }
    let cargo_manifest_path = rando_repo_path.join("Cargo.toml");
    if fs::exists(&cargo_manifest_path).await? {
        let cargo_command = || Ok::<_, Error>(if cfg!(target_os = "windows") && matches!(output_mode, OutputMode::Bench | OutputMode::BenchUncompressed) {
            //TODO update Rust toolchain on WSL
            let mut cargo = Command::new(crate::WSL);
            if let Some(wsl_distro) = &wsl_distro {
                cargo.arg("--distribution");
                cargo.arg(wsl_distro);
            }
            cargo.arg("cargo");
            cargo
        } else {
            let mut cargo = Command::new("cargo");
            if let Some(user_dirs) = UserDirs::new() {
                cargo.env("PATH", format!("{}:{}", user_dirs.home_dir().join(".cargo").join("bin").display(), env::var("PATH")?));
            }
            cargo
        });
        #[cfg(target_os = "windows")] let rust_library_filename = if let OutputMode::Bench | OutputMode::BenchUncompressed = output_mode { "rs.so" } else { "rs.dll" };
        #[cfg(any(target_os = "linux", target_os = "macos"))] let rust_library_filename = "rs.so";
        if matches!(output_mode, OutputMode::Bench | OutputMode::BenchUncompressed) || !fs::exists(rando_repo_path.join(rust_library_filename)).await? {
            //TODO update Rust
            tx.send(Message::Init(format!("building Rust code"))).await?;
            let mut cargo = cargo_command()?;
            cargo.arg("build");
            cargo.arg("--lib"); // old versions of the riir branch were organized as a single crate with multiple targets
            cargo.arg("--release");
            cargo.arg("--package=ootr-python");
            cargo.current_dir(&rando_repo_path);
            cargo.check("cargo build").await?;
            tx.send(Message::Init(format!("copying Rust module"))).await?;
            #[cfg(target_os = "windows")] {
                if let OutputMode::Bench | OutputMode::BenchUncompressed = output_mode {
                    let mut cp = Command::new(crate::WSL);
                    if let Some(wsl_distro) = &wsl_distro {
                        cp.arg("--distribution");
                        cp.arg(wsl_distro);
                    }
                    cp.arg("cp").arg("target/release/librs.so").arg("rs.so").current_dir(&rando_repo_path).check("wsl cp").await?;
                } else {
                    fs::copy(rando_repo_path.join("target").join("release").join("rs.dll"), rando_repo_path.join("rs.pyd")).await?;
                }
            }
            #[cfg(target_os = "linux")] fs::copy(rando_repo_path.join("target").join("release").join("librs.so"), rando_repo_path.join("rs.so")).await?;
            #[cfg(target_os = "macos")] fs::copy(rando_repo_path.join("target").join("release").join("librs.dylib"), rando_repo_path.join("rs.so")).await?;
        }
        let mut metadata_cmd = cargo_metadata::MetadataCommand::new();
        if let Some(user_dirs) = UserDirs::new() {
            metadata_cmd.env("PATH", format!("{}:{}", user_dirs.home_dir().join(".cargo").join("bin").display(), env::var("PATH")?));
        }
        if let Some(package) = metadata_cmd
            .manifest_path(cargo_manifest_path)
            .exec()?
            .packages
            .into_iter()
            .find(|package| package.name == "ootr-cli")
        {
            use_rust_cli = package.version >= Version { major: 8, minor: 2, patch: 49, pre: "fenhl.1.riir.2".parse()?, build: semver::BuildMetadata::default() };
            supports_unsalted_seeds = package.version >= Version { major: 8, minor: 2, patch: 54, pre: "fenhl.2.riir.2".parse()?, build: semver::BuildMetadata::default() };
        }
        if use_rust_cli {
            tx.send(Message::Init(format!("building Rust CLI"))).await?;
            let mut cargo = cargo_command()?;
            cargo.arg("build");
            cargo.arg("--release");
            cargo.arg("--package=ootr-cli"); // old versions of the riir branch had ootr-python as the default crate
            cargo.current_dir(&rando_repo_path);
            cargo.check("cargo build").await?;
        }
    } else {
        let version_py = fs::read_to_string(rando_repo_path.join("version.py")).await?;
        if let Some(base_version) = version_py.lines()
            .filter_map(|line| regex_captures!("^__version__ = '([0-9.]+)'$", line))
            .find_map(|(_, base_version)| base_version.parse::<Version>().ok())
        {
            if let Some(supplementary_version) = version_py.lines()
                .filter_map(|line| regex_captures!("^supplementary_version = ([0-9]+)$", line))
                .find_map(|(_, supplementary_version)| supplementary_version.parse::<u8>().ok())
            {
                supports_unsalted_seeds = (base_version, supplementary_version) >= (Version::new(8, 2, 54), 2);
            }
        }
    }
    if fs::exists(rando_repo_path.join("mypy.ini")).await? {
        tx.send(Message::Init(format!("compiling Python code with mypyc"))).await?;
        let mut mypyc = if cfg!(target_os = "windows") && matches!(output_mode, OutputMode::Bench | OutputMode::BenchUncompressed) {
            let mut mypyc = Command::new(crate::WSL);
            if let Some(wsl_distro) = &wsl_distro {
                mypyc.arg("--distribution");
                mypyc.arg(wsl_distro);
            }
            mypyc.arg("mypyc");
            mypyc
        } else {
            Command::new("mypyc")
        };
        mypyc.current_dir(&rando_repo_path).check("mypyc").await?;
    }
    let (repo_path, random_seeds) = match setup {
        RandoSetup::Normal { random_seeds, .. } => {
            (rando_repo_path, random_seeds)
        }
        RandoSetup::Rsl { .. } => {
            assert!(rando_repo_path.pop(), "rando_repo_path was root even though it was defined as subdirectory of repo_path");
            (rando_repo_path, true)
        }
    };
    let mut msg_buf = Vec::default();
    'wait_ready: while let Some((duration, reason)) = wait_ready(priority_users).await? {
        tx.send(Message::Init(reason)).await?;
        let recheck_ready_at = Instant::now() + duration;
        loop {
            select! {
                () = sleep_until(recheck_ready_at) => continue 'wait_ready,
                msg = rx.recv() => if let Some(msg) = msg {
                    msg_buf.push(msg);
                } else {
                    break 'wait_ready
                },
            }
        }
    }
    tx.send(Message::Ready(NonZeroU8::try_from(u8::try_from(if cores <= 0 {
        std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN).get().try_into().unwrap_or(i8::MAX) + cores
    } else {
        cores
    }).unwrap_or(1)).unwrap_or(NonZeroU8::MIN).get())).await?;
    let mut rando_tasks = FuturesUnordered::default();
    let mut abort_handles = HashMap::<_, Vec<_>>::default();
    let mut recheck_ready_at = None;
    let mut waiting_cores = 0;
    let handle_seed = |seed_idx| {
        let run_future = match setup {
            RandoSetup::Normal { ref settings, ref json_settings, world_counts, .. } => {
                let wsl_distro = wsl_distro.clone();
                let repo_path = repo_path.clone();
                let settings = settings.clone();
                let json_settings = json_settings.clone();
                Either::Left(async move { crate::run_rando(wsl_distro.as_deref(), &repo_path, use_rust_cli, supports_unsalted_seeds, random_seeds, &settings, &json_settings, world_counts, seed_idx, output_mode).await })
            }
            RandoSetup::Rsl { ref preset, .. } => {
                let wsl_distro = wsl_distro.clone();
                let repo_path = repo_path.clone();
                let rsl_version = rsl_version.clone().unwrap();
                let preset = preset.clone();
                Either::Right(async move { crate::run_rsl(wsl_distro.as_deref(), &repo_path, &rsl_version, use_rust_cli, supports_unsalted_seeds, random_seeds, preset.as_deref(), seed_idx, output_mode).await })
            }
        };
        let tx = tx.clone();
        let wsl_distro = wsl_distro.clone();
        tokio::spawn(async move {
            tx.send(match run_future.await? {
                RollOutput { instructions, rsl_instructions, log: Ok(spoiler_log_path), patch } => Message::Success {
                    spoiler_log: Either::Left(spoiler_log_path),
                    patch: patch.map(|(is_wsl, patch)| Either::Left((is_wsl.then(|| wsl_distro.clone()), patch))),
                    seed_idx, instructions, rsl_instructions,
                },
                RollOutput { instructions, rsl_instructions, log: Err(error_log), patch: _ } => Message::Failure {
                    seed_idx, instructions, rsl_instructions, error_log,
                },
            }).await?;
            Ok::<_, Error>(())
        })
    };
    for msg in msg_buf {
        match msg {
            SupervisorMessage::Roll(seed_idx) => {
                let rando_task = handle_seed(seed_idx);
                abort_handles.entry(seed_idx).or_default().push(rando_task.abort_handle());
                rando_tasks.push(rando_task);
            }
            SupervisorMessage::Cancel(seed_idx) => for abort_handle in abort_handles.get(&seed_idx).into_iter().flatten() {
                abort_handle.abort();
            },
        }
    }
    loop {
        let rx_is_closed = rx.is_closed();
        let recheck_ready = if_chain! {
            if !rx_is_closed;
            if let Some(recheck_ready_at) = recheck_ready_at;
            then {
                Either::Left(sleep_until(recheck_ready_at).map(Some))
            } else {
                Either::Right(future::ready(None))
            }
        };
        select! {
            Some(()) = recheck_ready => if let Some((duration, reason)) = wait_ready(priority_users).await? {
                tx.send(Message::Init(reason)).await?;
                recheck_ready_at = Some(Instant::now() + duration);
            } else {
                tx.send(Message::Ready(waiting_cores)).await?;
                waiting_cores = 0;
                recheck_ready_at = None;
            },
            Some(res) = rando_tasks.next() => {
                match res {
                    Ok(res) => { let () = res?; }
                    Err(e) if e.is_cancelled() => {} // a seed task being cancelled is expected with --race
                    Err(e) => return Err(e.into()),
                }
                if let Some((duration, reason)) = wait_ready(priority_users).await? {
                    tx.send(Message::Init(reason)).await?;
                    if let Some(ref mut recheck_ready_at) = recheck_ready_at {
                        *recheck_ready_at = (*recheck_ready_at).min(Instant::now() + duration);
                    } else {
                        recheck_ready_at = Some(Instant::now() + duration);
                    }
                    waiting_cores += 1;
                } else {
                    tx.send(Message::Ready(1)).await?;
                }
            }
            msg = rx.recv(), if !rx_is_closed => if let Some(msg) = msg {
                match msg {
                    SupervisorMessage::Roll(seed_idx) => {
                        let rando_task = handle_seed(seed_idx);
                        abort_handles.entry(seed_idx).or_default().push(rando_task.abort_handle());
                        rando_tasks.push(rando_task);
                    }
                    SupervisorMessage::Cancel(seed_idx) => for abort_handle in abort_handles.get(&seed_idx).into_iter().flatten() {
                        abort_handle.abort();
                    },
                }
            } else {
                // stop awaiting recheck_ready
            },
            else => break,
        }
    }
    //TODO config option to automatically delete repo path (always/if it didn't already exist)
    Ok(())
}
