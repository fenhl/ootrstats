use {
    std::{
        collections::{
            BTreeMap,
            hash_map::{
                self,
                HashMap,
            },
        },
        ffi::OsString,
        io::{
            IsTerminal as _,
            stderr,
            stdout,
        },
        iter,
        mem,
        num::NonZeroUsize,
        path::PathBuf,
        sync::Arc,
    },
    bytes::Bytes,
    chrono::prelude::*,
    crossterm::{
        cursor::MoveDown,
        event::{
            KeyCode,
            KeyEvent,
            KeyEventKind,
        },
        style::Print,
        terminal::{
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
    if_chain::if_chain,
    itertools::Itertools as _,
    lazy_regex::regex_is_match,
    nonempty_collections::NEVec,
    ootr_utils::spoiler::SpoilerLog,
    proc_macro2 as _, // feature config required for Span::start used in CustomExit impl
    rustls as _, // feature ring required for WebSocket connections to work
    serde::{
        Deserialize,
        Serialize,
    },
    tokio::{
        io::{
            self,
            AsyncWriteExt as _,
        },
        process::Command,
        select,
        sync::mpsc,
        task::JoinError,
        time::Instant,
    },
    wheel::{
        fs::{
            self,
            File,
        },
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
        gitdir,
    },
    crate::{
        config::Config,
        msg::Message,
    },
};
#[cfg(windows)] use directories::ProjectDirs;
#[cfg(unix)] use xdg::BaseDirectories;

mod config;
mod msg;
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
    /// present if the `bench` parameter was set.
    instructions: Option<Result<u64, String>>,
    /// always written by this version of ootrstats but may be absent in metadata from older ootrstats versions.
    worker: Option<Arc<str>>,
}

#[derive(Serialize)]
enum SeedState {
    Unchecked,
    Pending,
    Rolling {
        workers: NEVec<Arc<str>>,
    },
    Cancelled,
    Success {
        /// `None` means the seed was read from disk.
        worker: Option<Arc<str>>,
        instructions: Option<u64>,
        spoiler_log: serde_json::Value,
    },
    Failure {
        /// `None` means the seed was read from disk.
        worker: Option<Arc<str>>,
        instructions: Option<u64>,
        error_log: Bytes,
    },
}

fn parse_json_object(arg: &str) -> Result<serde_json::Map<String, serde_json::Value>, serde_json::Error> {
    serde_json::from_str(arg)
}

#[derive(Clone, clap::Parser)]
#[clap(version)]
struct Args {
    // randomizer settings

    /// Run the benchmarking suite.
    #[clap(long, conflicts_with("rsl"), conflicts_with("preset"), conflicts_with("settings"))]
    suite: bool,
    /// Use the random settings script to determine settings.
    #[clap(long)]
    rsl: bool,
    #[clap(short = 'u', long, default_value = "OoTRandomizer", default_value_if("rsl", "true", Some("matthewkirby")))]
    github_user: String,
    #[clap(long, default_value = "OoT-Randomizer", default_value_if("rsl", "true", Some("plando-random-settings")))]
    repo: String,
    #[clap(short, long, conflicts_with("rev"))]
    branch: Option<String>,
    #[clap(long)]
    rev: Option<gix::ObjectId>,
    #[clap(short, long, conflicts_with("rsl"))]
    preset: Option<String>,
    /// Settings string for the randomizer.
    #[clap(long, conflicts_with("rsl"), conflicts_with("preset"))]
    settings: Option<String>,
    /// Simulates a settings draft from the given file.
    #[clap(long, conflicts_with("rsl"), conflicts_with("preset"), conflicts_with("settings"))]
    draft: Option<PathBuf>,
    /// Specifies a JSON object of settings on the command line that will override the given preset or settings string.
    #[clap(long, conflicts_with("rsl"), default_value = "{}", value_parser = parse_json_object)]
    json_settings: serde_json::Map<String, serde_json::Value>,
    /// Generate seeds with varying world counts.
    #[clap(long, conflicts_with("rsl"))]
    world_counts: bool,
    /// Generate .zpf/.zpfz patch files.
    #[clap(long, conflicts_with("rsl"))]
    patch: bool,

    // ootrstats settings

    /// Sample size — how many seeds to roll.
    #[clap(short, long, default_value = "16384", default_value_if("world_counts", "true", Some("255")))]
    num_seeds: SeedIdx,
    /// If there are more available cores than remaining seeds, roll the same seed multiple times and keep the one that finishes first.
    #[clap(long)]
    race: bool,
    /// If the randomizer errors, retry instead of recording the failure.
    #[clap(long)]
    retry_failures: bool,
    /// Delete any existing stats instead of reusing them.
    #[clap(long)]
    clean: bool,
    /// Only roll seeds on the given worker(s).
    #[clap(short = 'w', long = "worker", conflicts_with("exclude_workers"))]
    include_workers: Vec<Arc<str>>,
    /// Don't roll seeds on the given worker(s).
    #[clap(short = 'x', long = "exclude-worker", conflicts_with("include_workers"))]
    exclude_workers: Vec<Arc<str>>,
    /// Print status updates as machine-readable JSON.
    #[clap(long)]
    json_messages: bool,
    #[clap(subcommand)]
    subcommand: Option<Subcommand>,
}

#[derive(Clone, clap::Subcommand)]
enum Subcommand {
    /// Benchmark — measure average CPU instructions to generate a seed.
    Bench {
        #[clap(long)]
        raw_data: bool,
        #[clap(long)]
        uncompressed: bool,
    },
    /// Categorize spoiler logs using a JSON query.
    Categorize {
        query: String,
    },
    /// Display most common exceptions thrown by the randomizer.
    Failures,
    /// Count chest appearances in Mido's house for the midos.house favicon.
    MidosHouse {
        out_path: PathBuf,
    },
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)] Config(#[from] config::Error),
    #[error(transparent)] GitHeadId(#[from] gix::reference::head_id::Error),
    #[error(transparent)] GitOpen(#[from] gix::open::Error),
    #[error(transparent)] Json(#[from] serde_json::Error),
    #[error(transparent)] Task(#[from] JoinError),
    #[error(transparent)] TryFromInt(#[from] std::num::TryFromIntError),
    #[error(transparent)] ReaderSend(#[from] mpsc::error::SendError<ReaderMessage>),
    #[error(transparent)] Utf8(#[from] std::str::Utf8Error),
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[cfg(unix)] #[error(transparent)] Xdg(#[from] xdg::BaseDirectoriesError),
    #[error("error parsing draft spec: {source}")]
    DraftParse {
        file: String,
        source: syn::Error,
    },
    #[error("empty error log")]
    EmptyErrorLog,
    #[error("failed to parse JSON query")]
    Jaq,
    #[cfg(windows)]
    #[error("user folder not found")]
    MissingHomeDir,
    #[error("missing traceback from error log")]
    MissingTraceback,
    #[error("found both spoiler and error logs for a seed")]
    SuccessAndFailure,
    #[error("at most 255 seeds may be generated with the --world-counts option")]
    TooManyWorlds,
    #[error("error(s) in worker(s): {}", .0.iter().map(|(worker, source)| format!("{worker}: {source}")).format(", "))]
    Worker(Vec<(Arc<str>, worker::Error)>),
    #[error("received a message from an unknown worker")]
    WorkerNotFound,
}

impl wheel::CustomExit for Error {
    fn exit(self, cmd_name: &'static str) -> ! {
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
        eprintln!("\r");
        match self {
            Self::DraftParse { file: _ /*TODO display the span of code? */, source } => {
                eprintln!("{cmd_name}: error parsing draft spec: {source}\r");
                let start = source.span().start();
                eprintln!("line {}, column {}\r", start.line, start.column);
                eprintln!("debug info: {debug}\r");
            }
            Self::Worker(errors) => match errors.into_iter().exactly_one() {
                Ok((worker, worker::Error::Local(ootrstats::worker::Error::Roll(ootrstats::RollError::PerfSyntax(stderr))))) => {
                    eprintln!("{cmd_name}: roll error in worker {worker}: failed to parse `perf` output\r");
                    eprintln!("stderr:\r");
                    eprintln!("{}\r", String::from_utf8_lossy(&stderr).lines().filter(|line| !regex_is_match!("^[0-9]+ files remaining$", line)).format("\r\n"));
                }
                Ok((worker, source)) => {
                    eprintln!("{cmd_name}: error in worker {worker}: {source}\r");
                    eprintln!("debug info: {debug}\r");
                }
                Err(errors) => {
                    eprintln!("{cmd_name}: errors in workers:\r");
                    for (worker, source) in errors {
                        eprintln!("\r");
                        eprintln!("in worker {worker}: {source}\r");
                    }
                    eprintln!("\r");
                    eprintln!("debug info: {debug}\r");
                }
            },
            _ => {
                eprintln!("{cmd_name}: {self}\r");
                eprintln!("debug info: {debug}\r");
            }
        }
        std::process::exit(1)
    }
}

async fn cli(mut args: Args) -> Result<(), Error> {
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
    let mut stdout = stdout();
    let mut stderr = stderr();
    Message::Preparing.print(args.json_messages, &mut stderr)?;
    let mut config = Config::load().await?;
    let mut log_file = if config.log {
        Some(File::create("ootrstats.log").await?)
    } else {
        None
    };

    macro_rules! log {
        ($($fmt:tt)*) => {{
            if let Some(ref mut log_file) = log_file {
                log_file.write_all(Local::now().format("%Y-%m-%d %H:%M:%S ").to_string().as_bytes()).await.at("ootrstats.log")?;
                log_file.write_all(format!($($fmt)*).as_bytes()).await.at("ootrstats.log")?;
                log_file.write_all(b"\n").await.at("ootrstats.log")?;
                log_file.flush().await.at("ootrstats.log")?;
            }
        }};
    }

    let rando_rev = if let Some(rev) = args.rev {
        rev
    } else {
        let mut dir_parent = gitdir().await?.join("github.com").join(&args.github_user).join(&args.repo);
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
            cmd.arg(format!("https://github.com/{}/{}.git", args.github_user, args.repo));
            if let Some(ref branch) = args.branch {
                cmd.arg("--branch");
                cmd.arg(branch);
            }
            cmd.arg(dir_name);
            cmd.current_dir(dir_parent).check("git clone").await?;
        }
        gix::open(dir)?.head_id()?.detach()
    };
    let setup = if args.rsl {
        RandoSetup::Rsl {
            github_user: args.github_user,
            repo: args.repo,
        }
    } else {
        RandoSetup::Normal {
            github_user: args.github_user,
            repo: args.repo,
            settings: if let Some(preset) = args.preset {
                RandoSettings::Preset(preset)
            } else if let Some(settings) = args.settings {
                RandoSettings::String(settings)
            } else if let Some(file) = args.draft {
                let file = fs::read_to_string(file).await?;
                RandoSettings::Draft(syn::parse_str(&file).map_err(|source| Error::DraftParse { file, source })?)
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
    if args.clean {
        fs::remove_dir_all(&stats_dir).await.missing_ok()?;
    }
    let available_parallelism = std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN).get().try_into().unwrap_or(SeedIdx::MAX).min(args.num_seeds);
    let is_bench = matches!(args.subcommand, Some(Subcommand::Bench { .. }));
    let start = Instant::now();
    let start_local = Local::now();
    let mut seed_states = Vec::from_iter(iter::repeat_with(|| SeedState::Unchecked).take(args.num_seeds.into()));
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
                                Ok(metadata) => metadata.instructions.and_then(Result::ok),
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
                                Ok(metadata) => metadata.instructions.and_then(Result::ok),
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
    let mut cancelled = false;

    macro_rules! cancel {
        ($workers:expr) => {{
            // finish rolling seeds that are already in progress but don't start any more
            cancelled = true;
            args.retry_failures = false;
            readers.clear();
            completed_readers = available_parallelism;
            reader_rx = mpsc::channel(1).1;
            for (seed_idx, seed_state) in seed_states.iter_mut().enumerate() {
                match seed_state {
                    SeedState::Unchecked | SeedState::Pending => *seed_state = SeedState::Cancelled,
                    SeedState::Rolling { workers: worker_names } => if args.race {
                        // --race means the user is okay with randomizer instances being cancelled, so we do that here to speed up the exit
                        for name in worker_names.iter() {
                            if let Some(worker) = $workers.iter().find(|worker| worker.name == *name) {
                                let _ = worker.supervisor_tx.send(ootrstats::worker::SupervisorMessage::Cancel(seed_idx.try_into()?)).await;
                            } else {
                                return Err(Error::WorkerNotFound)
                            }
                        }
                        *seed_state = SeedState::Cancelled;
                    },
                    SeedState::Cancelled | SeedState::Success { .. } | SeedState::Failure { .. } => {}
                }
            }
        }};
    }

    loop {
        enum Event {
            ReaderDone(Result<Result<(), Error>, JoinError>),
            ReaderMessage(ReaderMessage),
            WorkerDone(Arc<str>, Result<Result<(), worker::Error>, JoinError>),
            WorkerMessage(Arc<str>, ootrstats::worker::Message),
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
                        ReaderMessage::Pending(seed_idx) => {
                            seed_states[usize::from(seed_idx)] = SeedState::Pending;
                            Some(seed_idx)
                        }
                        ReaderMessage::Success { seed_idx, instructions } => if is_bench && instructions.is_none() {
                            // seed was already rolled but not benchmarked, roll a new seed instead
                            fs::remove_dir_all(stats_dir.join(seed_idx.to_string())).await?;
                            seed_states[usize::from(seed_idx)] = SeedState::Pending;
                            Some(seed_idx)
                        } else {
                            seed_states[usize::from(seed_idx)] = SeedState::Success {
                                worker: None,
                                spoiler_log: fs::read_json(stats_dir.join(seed_idx.to_string()).join("spoiler.json")).await?,
                                instructions,
                            };
                            None
                        },
                        ReaderMessage::Failure { seed_idx, instructions } => if args.retry_failures {
                            fs::remove_dir_all(stats_dir.join(seed_idx.to_string())).await?;
                            seed_states[usize::from(seed_idx)] = SeedState::Pending;
                            Some(seed_idx)
                        } else if is_bench && instructions.is_none() {
                            // seed was already rolled but not benchmarked, roll a new seed instead
                            fs::remove_dir_all(stats_dir.join(seed_idx.to_string())).await?;
                            seed_states[usize::from(seed_idx)] = SeedState::Pending;
                            Some(seed_idx)
                        } else {
                            seed_states[usize::from(seed_idx)] = SeedState::Failure {
                                worker: None,
                                error_log: fs::read(stats_dir.join(seed_idx.to_string()).join("error.log")).await?.into(),
                                instructions,
                            };
                            None
                        },
                        ReaderMessage::Done => {
                            completed_readers += 1;
                            None
                        }
                    },
                    Event::WorkerDone(name, result) => if_chain! {
                        if let Ok(ref mut workers) = workers;
                        if let Some(worker) = workers.iter_mut().find(|worker| worker.name == name);
                        then {
                            worker.stopped = true;
                            let mut seed_to_reroll = None;
                            match result? {
                                Ok(()) => {}
                                Err(e) => {
                                    worker.error = Some(e);
                                    let should_cancel = workers.iter().all(|worker| worker.error.is_some());
                                    for (seed_idx, state) in seed_states.iter_mut().enumerate() {
                                        if let SeedState::Rolling { workers } = state {
                                            if workers.contains(&name) {
                                                let new_workers = workers.iter().into_iter().filter(|worker| **worker != name).cloned().collect();
                                                if let Some(new_workers) = NEVec::from_vec(new_workers) {
                                                    *workers = new_workers;
                                                } else if should_cancel {
                                                    *state = SeedState::Cancelled;
                                                } else {
                                                    *state = SeedState::Pending;
                                                    seed_to_reroll.get_or_insert(seed_idx.try_into()?);
                                                }
                                            }
                                        }
                                    }
                                    if should_cancel {
                                        cancel!(workers);
                                    }
                                }
                            }
                            seed_to_reroll
                        } else {
                            return Err(Error::WorkerNotFound)
                        }
                    },
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
                                    while worker.error.is_none() && worker.ready > 0 {
                                        worker.msg = None;
                                        if let Some(seed_idx) = seed_states.iter().position(|state| matches!(state, SeedState::Pending)) {
                                            if let Err(mpsc::error::SendError(message)) = worker.roll(&mut seed_states, seed_idx.try_into()?).await {
                                                worker.error.get_or_insert(worker::Error::Receive { message });
                                                cancel!(workers);
                                                break
                                            }
                                        } else if args.race {
                                            let seed_idx = seed_states.iter()
                                                .enumerate()
                                                .filter_map(|(seed_idx, state)| if let SeedState::Rolling { workers } = state { Some((seed_idx, workers.len())) } else { None })
                                                .min_by_key(|&(_, num_workers)| num_workers);
                                            if let Some((seed_idx, _)) = seed_idx {
                                                if let Err(mpsc::error::SendError(message)) = worker.roll(&mut seed_states, seed_idx.try_into()?).await {
                                                    worker.error.get_or_insert(worker::Error::Receive { message });
                                                    cancel!(workers);
                                                    break
                                                }
                                            } else {
                                                break
                                            }
                                        } else {
                                            break
                                        }
                                    }
                                    None
                                }
                                ootrstats::worker::Message::Success { seed_idx, instructions, spoiler_log, patch } => if let SeedState::Rolling { workers: ref mut worker_names } = seed_states[usize::from(seed_idx)] {
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
                                                if let Some(wsl_distro) = wsl {
                                                    let mut cmd = Command::new(WSL);
                                                    if let Some(wsl_distro) = &wsl_distro {
                                                        cmd.arg("--distribution");
                                                        cmd.arg(wsl_distro);
                                                    }
                                                    cmd.arg("cat");
                                                    cmd.arg(&patch_path);
                                                    let patch = cmd.check("wsl cat").await?.stdout;
                                                    fs::write(stats_patch_path, patch).await?;
                                                    let mut cmd = Command::new(WSL);
                                                    if let Some(wsl_distro) = &wsl_distro {
                                                        cmd.arg("--distribution");
                                                        cmd.arg(wsl_distro);
                                                    }
                                                    cmd.arg("rm");
                                                    cmd.arg(patch_path);
                                                    cmd.check("wsl rm").await?;
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
                                    fs::write_json(seed_dir.join("metadata.json"), Metadata {
                                        instructions: Some(instructions.as_ref().copied().map_err(|stderr| String::from_utf8_lossy(stderr).into_owned())),
                                        worker: Some(name.clone()),
                                    }).await?;
                                    let mut new_workers = Vec::from(worker_names.clone());
                                    let Some(pos) = new_workers.iter().position(|worker| *worker == name) else { panic!("got success from a worker ({name}) that wasn't rolling that seed ({seed_idx})") };
                                    new_workers.swap_remove(pos);
                                    if_chain! {
                                        if !cancelled;
                                        if is_bench;
                                        if let Err(ref stderr) = instructions;
                                        then {
                                            // perf sometimes doesn't output instruction count for whatever reason, retry if this happens
                                            log!("worker {name} retrying seed {seed_idx} due to missing instruction count, stderr:");
                                            log!("{}", String::from_utf8_lossy(stderr));
                                            fs::remove_dir_all(seed_dir).await?;
                                            if let Some(new_workers) = NEVec::from_vec(new_workers) {
                                                *worker_names = new_workers;
                                            } else {
                                                seed_states[usize::from(seed_idx)] = SeedState::Pending;
                                            }
                                            Some(seed_idx)
                                        } else {
                                            // cancel remaining raced copies of this seed
                                            for name in new_workers {
                                                if let Some(worker) = workers.iter().find(|worker| worker.name == name) {
                                                    let _ = worker.supervisor_tx.send(ootrstats::worker::SupervisorMessage::Cancel(seed_idx)).await;
                                                } else {
                                                    return Err(Error::WorkerNotFound)
                                                }
                                            }
                                            seed_states[usize::from(seed_idx)] = SeedState::Success {
                                                worker: Some(name),
                                                spoiler_log: match spoiler_log {
                                                    Either::Left(_) => fs::read_json(stats_dir.join(seed_idx.to_string()).join("spoiler.json")).await?,
                                                    Either::Right(spoiler_log) => serde_json::from_slice(&spoiler_log)?,
                                                },
                                                instructions: instructions.as_ref().ok().copied(),
                                            };
                                            None
                                        }
                                    }
                                } else {
                                    // seed was already rolled but this worker's instance of this seed didn't get cancelled in time so we just ignore it
                                    None
                                },
                                ootrstats::worker::Message::Failure { seed_idx, instructions, error_log } => if let SeedState::Rolling { workers: ref mut worker_names } = seed_states[usize::from(seed_idx)] {
                                    let seed_dir = stats_dir.join(seed_idx.to_string());
                                    let mut new_workers = Vec::from(worker_names.clone());
                                    let pos = new_workers.iter().position(|worker| *worker == name).expect("got failure from a worker that wasn't rolling that seed");
                                    new_workers.swap_remove(pos);
                                    if args.retry_failures {
                                        fs::remove_dir_all(seed_dir).await.missing_ok()?;
                                        if let Some(new_workers) = NEVec::from_vec(new_workers) {
                                            *worker_names = new_workers;
                                        } else {
                                            seed_states[usize::from(seed_idx)] = SeedState::Pending;
                                        }
                                        Some(seed_idx)
                                    } else {
                                        fs::create_dir_all(&seed_dir).await?;
                                        let stats_error_log_path = seed_dir.join("error.log");
                                        fs::write(stats_error_log_path, &error_log).await?;
                                        fs::write_json(seed_dir.join("metadata.json"), Metadata {
                                            instructions: Some(instructions.as_ref().copied().map_err(|stderr| String::from_utf8_lossy(stderr).into_owned())),
                                            worker: Some(name.clone()),
                                        }).await?;
                                        if_chain! {
                                            if !cancelled;
                                            if is_bench;
                                            if let Err(ref stderr) = instructions;
                                            then {
                                                // perf sometimes doesn't output instruction count for whatever reason, retry if this happens
                                                log!("worker {name} retrying seed {seed_idx} due to missing instruction count, stderr:");
                                                log!("{}", String::from_utf8_lossy(stderr));
                                                fs::remove_dir_all(seed_dir).await?;
                                                if let Some(new_workers) = NEVec::from_vec(new_workers) {
                                                    *worker_names = new_workers;
                                                } else {
                                                    seed_states[usize::from(seed_idx)] = SeedState::Pending;
                                                }
                                                Some(seed_idx)
                                            } else {
                                                // cancel remaining raced copies of this seed
                                                for name in new_workers {
                                                    if let Some(worker) = workers.iter().find(|worker| worker.name == name) {
                                                        let _ = worker.supervisor_tx.send(ootrstats::worker::SupervisorMessage::Cancel(seed_idx)).await;
                                                    } else {
                                                        return Err(Error::WorkerNotFound)
                                                    }
                                                }
                                                seed_states[usize::from(seed_idx)] = SeedState::Failure {
                                                    worker: Some(name),
                                                    instructions: instructions.as_ref().ok().copied(),
                                                    error_log,
                                                };
                                                None
                                            }
                                        }
                                    }
                                } else {
                                    // seed was already rolled but this worker's instance of this seed didn't get cancelled in time so we just ignore it
                                    None
                                },
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
                                .filter(|worker::Config { name, .. }| args.include_workers.is_empty() || args.include_workers.contains(name))
                                .filter(|worker::Config { name, .. }| !args.exclude_workers.contains(name))
                                .filter(|&worker::Config { bench, .. }| bench || !is_bench)
                                .map(|worker::Config { name, kind, .. }| {
                                    let (task, state) = worker::State::new(worker_tx.clone(), name.clone(), kind, rando_rev, &setup, if let Some(Subcommand::Bench { uncompressed, .. }) = args.subcommand {
                                        if args.patch { unimplemented!("The `bench` subcommand currently cannot generate patch files") }
                                        if uncompressed { OutputMode::BenchUncompressed } else { OutputMode::Bench }
                                    } else {
                                        if args.patch { OutputMode::Patch } else { OutputMode::Normal }
                                    });
                                    (task.map(move |res| (name, res)), state)
                                })
                                .unzip::<_, _, _, Vec<_>>();
                            worker_tasks = new_worker_tasks;
                            workers = Ok(new_workers);
                            workers.as_mut().ok().expect("just inserted")
                        }
                    };
                    for worker in workers.iter_mut().filter(|worker| worker.error.is_none() && worker.ready > 0) {
                        if worker.roll(&mut seed_states, seed_idx).await.is_ok() {
                            break
                        }
                    }
                }
            },
            //TODO use signal-hook-tokio crate to handle interrupts on Unix?
            Some(res) = cli_rx.recv() => if let crossterm::event::Event::Key(KeyEvent { code: KeyCode::Char('c' | 'd'), kind: KeyEventKind::Press, .. }) = res.at_unknown()? {
                cancel!(workers.as_deref().unwrap_or_default());
            },
        }
        Message::Status {
            retry_failures: args.retry_failures,
            seed_states: &seed_states,
            workers: workers.as_deref().ok(),
            available_parallelism, completed_readers, start, start_local,
        }.print(args.json_messages, &mut stderr)?;
        if completed_readers == available_parallelism && seed_states.iter().all(|state| match state {
            SeedState::Cancelled | SeedState::Success { .. } | SeedState::Failure { .. } => true,
            SeedState::Unchecked | SeedState::Pending | SeedState::Rolling { .. } => false,
        }) {
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
    if !args.json_messages {
        if let Ok(ref workers) = workers {
            crossterm::execute!(stderr,
                MoveDown(workers.len() as u16),
            ).at_unknown()?;
        }
        crossterm::execute!(stderr,
            Print("\r\n"),
        ).at_unknown()?;
    }
    match args.subcommand {
        None => Message::Done { stats_dir }.print(args.json_messages, &mut stderr)?,
        Some(Subcommand::Bench { raw_data: false, uncompressed: _ }) => {
            let mut num_successes = 0u16;
            let mut num_failures = 0u16;
            let mut instructions_success = 0u64;
            let mut instructions_failure = 0u64;
            for state in seed_states {
                match state {
                    SeedState::Unchecked | SeedState::Pending | SeedState::Rolling { .. } => unreachable!(),
                    SeedState::Cancelled | SeedState::Success { instructions: None, .. } | SeedState::Failure { instructions: None, .. } => {}
                    SeedState::Success { instructions: Some(instructions), .. } => {
                        num_successes += 1;
                        instructions_success += instructions;
                    }
                    SeedState::Failure { instructions: Some(instructions), .. } => {
                        num_failures += 1;
                        instructions_failure += instructions;
                    }
                }
            }
            if num_successes == 0 {
                Message::InstructionsNoSuccesses.print(args.json_messages, &mut stdout)?;
            } else {
                let success_rate = num_successes as f64 / (num_successes as f64 + num_failures as f64);
                let average_instructions_success = instructions_success / u64::try_from(num_successes).unwrap();
                let average_instructions_failure = instructions_failure.checked_div(u64::try_from(num_failures).unwrap()).unwrap_or_default();
                let average_failure_count = (1.0 - success_rate) / success_rate; // mean of 0-support geometric distribution
                let average_instructions = average_failure_count * average_instructions_failure as f64 + average_instructions_success as f64;
                Message::Instructions { num_successes, num_failures, success_rate, average_instructions_success, average_instructions_failure, average_failure_count, average_instructions }.print(args.json_messages, &mut stdout)?;
            }
        }
        Some(Subcommand::Bench { raw_data: true, uncompressed: _ }) => {
            for state in seed_states {
                match state {
                    SeedState::Unchecked | SeedState::Pending | SeedState::Rolling { .. } => unreachable!(),
                    SeedState::Cancelled | SeedState::Success { instructions: None, .. } | SeedState::Failure { instructions: None, .. } => {}
                    SeedState::Success { instructions: Some(instructions), .. } => {
                        crossterm::execute!(stdout,
                            Print(format_args!("s {instructions}\r\n")),
                        ).at_unknown()?;
                    }
                    SeedState::Failure { instructions: Some(instructions), .. } => {
                        crossterm::execute!(stdout,
                            Print(format_args!("f {instructions}\r\n")),
                        ).at_unknown()?;
                    }
                }
            }
        }
        Some(Subcommand::Categorize { query }) => {
            let mut defs = jaq_interpret::ParseCtx::new(Vec::default());
            defs.insert_natives(jaq_core::core());
            defs.insert_defs(jaq_std::std());
            if !defs.errs.is_empty() {
                return Err(Error::Jaq)
            }
            let (filter, errs) = jaq_parse::parse(&query, jaq_parse::main());
            if !errs.is_empty() {
                return Err(Error::Jaq)
            }
            let filter = defs.compile(filter.unwrap());
            if !defs.errs.is_empty() {
                return Err(Error::Jaq)
            }
            let inputs = jaq_interpret::RcIter::new(iter::empty());
            let mut outputs = BTreeMap::<jaq_interpret::Val, usize>::default();
            for state in seed_states {
                if let SeedState::Success { spoiler_log, .. } = state {
                    for value in jaq_interpret::FilterT::run(&filter, (jaq_interpret::Ctx::new([], &inputs), jaq_interpret::Val::from(spoiler_log))) {
                        *outputs.entry(value.map_err(|_| Error::Jaq)?).or_default() += 1;
                    }
                }
            }
            let mut outputs = outputs.into_iter().collect_vec();
            outputs.sort_by(|(_, count1), (_, count2)| count2.cmp(count1));
            for (output, count) in outputs {
                Message::Category { output: output.into(), count }.print(args.json_messages, &mut stdout)?;
            }
        }
        Some(Subcommand::Failures) => {
            let mut counts = HashMap::<_, HashMap<_, (SeedIdx, usize)>>::default();
            for (seed_idx, state) in seed_states.iter().enumerate() {
                if let SeedState::Failure { error_log, .. } = state {
                    let error_log = std::str::from_utf8(error_log)?;
                    let mut lines = error_log.trim().lines();
                    let msg = lines.next_back().ok_or(Error::EmptyErrorLog)?;
                    let _ = lines.next_back().ok_or(Error::MissingTraceback)?;
                    let location = lines.next_back().ok_or(Error::MissingTraceback)?;
                    match counts.entry(location).or_default().entry(msg) {
                        hash_map::Entry::Occupied(mut entry) => entry.get_mut().1 += 1,
                        hash_map::Entry::Vacant(entry) => { entry.insert((seed_idx.try_into()?, 1)); }
                    }
                }
            }
            Message::FailuresHeader { stats_dir }.print(args.json_messages, &mut stdout)?;
            for msgs in counts.into_values().sorted_unstable_by_key(|msgs| -(msgs.values().map(|&(_, count)| count).sum::<usize>() as isize)).take(10) {
                let count = msgs.values().map(|&(_, count)| count).sum::<usize>();
                let mut msgs = msgs.into_iter().collect_vec();
                msgs.sort_unstable_by_key(|&(_, (_, count))| count);
                let (top_msg, (seed_idx, top_count)) = msgs.pop().expect("no error messages");
                Message::Failure { count, top_msg, top_count, seed_idx, msgs }.print(args.json_messages, &mut stdout)?;
            }
        }
        Some(Subcommand::MidosHouse { out_path }) => {
            let mut counts = HashMap::<_, usize>::default();
            for state in seed_states {
                if let SeedState::Success { spoiler_log, .. } = state {
                    for appearances in serde_json::from_value::<SpoilerLog>(spoiler_log)?.midos_house_chests() {
                        *counts.entry(appearances).or_default() += 1;
                    }
                }
            }
            let mut counts = counts.into_iter().collect_vec();
            counts.sort_unstable();
            fs::write_json(out_path, counts).await?;
        }
    }
    if let Ok(workers) = workers {
        let worker_errors = workers.into_iter()
            .filter_map(|worker| Some((worker.name, worker.error?)))
            .collect_vec();
        if !worker_errors.is_empty() {
            return Err(Error::Worker(worker_errors))
        }
    }
    Ok(())
}

#[wheel::main(custom_exit)]
async fn main(args: Args) -> Result<(), Error> {
    if !args.json_messages {
        enable_raw_mode().at_unknown()?;
    }
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
