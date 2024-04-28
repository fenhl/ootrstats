use {
    std::{
        collections::{
            HashMap,
            VecDeque,
        },
        ffi::OsString,
        io::{
            IsTerminal as _,
            stderr,
        },
        mem,
        num::NonZeroUsize,
        path::PathBuf,
    },
    chrono::{
        TimeDelta,
        prelude::*,
    },
    crossterm::{
        cursor::{
            MoveDown,
            MoveToColumn,
            MoveUp,
        },
        event::{
            KeyCode,
            KeyEvent,
            KeyEventKind,
            KeyModifiers,
        },
        style::Print,
        terminal::{
            Clear,
            ClearType,
            disable_raw_mode,
            enable_raw_mode,
        },
    },
    either::Either,
    futures::{
        future::{
            FutureExt as _,
            TryFutureExt as _,
        },
        stream::{
            FuturesUnordered,
            StreamExt as _,
        },
    },
    git2::Repository,
    if_chain::if_chain,
    itertools::Itertools as _,
    lazy_regex::regex_is_match,
    ootr_utils::spoiler::SpoilerLog,
    serde::{
        Deserialize,
        Serialize,
    },
    tokio::{
        io,
        process::Command,
        select,
        sync::mpsc,
        task::JoinError,
        time::Instant,
    },
    wheel::{
        fs,
        traits::{
            AsyncCommandOutputExt as _,
            IoResultExt as _,
        },
    },
    ootrstats::{
        OutputMode,
        RandoSettings,
        RandoSetup,
        SeedIdx,
        WSL,
    },
    crate::config::Config,
};
#[cfg(windows)] use directories::{
    ProjectDirs,
    UserDirs,
};
#[cfg(unix)] use {
    std::path::Path,
    xdg::BaseDirectories,
};

mod config;
mod worker;

enum ReaderMessage {
    Pending(SeedIdx),
    Success {
        seed_idx: SeedIdx,
        instructions: Option<u64>,
    },
    Failure {
        seed_idx: SeedIdx,
        instructions: Option<u64>,
    },
    Done,
}

#[derive(Deserialize, Serialize)]
struct Metadata {
    /// present iff the `bench` parameter was set.
    instructions: Option<u64>,
}

fn parse_json_object(arg: &str) -> Result<serde_json::Map<String, serde_json::Value>, serde_json::Error> {
    serde_json::from_str(arg)
}

#[derive(Clone, clap::Parser)]
#[clap(version)]
struct Args {
    /// Sample size — how many seeds to roll.
    #[clap(short, long, default_value = "16384", default_value_if("world_counts", "true", Some("255")))]
    num_seeds: SeedIdx,
    /// Run the benchmarking suite.
    #[clap(long, conflicts_with("rsl"), conflicts_with("preset"))]
    suite: bool,
    #[clap(long)]
    rsl: bool,
    #[clap(short = 'u', long, default_value = "OoTRandomizer", default_value_if("rsl", "true", Some("matthewkirby")))]
    github_user: String,
    #[clap(short, long)]
    branch: Option<String>,
    #[clap(long, conflicts_with("branch"))]
    rev: Option<git2::Oid>,
    #[clap(short, long, conflicts_with("rsl"))]
    preset: Option<String>,
    /// Settings string for the randomizer.
    #[clap(long, conflicts_with("rsl"), conflicts_with("preset"))]
    settings: Option<String>,
    /// Specifies a JSON object of settings on the command line that will override the given preset or settings string.
    #[clap(long, conflicts_with("rsl"), default_value = "{}", value_parser = parse_json_object)]
    json_settings: serde_json::Map<String, serde_json::Value>,
    /// Generate seeds with varying world counts.
    #[clap(long, conflicts_with("rsl"))]
    world_counts: bool,
    /// Generate .zpf/.zpfz patch files.
    #[clap(long, conflicts_with("rsl"))]
    patch: bool,
    #[clap(long)]
    retry_failures: bool,
    #[clap(subcommand)]
    subcommand: Option<Subcommand>,
}

#[derive(Clone, clap::Subcommand)]
enum Subcommand {
    /// Benchmark — measure average CPU instructions to generate a seed.
    Bench {
        #[clap(long)]
        raw_data: bool,
    },
    /// Count chest appearances in Mido's house for the midos.house favicon
    MidosHouse {
        out_path: PathBuf,
    },
}

enum SubcommandData {
    None {
        num_successes: u16,
        num_failures: u16,
    },
    Bench {
        instructions_success: Vec<u64>,
        instructions_failure: Vec<u64>,
        raw_data: bool,
    },
    MidosHouse {
        out_path: PathBuf,
        spoiler_logs: Vec<SpoilerLog>,
        num_failures: u16,
    },
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)] Config(#[from] config::Error),
    #[error(transparent)] Git(#[from] git2::Error),
    #[error(transparent)] Json(#[from] serde_json::Error),
    #[error(transparent)] Task(#[from] JoinError),
    #[error(transparent)] ReaderSend(#[from] mpsc::error::SendError<ReaderMessage>),
    #[error(transparent)] WorkerSend(#[from] mpsc::error::SendError<ootrstats::worker::SupervisorMessage>),
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[cfg(unix)] #[error(transparent)] Xdg(#[from] xdg::BaseDirectoriesError),
    #[cfg(windows)]
    #[error("user folder not found")]
    MissingHomeDir,
    #[error("found both spoiler and error logs for a seed")]
    SuccessAndFailure,
    #[error("at most 255 seeds may be generated with the --world-counts option")]
    TooManyWorlds,
    #[error("error in worker {worker}: {source}")]
    Worker {
        worker: String,
        source: worker::Error,
    },
    #[error("received a message from an unknown worker")]
    WorkerNotFound,
}

impl wheel::CustomExit for Error {
    fn exit(self, cmd_name: &'static str) -> ! {
        eprintln!("\r");
        match self {
            Self::Worker { worker, source: worker::Error::Local(ootrstats::worker::Error::Roll(ootrstats::RollError::PerfSyntax(stderr))) } => {
                eprintln!("{cmd_name}: roll error in worker {worker}: failed to parse `perf` output\r");
                eprintln!("stderr:\r");
                eprintln!("{}\r", String::from_utf8_lossy(&stderr).lines().filter(|line| !regex_is_match!("^[0-9]+ files remaining$", line)).format("\r\n"));
            }
            _ => {
                let mut debug = format!("{self:?}");
                if debug.len() > 2000 && stderr().is_terminal() {
                    let mut prefix_end = 1000;
                    while !debug.is_char_boundary(prefix_end) {
                        prefix_end -= 1;
                    }
                    let mut suffix_start = debug.len() - 1000;
                    while !debug.is_char_boundary(suffix_start) {
                        suffix_start += 1;
                    }
                    debug = format!("{} […] {}", &debug[..prefix_end], &debug[suffix_start..]);
                }
                eprintln!("{cmd_name}: {self}\r");
                eprintln!("debug info: {debug}\r");
            }
        }
        std::process::exit(1)
    }
}

async fn cli(args: Args) -> Result<(), Error> {
    if args.world_counts && args.num_seeds > 255 {
        return Err(Error::TooManyWorlds)
    }
    let (cli_tx, mut cli_rx) = mpsc::channel(256);
    tokio::spawn(async move {
        let mut cli_events = crossterm::event::EventStream::default();
        while let Some(event) = cli_events.next().await {
            if cli_tx.send(event).await.is_err() { break }
        }
    });
    let mut stderr = stderr();
    crossterm::execute!(stderr,
        Print("preparing..."),
    ).at_unknown()?;
    let mut config = Config::load().await?;
    let rando_rev = if let Some(rev) = args.rev {
        rev
    } else {
        let repo_name = if args.rsl { "plando-random-settings" } else { "OoT-Randomizer" };
        #[cfg(windows)] let mut dir_parent = UserDirs::new().ok_or(Error::MissingHomeDir)?.home_dir().join("git").join("github.com").join(&args.github_user).join(repo_name);
        #[cfg(unix)] let mut dir_parent = Path::new("/opt/git/github.com").join(&args.github_user).join(repo_name); //TODO respect GITDIR envar and allow ~/git fallback
        let dir_name = if let Some(ref branch) = args.branch {
            dir_parent = dir_parent.join("branch");
            branch
        } else {
            "main"
        };
        let dir = dir_parent.join(dir_name);
        if fs::exists(&dir).await? {
            Command::new("git")
                .arg("pull")
                .current_dir(&dir)
                .check("git pull").await?;
        } else {
            fs::create_dir_all(&dir_parent).await?;
            let mut cmd = Command::new("git");
            cmd.arg("clone");
            cmd.arg("--depth=1");
            cmd.arg(format!("https://github.com/{}/{repo_name}.git", args.github_user));
            if let Some(ref branch) = args.branch {
                cmd.arg("--branch");
                cmd.arg(branch);
            }
            cmd.arg(dir_name);
            cmd.current_dir(dir_parent).check("git clone").await?;
        }
        Repository::open(dir)?.head()?.peel_to_commit()?.id()
    };
    let setup = if args.rsl {
        RandoSetup::Rsl {
            github_user: args.github_user,
        }
    } else {
        RandoSetup::Normal {
            github_user: args.github_user,
            settings: if let Some(preset) = args.preset {
                RandoSettings::Preset(preset)
            } else if let Some(settings) = args.settings {
                RandoSettings::String(settings)
            } else {
                RandoSettings::Default
            },
            json_settings: args.json_settings,
            world_counts: args.world_counts,
        }
    };
    let stats_dir = {
        let stats_root = if let Some(stats_dir) = config.stats_dir.take() {
            stats_dir
        } else {
            #[cfg(windows)] let project_dirs = ProjectDirs::from("net", "Fenhl", "ootrstats").ok_or(Error::MissingHomeDir)?;
            #[cfg(windows)] { project_dirs.data_dir().to_owned() }
            #[cfg(unix)] { BaseDirectories::new()?.place_data_file("ootrstats").at_unknown()? }
        };
        stats_root.join(setup.stats_dir(rando_rev))
    };
    let available_parallelism = std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN).get().try_into().unwrap_or(SeedIdx::MAX).min(args.num_seeds);
    let is_bench = matches!(args.subcommand, Some(Subcommand::Bench { .. }));
    let start = Instant::now();
    let start_local = Local::now();
    let mut subcommand_data = match args.subcommand {
        None => SubcommandData::None {
            num_successes: 0,
            num_failures: 0,
        },
        Some(Subcommand::Bench { raw_data }) => SubcommandData::Bench {
            instructions_success: Vec::with_capacity(args.num_seeds.into()),
            instructions_failure: Vec::with_capacity(args.num_seeds.into()),
            raw_data,
        },
        Some(Subcommand::MidosHouse { out_path }) => SubcommandData::MidosHouse {
            spoiler_logs: Vec::with_capacity(args.num_seeds.into()),
            num_failures: 0,
            out_path,
        },
    };
    let (reader_tx, mut reader_rx) = mpsc::channel(args.num_seeds.min(256).into());
    let mut readers = (0..available_parallelism).map(|task_idx| {
        let stats_dir = stats_dir.clone();
        let reader_tx = reader_tx.clone();
        tokio::spawn(async move {
            for seed_idx in (task_idx..args.num_seeds).step_by(available_parallelism.into()) {
                let seed_path = stats_dir.join(seed_idx.to_string());
                let stats_spoiler_log_path = seed_path.join("spoiler.json");
                let stats_error_log_path = seed_path.join("error.log");
                match (fs::exists(&stats_spoiler_log_path).await?, fs::exists(&stats_error_log_path).await?) {
                    (false, false) => reader_tx.send(ReaderMessage::Pending(seed_idx)).await?,
                    (false, true) => {
                        let instructions = if is_bench {
                            match fs::read_json::<Metadata>(seed_path.join("metadata.json")).await {
                                Ok(metadata) => metadata.instructions,
                                Err(wheel::Error::Io { inner, .. }) if inner.kind() == io::ErrorKind::NotFound => None,
                                Err(e) => return Err(e.into()),
                            }
                        } else {
                            None
                        };
                        reader_tx.send(ReaderMessage::Failure { seed_idx, instructions }).await?;
                    }
                    (true, false) => {
                        let instructions = if is_bench {
                            match fs::read_json::<Metadata>(seed_path.join("metadata.json")).await {
                                Ok(metadata) => metadata.instructions,
                                Err(wheel::Error::Io { inner, .. }) if inner.kind() == io::ErrorKind::NotFound => None,
                                Err(e) => return Err(e.into()),
                            }
                        } else {
                            None
                        };
                        reader_tx.send(ReaderMessage::Success { seed_idx, instructions }).await?;
                    }
                    (true, true) => return Err(Error::SuccessAndFailure),
                }
            }
            reader_tx.send(ReaderMessage::Done).await?;
            Ok(())
        })
    }).collect::<FuturesUnordered<_>>();
    drop(reader_tx);
    let mut completed_readers = 0;
    let (worker_tx, mut worker_rx) = mpsc::channel(256);
    let mut worker_tasks = FuturesUnordered::default();
    let mut workers = Err::<Vec<worker::State>, _>(worker_tx);
    let mut pending_seeds = VecDeque::default();
    loop {
        enum Event {
            ReaderDone(Result<Result<(), Error>, JoinError>),
            ReaderMessage(ReaderMessage),
            WorkerDone(String, Result<Result<(), worker::Error>, JoinError>),
            WorkerMessage(String, ootrstats::worker::Message),
            End,
        }

        select! {
            event = async {
                select! {
                    Some(res) = readers.next() => Event::ReaderDone(res),
                    Some(msg) = reader_rx.recv() => Event::ReaderMessage(msg),
                    Some((name, res)) = worker_tasks.next() => Event::WorkerDone(name, res),
                    Some((name, msg)) = worker_rx.recv() => Event::WorkerMessage(name, msg),
                    else => Event::End,
                }
            } => {
                let seed_idx = match event {
                    Event::ReaderDone(res) => {
                        let () = res??;
                        None
                    }
                    Event::ReaderMessage(msg) => match msg {
                        ReaderMessage::Pending(seed_idx) => Some(seed_idx),
                        ReaderMessage::Success { seed_idx, instructions } => match subcommand_data {
                            SubcommandData::None { ref mut num_successes, .. } => {
                                *num_successes += 1;
                                None
                            }
                            SubcommandData::Bench { ref mut instructions_success, .. } => if let Some(instructions) = instructions {
                                instructions_success.push(instructions);
                                None
                            } else {
                                // seed was already rolled but not benchmarked, roll a new seed instead
                                fs::remove_dir_all(stats_dir.join(seed_idx.to_string())).await?;
                                Some(seed_idx)
                            },
                            SubcommandData::MidosHouse { ref mut spoiler_logs, .. } => {
                                spoiler_logs.push(fs::read_json(stats_dir.join(seed_idx.to_string()).join("spoiler.json")).await?);
                                None
                            }
                        },
                        ReaderMessage::Failure { seed_idx, instructions } => if args.retry_failures {
                            fs::remove_dir_all(stats_dir.join(seed_idx.to_string())).await?;
                            Some(seed_idx)
                        } else {
                            match subcommand_data {
                                SubcommandData::None { ref mut num_failures, .. } | SubcommandData::MidosHouse { ref mut num_failures, .. } => {
                                    *num_failures += 1;
                                    None
                                }
                                SubcommandData::Bench { ref mut instructions_failure, .. } => if let Some(instructions) = instructions {
                                    instructions_failure.push(instructions);
                                    None
                                } else {
                                    // seed was already rolled but not benchmarked, roll a new seed instead
                                    fs::remove_dir_all(stats_dir.join(seed_idx.to_string())).await?;
                                    Some(seed_idx)
                                },
                            }
                        },
                        ReaderMessage::Done => {
                            completed_readers += 1;
                            None
                        }
                    },
                    Event::WorkerDone(name, result) => {
                        let () = result?.map_err(|source| Error::Worker { worker: name.clone(), source })?;
                        if_chain! {
                            if let Ok(ref mut workers) = workers;
                            if let Some(worker) = workers.iter_mut().find(|worker| worker.name == name);
                            then {
                                worker.stopped = true;
                            } else {
                                return Err(Error::WorkerNotFound)
                            }
                        }
                        None
                    }
                    Event::WorkerMessage(name, msg) => if_chain! {
                        if let Ok(ref mut workers) = workers;
                        if let Some(worker) = workers.iter_mut().find(|worker| worker.name == name);
                        then {
                            match msg {
                                ootrstats::worker::Message::Init(msg) => {
                                    worker.msg = Some(msg);
                                    None
                                }
                                ootrstats::worker::Message::Ready(ready) => {
                                    worker.ready += ready;
                                    while worker.ready > 0 {
                                        worker.msg = None;
                                        let Some(seed_idx) = pending_seeds.pop_front() else { break };
                                        worker.roll(seed_idx).await?;
                                    }
                                    None
                                }
                                ootrstats::worker::Message::Success { seed_idx, instructions, spoiler_log, patch } => {
                                    worker.running -= 1;
                                    worker.completed += 1;
                                    let seed_dir = stats_dir.join(seed_idx.to_string());
                                    fs::create_dir_all(&seed_dir).await?;
                                    let stats_spoiler_log_path = seed_dir.join("spoiler.json");
                                    match spoiler_log {
                                        Either::Left(ref spoiler_log_path) => {
                                            let is_same_drive = {
                                                #[cfg(windows)] {
                                                    spoiler_log_path.components().find_map(|component| if let std::path::Component::Prefix(prefix) = component { Some(prefix) } else { None })
                                                    == stats_spoiler_log_path.components().find_map(|component| if let std::path::Component::Prefix(prefix) = component { Some(prefix) } else { None })
                                                }
                                                #[cfg(not(windows))] { true }
                                            };
                                            if is_same_drive {
                                                fs::rename(spoiler_log_path, stats_spoiler_log_path).await?;
                                            } else {
                                                fs::copy(spoiler_log_path, stats_spoiler_log_path).await?;
                                                fs::remove_file(spoiler_log_path).await?;
                                            }
                                        }
                                        Either::Right(ref spoiler_log) => fs::write(stats_spoiler_log_path, spoiler_log).await?,
                                    }
                                    if let Some(patch) = patch {
                                        match patch {
                                            Either::Left((wsl, patch_path)) => {
                                                let mut patch_filename = OsString::from("patch.");
                                                if let Some(ext) = patch_path.extension() {
                                                    patch_filename.push(ext);
                                                }
                                                let stats_patch_path = seed_dir.join(patch_filename);
                                                if wsl {
                                                    let patch = Command::new(WSL).arg("cat").arg(&patch_path).check("wsl cat").await?.stdout;
                                                    fs::write(stats_patch_path, patch).await?;
                                                    Command::new(WSL).arg("rm").arg(patch_path).check("wsl rm").await?;
                                                } else {
                                                    let is_same_drive = {
                                                        #[cfg(windows)] {
                                                            patch_path.components().find_map(|component| if let std::path::Component::Prefix(prefix) = component { Some(prefix) } else { None })
                                                            == stats_patch_path.components().find_map(|component| if let std::path::Component::Prefix(prefix) = component { Some(prefix) } else { None })
                                                        }
                                                        #[cfg(not(windows))] { true }
                                                    };
                                                    if is_same_drive {
                                                        fs::rename(patch_path, stats_patch_path).await?;
                                                    } else {
                                                        fs::copy(&patch_path, stats_patch_path).await?;
                                                        fs::remove_file(patch_path).await?;
                                                    }
                                                }
                                            }
                                            Either::Right((ext, patch)) => {
                                                let stats_patch_path = seed_dir.join(format!("patch.{ext}"));
                                                fs::write(stats_patch_path, patch).await?;
                                            }
                                        }
                                    }
                                    fs::write(seed_dir.join("metadata.json"), serde_json::to_vec_pretty(&Metadata {
                                        instructions,
                                    })?).await?;
                                    match subcommand_data {
                                        SubcommandData::None { ref mut num_successes, .. } => {
                                            *num_successes += 1;
                                            None
                                        }
                                        SubcommandData::Bench { ref mut instructions_success, .. } => if let Some(instructions) = instructions {
                                            instructions_success.push(instructions);
                                            None
                                        } else {
                                            // perf sometimes doesn't output instruction count for whatever reason, retry if this happens
                                            fs::remove_dir_all(seed_dir).await?;
                                            Some(seed_idx)
                                        },
                                        SubcommandData::MidosHouse { ref mut spoiler_logs, .. } => {
                                            spoiler_logs.push(match spoiler_log {
                                                Either::Left(_) => fs::read_json(stats_dir.join(seed_idx.to_string()).join("spoiler.json")).await?,
                                                Either::Right(spoiler_log) => serde_json::from_slice(&spoiler_log)?,
                                            });
                                            None
                                        }
                                    }
                                }
                                ootrstats::worker::Message::Failure { seed_idx, instructions, error_log } => {
                                    worker.running -= 1;
                                    worker.completed += 1;
                                    let seed_dir = stats_dir.join(seed_idx.to_string());
                                    if args.retry_failures {
                                        fs::remove_dir_all(seed_dir).await.missing_ok()?;
                                        Some(seed_idx)
                                    } else {
                                        fs::create_dir_all(&seed_dir).await?;
                                        let stats_error_log_path = seed_dir.join("error.log");
                                        fs::write(stats_error_log_path, &error_log).await?;
                                        fs::write(seed_dir.join("metadata.json"), serde_json::to_vec_pretty(&Metadata {
                                            instructions,
                                        })?).await?;
                                        match subcommand_data {
                                            SubcommandData::None { ref mut num_failures, .. } | SubcommandData::MidosHouse { ref mut num_failures, .. } => {
                                                *num_failures += 1;
                                                None
                                            }
                                            SubcommandData::Bench { ref mut instructions_failure, .. } => if let Some(instructions) = instructions {
                                                instructions_failure.push(instructions);
                                                None
                                            } else {
                                                // perf sometimes doesn't output instruction count for whatever reason, retry if this happens
                                                fs::remove_dir_all(seed_dir).await?;
                                                Some(seed_idx)
                                            },
                                        }
                                    }
                                }
                            }
                        } else {
                            return Err(Error::WorkerNotFound)
                        }
                    },
                    Event::End => break,
                };
                if let Some(seed_idx) = seed_idx {
                    let workers = match workers {
                        Ok(ref mut workers) => workers,
                        Err(worker_tx) => {
                            let (new_worker_tasks, new_workers) = mem::take(&mut config.workers).into_iter()
                                .filter(|&worker::Config { bench, .. }| bench || !is_bench)
                                .map(|worker::Config { name, kind, .. }| {
                                    let (task, state) = worker::State::new(worker_tx.clone(), name.clone(), kind, rando_rev, &setup, match (is_bench, args.patch) {
                                        (false, false) => OutputMode::Normal,
                                        (false, true) => OutputMode::Patch,
                                        (true, false) => OutputMode::Bench,
                                        (true, true) => unimplemented!("The `bench` subcommand currently cannot generate patch files"),
                                    });
                                    (task.map(move |res| (name, res)), state)
                                })
                                .unzip::<_, _, _, Vec<_>>();
                            worker_tasks = new_worker_tasks;
                            workers = Ok(new_workers);
                            workers.as_mut().ok().expect("just inserted")
                        }
                    };
                    if let Some(worker) = workers.iter_mut().find(|worker| worker.ready > 0) {
                        worker.roll(seed_idx).await?;
                    } else {
                        pending_seeds.push_back(seed_idx);
                    }
                }
            },
            //TODO use signal-hook-tokio crate to handle interrupts on Unix?
            Some(res) = cli_rx.recv() => if let crossterm::event::Event::Key(KeyEvent { code: KeyCode::Char('c' | 'd'), modifiers, kind: KeyEventKind::Release, .. }) = res.at_unknown()? {
                if modifiers.contains(KeyModifiers::CONTROL) {
                    // finish rolling seeds that are already in progress but don't start any more
                    readers.clear();
                    completed_readers = available_parallelism;
                    reader_rx = mpsc::channel(1).1;
                    pending_seeds.clear();
                }
            },
        }
        if let Ok(ref workers) = workers {
            for worker in workers {
                if worker.stopped {
                    crossterm::execute!(stderr,
                        Print(format_args!("\r\n{}: done", worker.name)),
                        Clear(ClearType::UntilNewLine),
                    ).at_unknown()?;
                } else if let Some(ref msg) = worker.msg {
                    crossterm::execute!(stderr,
                        Print(format_args!("\r\n{}: {}", worker.name, msg)),
                        Clear(ClearType::UntilNewLine),
                    ).at_unknown()?;
                } else {
                    let total = workers.iter().map(|worker| worker.completed).sum::<u16>();
                    if total > 0 {
                        crossterm::execute!(stderr,
                            Print(format_args!(
                                "\r\n{}: {} rolled ({}%), {} running",
                                worker.name,
                                worker.completed,
                                100 * u32::from(worker.completed) / u32::from(total),
                                worker.running,
                            )),
                            Clear(ClearType::UntilNewLine),
                        ).at_unknown()?;
                    } else {
                        crossterm::execute!(stderr,
                            Print(format_args!(
                                "\r\n{}: 0 rolled, {} running",
                                worker.name,
                                worker.running,
                            )),
                            Clear(ClearType::UntilNewLine),
                        ).at_unknown()?;
                    }
                }
            }
            crossterm::execute!(stderr,
                MoveUp(workers.len() as u16),
            ).at_unknown()?;
        }
        crossterm::execute!(stderr,
            MoveToColumn(0),
            Print(if completed_readers == available_parallelism {
                // list of pending seeds fully initialized
                match subcommand_data {
                    SubcommandData::None { num_successes, num_failures } => {
                        let rolled = num_successes + num_failures;
                        let started = rolled + workers.as_ref().map(|workers| workers.iter().map(|worker| u16::from(worker.running)).sum::<u16>()).unwrap_or_default();
                        let total = usize::from(started) + pending_seeds.len();
                        let completed = match workers {
                            Ok(ref workers) => workers.iter().map(|worker| worker.completed).sum(),
                            Err(_) => 0,
                        };
                        let skipped = usize::from(rolled - completed);
                        format!(
                            "{started}/{total} seeds started, {rolled} rolled, {num_failures} failures ({}%), ETA {}",
                            if num_successes > 0 || num_failures > 0 { 100 * num_failures / (num_successes + num_failures) } else { 100 },
                            if completed > 0 { (start_local + TimeDelta::from_std(start.elapsed().mul_f64((total - skipped) as f64 / completed as f64)).expect("ETA too long")).format("%Y-%m-%d %H:%M:%S").to_string() } else { format!("unknown") },
                        )
                    }
                    SubcommandData::Bench { ref instructions_success, ref instructions_failure, .. } => {
                        let rolled = instructions_success.len() + instructions_failure.len();
                        let started = rolled + workers.as_ref().map(|workers| workers.iter().map(|worker| usize::from(worker.running)).sum::<usize>()).unwrap_or_default();
                        let total = started + pending_seeds.len();
                        let completed = match workers {
                            Ok(ref workers) => workers.iter().map(|worker| worker.completed).sum(),
                            Err(_) => 0,
                        };
                        let skipped = rolled - usize::from(completed);
                        format!(
                            "{started}/{total} seeds started, {rolled} rolled, {} failures ({}%), ETA {}",
                            instructions_failure.len(),
                            if !instructions_success.is_empty() || !instructions_failure.is_empty() { 100 * instructions_failure.len() / (instructions_success.len() + instructions_failure.len()) } else { 100 },
                            if completed > 0 { (start_local + TimeDelta::from_std(start.elapsed().mul_f64((total - skipped) as f64 / completed as f64)).expect("ETA too long")).format("%Y-%m-%d %H:%M:%S").to_string() } else { format!("unknown") },
                        )
                    }
                    SubcommandData::MidosHouse { ref spoiler_logs, num_failures, .. } => {
                        let rolled = spoiler_logs.len() + usize::from(num_failures);
                        let started = rolled + workers.as_ref().map(|workers| workers.iter().map(|worker| usize::from(worker.running)).sum::<usize>()).unwrap_or_default();
                        let total = started + pending_seeds.len();
                        let completed = match workers {
                            Ok(ref workers) => workers.iter().map(|worker| worker.completed).sum(),
                            Err(_) => 0,
                        };
                        let skipped = rolled - usize::from(completed);
                        format!(
                            "{started}/{total} seeds started, {rolled} rolled, {num_failures} failures ({}%), ETA {}",
                            if !spoiler_logs.is_empty() || num_failures > 0 { 100 * usize::from(num_failures) / (spoiler_logs.len() + usize::from(num_failures)) } else { 100 },
                            if completed > 0 { (start_local + TimeDelta::from_std(start.elapsed().mul_f64((total - skipped) as f64 / completed as f64)).expect("ETA too long")).format("%Y-%m-%d %H:%M:%S").to_string() } else { format!("unknown") },
                        )
                    }
                }
            } else {
                match subcommand_data {
                    SubcommandData::None { num_successes, num_failures } => {
                        let rolled = usize::from(num_successes + num_failures);
                        let started = workers.as_ref().map(|workers| workers.iter().map(|worker| usize::from(worker.running)).sum::<usize>()).unwrap_or_default();
                        format!(
                            "checking for existing seeds: {rolled} rolled, {started} running, {} pending, {} still being checked",
                            pending_seeds.len(),
                            usize::from(args.num_seeds) - pending_seeds.len() - started - rolled,
                        )
                    }
                    SubcommandData::Bench { ref instructions_success, ref instructions_failure, .. } => {
                        let rolled = instructions_success.len() + instructions_failure.len();
                        let started = workers.as_ref().map(|workers| workers.iter().map(|worker| usize::from(worker.running)).sum::<usize>()).unwrap_or_default();
                        format!(
                            "checking for existing seeds: {rolled} rolled, {started} running, {} pending, {} still being checked",
                            pending_seeds.len(),
                            usize::from(args.num_seeds) - pending_seeds.len() - started - rolled,
                        )
                    }
                    SubcommandData::MidosHouse { ref spoiler_logs, num_failures, .. } => {
                        let rolled = spoiler_logs.len() + usize::from(num_failures);
                        let started = workers.as_ref().map(|workers| workers.iter().map(|worker| usize::from(worker.running)).sum::<usize>()).unwrap_or_default();
                        format!(
                            "checking for existing seeds: {rolled} rolled, {started} running, {} pending, {} still being checked",
                            pending_seeds.len(),
                            usize::from(args.num_seeds) - pending_seeds.len() - started - rolled,
                        )
                    }
                }
            }),
            Clear(ClearType::UntilNewLine),
        ).at_unknown()?;
        if pending_seeds.is_empty() && completed_readers == available_parallelism {
            if let Ok(ref mut workers) = workers {
                for worker in workers {
                    // drop sender so the worker can shut down
                    worker.supervisor_tx = mpsc::channel(1).0;
                }
            } else if worker_tasks.is_empty() {
                // make sure worker_tx is dropped to prevent deadlock
                workers = Ok(Vec::default());
            }
        }
    }
    drop(cli_rx);
    if let Ok(ref workers) = workers {
        crossterm::execute!(stderr,
            MoveDown(workers.len() as u16),
        ).at_unknown()?;
    }
    crossterm::execute!(stderr,
        Print("\r\n"),
    ).at_unknown()?;
    match subcommand_data {
        SubcommandData::None { .. } => crossterm::execute!(stderr,
            Print(format_args!("stats saved to {}\r\n", stats_dir.display())),
        ).at_unknown()?,
        SubcommandData::Bench { instructions_success, instructions_failure, raw_data } => if raw_data {
            for instructions in instructions_success {
                println!("s {instructions}");
            }
            for instructions in instructions_failure {
                println!("f {instructions}");
            }
        } else {
            if instructions_success.is_empty() {
                crossterm::execute!(stderr,
                    Print("No successful seeds, so average instruction count is infinite\r\n"),
                ).at_unknown()?;
            } else {
                let success_rate = instructions_success.len() as f64 / (instructions_success.len() as f64 + instructions_failure.len() as f64);
                let average_instructions_success = instructions_success.iter().sum::<u64>() / u64::try_from(instructions_success.len()).unwrap();
                let average_instructions_failure = instructions_failure.iter().sum::<u64>().checked_div(u64::try_from(instructions_failure.len()).unwrap()).unwrap_or_default();
                let average_failure_count = (1.0 - success_rate) / success_rate; // mean of 0-support geometric distribution
                let average_instructions = average_failure_count * average_instructions_failure as f64 + average_instructions_success as f64;
                crossterm::execute!(stderr,
                    Print(format_args!("success rate: {}/{} ({:.02}%)\r\n", instructions_success.len(), instructions_success.len() + instructions_failure.len(), success_rate * 100.0)),
                    Print(format_args!("average instructions (success): {average_instructions_success} ({average_instructions_success:.3e})\r\n")),
                    Print(format_args!("average instructions (failure): {}\r\n", if instructions_failure.is_empty() { format!("N/A") } else { format!("{average_instructions_failure} ({average_instructions_failure:.3e})") })),
                    Print(format_args!("average total instructions until success: {average_instructions} ({average_instructions:.3e})\r\n")),
                ).at_unknown()?;
            }
        },
        SubcommandData::MidosHouse { out_path, spoiler_logs, .. } => {
            let mut counts = HashMap::<_, usize>::default();
            for spoiler_log in spoiler_logs {
                for appearances in spoiler_log.midos_house_chests() {
                    *counts.entry(appearances).or_default() += 1;
                }
            }
            let mut counts = counts.into_iter().collect_vec();
            counts.sort_unstable();
            let mut buf = serde_json::to_vec_pretty(&counts)?;
            buf.push(b'\n');
            fs::write(out_path, buf).await?;
        }
    }
    Ok(())
}

#[wheel::main(custom_exit)]
async fn main(args: Args) -> Result<(), Error> {
    enable_raw_mode().at_unknown()?;
    let res = if args.suite {
        cli(args.clone())
            .and_then(|()| cli(Args { preset: Some(format!("tournament")), ..args.clone() }))
            .and_then(|()| cli(Args { preset: Some(format!("mw")), ..args.clone() }))
            .and_then(|()| cli(Args { preset: Some(format!("hell")), ..args.clone() }))
            .and_then(|()| cli(Args { rsl: true, github_user: format!("fenhl"), branch: Some(format!("dev-mvp")), ..args.clone() })) //TODO check to make sure plando-random-settings branch is up to date with matthewkirby:master and the randomizer commit specified in rslversion.py is equal to the specified randomizer commit
            .await
    } else {
        cli(args).await
    };
    disable_raw_mode().at_unknown()?;
    res
}
