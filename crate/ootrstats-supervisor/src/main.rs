#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use {
    std::{
        borrow::Cow,
        collections::{
            BTreeMap,
            HashSet,
            hash_map::{
                self,
                HashMap,
            },
        },
        ffi::OsString,
        io::{
            self,
            IsTerminal as _,
            stderr,
            stdout,
        },
        iter,
        num::NonZero,
        path::PathBuf,
        sync::Arc,
    },
    bytes::Bytes,
    chrono::prelude::*,
    crossterm::{
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
        future::FutureExt as _,
        stream::{
            FuturesUnordered,
            StreamExt as _,
        },
    },
    if_chain::if_chain,
    itertools::Itertools as _,
    lazy_regex::regex_is_match,
    nonempty_collections::{
        IntoIteratorExt as _,
        NEVec,
        NonEmptyIterator as _,
        nev,
    },
    ootr_utils::spoiler::SpoilerLog,
    ootrstats_supervisor as _, // included directly as modules
    proc_macro2 as _, // feature config required for Span::start used in CustomExit impl
    rustls as _, // feature ring required for WebSocket connections to work
    serde::{
        Deserialize,
        Serialize,
    },
    tokio::{
        io::AsyncWriteExt as _,
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
            IsNetworkError,
        },
    },
    ootrstats::{
        OutputMode,
        RandoSettings,
        RandoSetup,
        SeedIdx,
        Seeds,
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

fn parse_traceback<'a>(worker: &Arc<str>, error_log: &'a str) -> Result<(&'a str, &'a str), Error> {
    //TODO account for additional output from macOS `time`
    let mut rev_lines = error_log.trim().lines().rev();
    let mut msg = rev_lines.next().ok_or(Error::EmptyErrorLog)?;
    let _ = rev_lines.next().ok_or_else(|| Error::MissingTraceback { worker: worker.clone(), error_log: error_log.to_owned(), missing_part: "skip" })?;
    let mut location = rev_lines.next().ok_or_else(|| Error::MissingTraceback { worker: worker.clone(), error_log: error_log.to_owned(), missing_part: "location" })?;
    if rev_lines.any(|line| line.contains("Performance counter stats")) {
        let _ = rev_lines.next().ok_or(Error::EmptyErrorLog);
        msg = rev_lines.next().ok_or(Error::EmptyErrorLog)?;
        let _ = rev_lines.next().ok_or_else(|| Error::MissingTraceback { worker: worker.clone(), error_log: error_log.to_owned(), missing_part: "skip (perf)" })?;
        location = rev_lines.next().ok_or_else(|| Error::MissingTraceback { worker: worker.clone(), error_log: error_log.to_owned(), missing_part: "location (perf)" })?;
    }
    Ok((location, msg))
}

enum ReaderMessage {
    Pending {
        seed_idx: SeedIdx,
        allowed_workers: Option<NEVec<Arc<str>>>,
    },
    Success {
        seed_idx: SeedIdx,
        worker: Arc<str>,
        instructions: Option<u64>,
        rsl_instructions: Option<u64>,
    },
    Failure {
        seed_idx: SeedIdx,
        worker: Arc<str>,
        instructions: Option<u64>,
        rsl_instructions: Option<u64>,
    },
    Done,
}

#[derive(Deserialize, Serialize)]
struct Metadata {
    /// present if the `bench` parameter was set.
    instructions: Option<Result<u64, String>>,
    rsl_instructions: Option<Result<u64, String>>,
    /// always written by this version of ootrstats but may be absent in metadata from older ootrstats versions.
    worker: Arc<str>,
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
        /// None if the seed was read from disk.
        #[serde(skip)]
        completed_at: Option<Instant>,
        worker: Arc<str>,
        instructions: Option<u64>,
        rsl_instructions: Option<u64>,
        spoiler_log: serde_json::Value,
    },
    Failure {
        /// None if the seed was read from disk.
        #[serde(skip)]
        completed_at: Option<Instant>,
        worker: Arc<str>,
        instructions: Option<u64>,
        rsl_instructions: Option<u64>,
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
    #[clap(long)]
    repo: Option<String>,
    #[clap(short, long, conflicts_with("rev"))]
    branch: Option<String>,
    #[clap(long)]
    rev: Option<gix::ObjectId>,
    #[clap(short, long)]
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
    /// Generates a fixed seed. Useful for confirming suspected unseeded randomization.
    #[clap(long)]
    seed: Option<String>,
    /// Generate .zpf/.zpfz patch files.
    #[clap(long, conflicts_with("rsl"))]
    patch: bool,
    /// Generate .z64 rom files.
    #[clap(long, conflicts_with("rsl"))]
    rom: bool,
    /// Generate uncompressed .n64 rom files.
    #[clap(long, conflicts_with("rsl"))]
    uncompressed_rom: bool,

    // ootrstats settings

    /// Sample size — how many seeds to roll.
    #[clap(short, long, default_value = "16384", default_value_if("world_counts", "true", Some("255")))]
    num_seeds: NonZero<SeedIdx>,
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
    /// Randomizer or RSL script git revision to compare against when benchmarking.
    #[clap(long)]
    baseline_rev: Option<gix::ObjectId>,
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
    #[error("cancelled by user")]
    Cancelled,
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
    MissingTraceback {
        worker: Arc<str>,
        error_log: String,
        missing_part: &'static str,
    },
    #[error("found both spoiler and error logs for a seed")]
    SuccessAndFailure,
    #[error("at most 255 seeds may be generated with the --world-counts option")]
    TooManyWorlds,
    #[error("error(s) in worker(s): {}", .worker_errors.iter().map(|(worker, source)| format!("{worker}: {source}")).format(", "))]
    Worker {
        worker_errors: Vec<(Arc<str>, worker::Error)>,
        cancelled: bool,
    },
    #[error("received a message from an unknown worker")]
    WorkerNotFound,
}

impl IsNetworkError for Error {
    fn is_network_error(&self) -> bool {
        match self {
            | Self::Config(_)
            | Self::GitHeadId(_)
            | Self::GitOpen(_)
            | Self::Json(_)
            | Self::Task(_)
            | Self::TryFromInt(_)
            | Self::ReaderSend(_)
            | Self::Utf8(_)
            | Self::Cancelled
            | Self::DraftParse { .. }
            | Self::EmptyErrorLog
            | Self::Jaq
            | Self::MissingTraceback { .. }
            | Self::SuccessAndFailure
            | Self::TooManyWorlds
            | Self::WorkerNotFound
                => false,
            #[cfg(windows)] Self::MissingHomeDir => false,
            Self::Wheel(e) => e.is_network_error(),
            Self::Worker { worker_errors, .. } => worker_errors.iter().all(|(_, e)| e.is_network_error()),
        }
    }
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
            Self::Cancelled => eprintln!("cancelled by pressing C or D\r"),
            Self::DraftParse { file: _ /*TODO display the span of code? */, source } => {
                eprintln!("{cmd_name}: error parsing draft spec: {source}\r");
                let start = source.span().start();
                eprintln!("line {}, column {}\r", start.line, start.column);
                eprintln!("debug info: {debug}\r");
            }
            Self::Worker { worker_errors, .. } => match worker_errors.into_iter().exactly_one() {
                Ok((worker, worker::Error::Local(ootrstats::worker::Error::Roll(ootrstats::RollError::PerfSyntax(stderr))))) => {
                    eprintln!("{cmd_name}: roll error in worker {worker}: failed to parse `perf` output\r");
                    eprintln!("stderr:\r");
                    eprintln!("{}\r", String::from_utf8_lossy(&stderr).lines().filter(|line| !regex_is_match!("^[0-9]+ files remaining$", line)).format("\r\n"));
                }
                Ok((worker, source)) => {
                    eprintln!("{cmd_name}: {} in worker {worker}: {source}\r", if source.is_network_error() { "network error" } else { "error" });
                    eprintln!("debug info: {debug}\r");
                }
                Err(errors) => {
                    eprintln!("{cmd_name}: errors in workers:\r");
                    for (worker, source) in errors {
                        eprintln!("\r");
                        eprintln!("{} in worker {worker}: {}\r", if source.is_network_error() { "network error" } else { "error" }, source.to_string().lines().format("\r\n"));
                        let mut debug = format!("{source:?}");
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
                        eprintln!("debug info: {debug}\r");
                    }
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

async fn cli(label: Option<&'static str>, mut args: Args) -> Result<bool, Error> {
    if args.world_counts && args.num_seeds.get() > 255 {
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
    Message::Preparing(label).print(args.json_messages, &mut stderr)?;
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

    let is_bench = matches!(args.subcommand, Some(Subcommand::Bench { .. }));
    let repo = if let Some(repo) = args.repo {
        Cow::Owned(repo)
    } else if args.rsl {
        Cow::Borrowed("plando-random-settings")
    } else {
        Cow::Borrowed("OoT-Randomizer")
    };
    let rando_rev = if let Some(rev) = args.rev {
        rev
    } else {
        let mut dir_parent = gitdir().await?.join("github.com").join(&args.github_user).join(&*repo);
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
            cmd.arg(format!("https://github.com/{}/{repo}.git", args.github_user));
            if let Some(ref branch) = args.branch {
                cmd.arg("--branch");
                cmd.arg(branch);
            }
            cmd.arg(dir_name);
            cmd.current_dir(dir_parent).check("git clone").await?;
        }
        gix::open(dir)?.head_id()?.detach()
    };
    let baseline_rando_rev = 'baseline_rando_rev: {
        if let Some(rev) = args.baseline_rev {
            Some(rev)
        } else if is_bench && args.github_user == "fenhl" {
            if args.rev.is_some() {
                None
            } else {
                let dir_parent = gitdir().await?.join("github.com").join(&args.github_user).join(&*repo);
                let dir_name = match args.branch.as_deref() {
                    Some("riir2") if args.rsl => "main",
                    Some("riir") if !args.rsl => "main",
                    _ => break 'baseline_rando_rev None,
                };
                let dir = dir_parent.join(dir_name);
                if fs::exists(&dir).await? {
                    Some(gix::open(dir)?.head_id()?.detach())
                } else {
                    break 'baseline_rando_rev None
                }
            }
        } else {
            None
        }
    };
    let setup = if args.rsl {
        RandoSetup::Rsl {
            github_user: args.github_user,
            repo: repo.into_owned(),
            preset: args.preset,
            seeds: if let Some(seed) = args.seed {
                Seeds::Fixed(seed)
            } else if args.retry_failures {
                Seeds::Random
            } else {
                Seeds::Default
            },
        }
    } else {
        RandoSetup::Normal {
            github_user: args.github_user,
            repo: repo.into_owned(),
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
            seeds: if let Some(seed) = args.seed {
                Seeds::Fixed(seed)
            } else if args.retry_failures {
                Seeds::Random
            } else {
                Seeds::Default
            },
        }
    };
    let stats_root = if let Some(stats_dir) = config.stats_dir.take() {
        stats_dir
    } else {
        #[cfg(windows)] let project_dirs = ProjectDirs::from("net", "Fenhl", "ootrstats").ok_or(Error::MissingHomeDir)?;
        #[cfg(windows)] { project_dirs.data_dir().to_owned() }
        #[cfg(unix)] { BaseDirectories::new().place_data_file("ootrstats").at_unknown()? }
    };
    let stats_dir = stats_root.join(setup.stats_dir(rando_rev));
    let baseline_stats_dir = baseline_rando_rev.map(|rando_rev| stats_root.join(setup.stats_dir(rando_rev)));
    if args.clean {
        fs::remove_dir_all(&stats_dir).await.missing_ok()?;
    }
    let available_parallelism = std::thread::available_parallelism().unwrap_or(NonZero::<usize>::MIN).try_into().unwrap_or(NonZero::<SeedIdx>::MAX).min(args.num_seeds);
    let start = Instant::now();
    let start_local = Local::now();
    let mut seed_states = Vec::from_iter(iter::repeat_with(|| SeedState::Unchecked).take(args.num_seeds.get().into()));
    let mut allowed_workers = HashMap::new();
    let (reader_tx, mut reader_rx) = mpsc::channel(args.num_seeds.get().min(256).into());
    let mut readers = (0..available_parallelism.get()).map(|task_idx| {
        let stats_dir = stats_dir.clone();
        let baseline_stats_dir = baseline_stats_dir.clone();
        let reader_tx = reader_tx.clone();
        tokio::spawn(async move {
            for seed_idx in (task_idx..args.num_seeds.get()).step_by(available_parallelism.get().into()) {
                let seed_path = stats_dir.join(seed_idx.to_string());
                let stats_spoiler_log_path = seed_path.join("spoiler.json");
                let stats_error_log_path = seed_path.join("error.log");
                match (fs::exists(&stats_spoiler_log_path).await?, fs::exists(&stats_error_log_path).await?) {
                    (false, false) => reader_tx.send(ReaderMessage::Pending {
                        allowed_workers: if let Some(ref baseline_stats_dir) = baseline_stats_dir {
                            let baseline_seed_path = baseline_stats_dir.join(seed_idx.to_string());
                            match fs::read_json(baseline_seed_path.join("metadata.json")).await {
                                Ok(Metadata { worker, .. }) => Some(nev![worker]),
                                Err(wheel::Error::Io { inner, .. }) if inner.kind() == io::ErrorKind::NotFound => None,
                                Err(e) => return Err(e.into()),
                            }
                        } else {
                            None
                        },
                        seed_idx,
                    }).await?,
                    (false, true) => {
                        let Metadata { instructions, rsl_instructions, worker } = fs::read_json(seed_path.join("metadata.json")).await?;
                        reader_tx.send(ReaderMessage::Failure {
                            instructions: instructions.and_then(Result::ok),
                            rsl_instructions: rsl_instructions.and_then(Result::ok),
                            seed_idx, worker,
                        }).await?;
                    }
                    (true, false) => {
                        let Metadata { instructions, rsl_instructions, worker } = fs::read_json(seed_path.join("metadata.json")).await?;
                        reader_tx.send(ReaderMessage::Success {
                            instructions: instructions.and_then(Result::ok),
                            rsl_instructions: rsl_instructions.and_then(Result::ok),
                            seed_idx, worker,
                        }).await?;
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
    let mut worker_tx = Some(worker_tx);
    let mut worker_tasks = FuturesUnordered::default();
    let mut workers = config.workers.iter()
        .filter(|worker::Config { name, .. }| args.include_workers.is_empty() || args.include_workers.contains(name))
        .filter(|worker::Config { name, .. }| !args.exclude_workers.contains(name))
        .filter(|worker::Config { bench, .. }| *bench || !is_bench)
        .map(|worker::Config { name, .. }| worker::State::new(name.clone()))
        .collect_vec();
    let mut cancelled = false;
    let mut cancelled_by_user = false;

    macro_rules! cancel {
        ($workers:expr) => {{
            // finish rolling seeds that are already in progress but don't start any more
            cancelled = true;
            args.retry_failures = false;
            readers.clear();
            completed_readers = available_parallelism.get();
            reader_rx = mpsc::channel(1).1;
            for (seed_idx, seed_state) in seed_states.iter_mut().enumerate() {
                match seed_state {
                    SeedState::Unchecked | SeedState::Pending => *seed_state = SeedState::Cancelled,
                    SeedState::Rolling { workers: worker_names } => if args.race {
                        // --race means the user is okay with randomizer instances being cancelled, so we do that here to speed up the exit
                        for name in worker_names.iter() {
                            if let Some(worker) = $workers.iter().find(|worker| worker.name == *name) {
                                if let Some(tx) = &worker.supervisor_tx {
                                    let _ = tx.send(ootrstats::worker::SupervisorMessage::Cancel(seed_idx.try_into()?)).await;
                                }
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
                log!(
                    "waiting for event, readers {}, reader_rx {}, worker_tasks {}, worker_rx {}",
                    if readers.is_empty() { "empty" } else { "present" },
                    if reader_rx.is_closed() { "closed" } else { "open" },
                    if worker_tasks.is_empty() { "empty" } else { "present" },
                    if worker_rx.is_closed() { "closed" } else { "open" },
                ); // debugging workers getting stuck stopping
                Ok::<_, Error>(select! {
                    Some(res) = readers.next() => Event::ReaderDone(res),
                    Some(msg) = reader_rx.recv() => Event::ReaderMessage(msg),
                    Some((name, res)) = worker_tasks.next() => Event::WorkerDone(name, res),
                    Some((name, msg)) = worker_rx.recv() => Event::WorkerMessage(name, msg),
                    else => Event::End,
                })
            } => {
                match event? {
                    Event::ReaderDone(res) => { let () = res??; }
                    Event::ReaderMessage(msg) => match msg {
                        ReaderMessage::Pending { seed_idx, allowed_workers: seed_allowed_workers } => {
                            if let Some(seed_allowed_workers) = seed_allowed_workers {
                                allowed_workers.insert(seed_idx, seed_allowed_workers);
                            }
                            seed_states[usize::from(seed_idx)] = SeedState::Pending;
                        }
                        ReaderMessage::Success { seed_idx, worker, instructions, rsl_instructions } => {
                            allowed_workers.insert(seed_idx, nev![worker.clone()]);
                            if is_bench && instructions.is_none() {
                                // seed was already rolled but not benchmarked, roll a new seed instead
                                fs::remove_dir_all(stats_dir.join(seed_idx.to_string())).await?;
                                seed_states[usize::from(seed_idx)] = SeedState::Pending;
                            } else {
                                seed_states[usize::from(seed_idx)] = SeedState::Success {
                                    completed_at: None,
                                    spoiler_log: fs::read_json(stats_dir.join(seed_idx.to_string()).join("spoiler.json")).await?,
                                    worker, instructions, rsl_instructions,
                                };
                            }
                        }
                        ReaderMessage::Failure { worker, seed_idx, instructions, rsl_instructions } => {
                            let error_log = Bytes::from(fs::read(stats_dir.join(seed_idx.to_string()).join("error.log")).await?);
                            if args.retry_failures || parse_traceback(&worker, std::str::from_utf8(&error_log)?)?.1.contains("Cannot allocate memory") {
                                fs::remove_dir_all(stats_dir.join(seed_idx.to_string())).await?;
                                seed_states[usize::from(seed_idx)] = SeedState::Pending;
                            } else {
                                allowed_workers.insert(seed_idx, nev![worker.clone()]);
                                if is_bench && instructions.is_none() {
                                    // seed was already rolled but not benchmarked, roll a new seed instead
                                    fs::remove_dir_all(stats_dir.join(seed_idx.to_string())).await?;
                                    seed_states[usize::from(seed_idx)] = SeedState::Pending;
                                } else {
                                    seed_states[usize::from(seed_idx)] = SeedState::Failure {
                                        completed_at: None,
                                        worker, instructions, rsl_instructions, error_log,
                                    };
                                }
                            }
                        }
                        ReaderMessage::Done => completed_readers += 1,
                    },
                    Event::WorkerDone(name, result) => if let Some(worker) = workers.iter_mut().find(|worker| worker.name == name) {
                        match result? {
                            Ok(()) => worker.stopped = true,
                            Err(e) => {
                                if e.is_network_error() {
                                    worker.ready = 0;
                                    worker.supervisor_tx = None;
                                } else {
                                    worker.stopped = true;
                                }
                                worker.error = Some(e);
                                let should_cancel = workers.iter().all(|worker| worker.error.as_ref().is_some_and(|e| !e.is_network_error()));
                                for state in &mut seed_states {
                                    if let SeedState::Rolling { workers } = state {
                                        if workers.contains(&name) {
                                            if let Some(new_workers) = workers.iter().filter(|worker| **worker != name).cloned().try_into_nonempty_iter() {
                                                *workers = new_workers.collect();
                                            } else if should_cancel {
                                                *state = SeedState::Cancelled;
                                            } else {
                                                *state = SeedState::Pending;
                                            }
                                        }
                                    }
                                }
                                if should_cancel {
                                    cancel!(workers);
                                }
                            }
                        }
                    } else {
                        return Err(Error::WorkerNotFound)
                    },
                    Event::WorkerMessage(name, msg) => if let Some(worker) = workers.iter_mut().find(|worker| worker.name == name) {
                        match msg {
                            ootrstats::worker::Message::Init(msg) => worker.msg = Some(msg),
                            ootrstats::worker::Message::Ready(ready) => {
                                worker.ready += ready;
                                if worker.error.is_none() && worker.ready > 0 {
                                    worker.msg = None;
                                }
                            }
                            ootrstats::worker::Message::Success { seed_idx, instructions, rsl_instructions, spoiler_log, patch, compressed_rom, uncompressed_rom, rsl_plando } => if let SeedState::Rolling { workers: ref mut worker_names } = seed_states[usize::from(seed_idx)] {
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
                                if let Some(compressed_rom) = compressed_rom {
                                    match compressed_rom {
                                        Either::Left((wsl, compressed_rom_path)) => {
                                            let stats_compressed_rom_path = seed_dir.join("rom.z64");
                                            if let Some(wsl_distro) = wsl {
                                                let mut cmd = Command::new(WSL);
                                                if let Some(wsl_distro) = &wsl_distro {
                                                    cmd.arg("--distribution");
                                                    cmd.arg(wsl_distro);
                                                }
                                                cmd.arg("cat");
                                                cmd.arg(&compressed_rom_path);
                                                let compressed_rom = cmd.check("wsl cat").await?.stdout;
                                                fs::write(stats_compressed_rom_path, compressed_rom).await?;
                                                let mut cmd = Command::new(WSL);
                                                if let Some(wsl_distro) = &wsl_distro {
                                                    cmd.arg("--distribution");
                                                    cmd.arg(wsl_distro);
                                                }
                                                cmd.arg("rm");
                                                cmd.arg(compressed_rom_path);
                                                cmd.check("wsl rm").await?;
                                            } else {
                                                let is_same_drive = {
                                                    #[cfg(windows)] {
                                                        compressed_rom_path.components().find_map(|component| if let std::path::Component::Prefix(prefix) = component { Some(prefix) } else { None })
                                                        == stats_compressed_rom_path.components().find_map(|component| if let std::path::Component::Prefix(prefix) = component { Some(prefix) } else { None })
                                                    }
                                                    #[cfg(not(windows))] { true }
                                                };
                                                if is_same_drive {
                                                    fs::rename(compressed_rom_path, stats_compressed_rom_path).await?;
                                                } else {
                                                    fs::copy(&compressed_rom_path, stats_compressed_rom_path).await?;
                                                    fs::remove_file(compressed_rom_path).await?;
                                                }
                                            }
                                        }
                                        Either::Right(compressed_rom) => {
                                            let stats_compressed_rom_path = seed_dir.join("rom.z64");
                                            fs::write(stats_compressed_rom_path, compressed_rom).await?;
                                        }
                                    }
                                }
                                if let Some(uncompressed_rom) = uncompressed_rom {
                                    match uncompressed_rom {
                                        Either::Left(uncompressed_rom_path) => {
                                            let stats_uncompressed_rom_path = seed_dir.join("uncompressed-rom.n64");
                                            let is_same_drive = {
                                                #[cfg(windows)] {
                                                    uncompressed_rom_path.components().find_map(|component| if let std::path::Component::Prefix(prefix) = component { Some(prefix) } else { None })
                                                    == stats_uncompressed_rom_path.components().find_map(|component| if let std::path::Component::Prefix(prefix) = component { Some(prefix) } else { None })
                                                }
                                                #[cfg(not(windows))] { true }
                                            };
                                            if is_same_drive {
                                                fs::rename(uncompressed_rom_path, stats_uncompressed_rom_path).await?;
                                            } else {
                                                fs::copy(&uncompressed_rom_path, stats_uncompressed_rom_path).await?;
                                                fs::remove_file(uncompressed_rom_path).await?;
                                            }
                                        }
                                        Either::Right(uncompressed_rom) => {
                                            let stats_uncompressed_rom_path = seed_dir.join("uncompressed-rom.n64");
                                            fs::write(stats_uncompressed_rom_path, uncompressed_rom).await?;
                                        }
                                    }
                                }
                                if let Some(rsl_plando) = rsl_plando {
                                    match rsl_plando {
                                        Either::Left(rsl_plando_path) => {
                                            let stats_rsl_plando_path = seed_dir.join("random_settings.json");
                                            let is_same_drive = {
                                                #[cfg(windows)] {
                                                    rsl_plando_path.components().find_map(|component| if let std::path::Component::Prefix(prefix) = component { Some(prefix) } else { None })
                                                    == stats_rsl_plando_path.components().find_map(|component| if let std::path::Component::Prefix(prefix) = component { Some(prefix) } else { None })
                                                }
                                                #[cfg(not(windows))] { true }
                                            };
                                            if is_same_drive {
                                                fs::rename(rsl_plando_path, stats_rsl_plando_path).await?;
                                            } else {
                                                fs::copy(&rsl_plando_path, stats_rsl_plando_path).await?;
                                                fs::remove_file(rsl_plando_path).await?;
                                            }
                                        }
                                        Either::Right(rsl_plando) => {
                                            let stats_rsl_plando_path = seed_dir.join("random_settings.json");
                                            fs::write(stats_rsl_plando_path, rsl_plando).await?;
                                        }
                                    }
                                }
                                fs::write_json(seed_dir.join("metadata.json"), Metadata {
                                    instructions: Some(instructions.as_ref().copied().map_err(|stderr| String::from_utf8_lossy(stderr).into_owned())),
                                    rsl_instructions: Some(rsl_instructions.as_ref().copied().map_err(|stderr| String::from_utf8_lossy(stderr).into_owned())),
                                    worker: name.clone(),
                                }).await?;
                                let mut new_workers = Vec::from(worker_names.clone());
                                let Some(pos) = new_workers.iter().position(|worker| *worker == name) else { panic!("got success from a worker ({name}) that wasn't rolling that seed ({seed_idx})") };
                                new_workers.swap_remove(pos);
                                if_chain! {
                                    if !cancelled;
                                    if is_bench;
                                    if let Some(ref stderr) = instructions.as_ref().err().or_else(|| rsl_instructions.as_ref().err());
                                    then {
                                        // perf sometimes doesn't output instruction count for whatever reason, retry if this happens
                                        log!("worker {name} retrying seed {seed_idx} due to missing instruction count, stderr:");
                                        log!("{}", String::from_utf8_lossy(stderr));
                                        fs::remove_dir_all(seed_dir).await?;
                                        if let Some(new_workers) = NEVec::try_from_vec(new_workers) {
                                            *worker_names = new_workers;
                                        } else {
                                            seed_states[usize::from(seed_idx)] = SeedState::Pending;
                                        }
                                    } else {
                                        // cancel remaining raced copies of this seed
                                        for name in new_workers {
                                            if let Some(worker) = workers.iter().find(|worker| worker.name == name) {
                                                if let Some(tx) = &worker.supervisor_tx {
                                                    let _ = tx.send(ootrstats::worker::SupervisorMessage::Cancel(seed_idx)).await;
                                                }
                                            } else {
                                                return Err(Error::WorkerNotFound)
                                            }
                                        }
                                        seed_states[usize::from(seed_idx)] = SeedState::Success {
                                            completed_at: Some(Instant::now()),
                                            worker: name,
                                            spoiler_log: match spoiler_log {
                                                Either::Left(_) => fs::read_json(stats_dir.join(seed_idx.to_string()).join("spoiler.json")).await?,
                                                Either::Right(spoiler_log) => serde_json::from_slice(&spoiler_log)?,
                                            },
                                            instructions: instructions.as_ref().ok().copied(),
                                            rsl_instructions: rsl_instructions.as_ref().ok().copied(),
                                        };
                                    }
                                }
                            } else {
                                // seed was already rolled but this worker's instance of this seed didn't get cancelled in time so we just ignore it
                            },
                            ootrstats::worker::Message::Failure { seed_idx, instructions, rsl_instructions, error_log, rsl_plando } => if let SeedState::Rolling { workers: ref mut worker_names } = seed_states[usize::from(seed_idx)] {
                                let seed_dir = stats_dir.join(seed_idx.to_string());
                                let mut new_workers = Vec::from(worker_names.clone());
                                let pos = new_workers.iter().position(|worker| *worker == name).expect("got failure from a worker that wasn't rolling that seed");
                                new_workers.swap_remove(pos);
                                if args.retry_failures || parse_traceback(&name, std::str::from_utf8(&error_log)?)?.1.contains("Cannot allocate memory") {
                                    fs::remove_dir_all(seed_dir).await.missing_ok()?;
                                    if let Some(new_workers) = NEVec::try_from_vec(new_workers) {
                                        *worker_names = new_workers;
                                    } else {
                                        seed_states[usize::from(seed_idx)] = SeedState::Pending;
                                    }
                                } else {
                                    fs::create_dir_all(&seed_dir).await?;
                                    let stats_error_log_path = seed_dir.join("error.log");
                                    fs::write(stats_error_log_path, &error_log).await?;
                                    if let Some(rsl_plando) = rsl_plando {
                                        match rsl_plando {
                                            Either::Left(rsl_plando_path) => {
                                                let stats_rsl_plando_path = seed_dir.join("random_settings.json");
                                                let is_same_drive = {
                                                    #[cfg(windows)] {
                                                        rsl_plando_path.components().find_map(|component| if let std::path::Component::Prefix(prefix) = component { Some(prefix) } else { None })
                                                        == stats_rsl_plando_path.components().find_map(|component| if let std::path::Component::Prefix(prefix) = component { Some(prefix) } else { None })
                                                    }
                                                    #[cfg(not(windows))] { true }
                                                };
                                                if is_same_drive {
                                                    fs::rename(rsl_plando_path, stats_rsl_plando_path).await?;
                                                } else {
                                                    fs::copy(&rsl_plando_path, stats_rsl_plando_path).await?;
                                                    fs::remove_file(rsl_plando_path).await?;
                                                }
                                            }
                                            Either::Right(rsl_plando) => {
                                                let stats_rsl_plando_path = seed_dir.join("random_settings.json");
                                                fs::write(stats_rsl_plando_path, rsl_plando).await?;
                                            }
                                        }
                                    }
                                    fs::write_json(seed_dir.join("metadata.json"), Metadata {
                                        instructions: Some(instructions.as_ref().copied().map_err(|stderr| String::from_utf8_lossy(stderr).into_owned())),
                                        rsl_instructions: Some(rsl_instructions.as_ref().copied().map_err(|stderr| String::from_utf8_lossy(stderr).into_owned())),
                                        worker: name.clone(),
                                    }).await?;
                                    if_chain! {
                                        if !cancelled;
                                        if is_bench;
                                        if let Some(ref stderr) = instructions.as_ref().err().or_else(|| rsl_instructions.as_ref().err());
                                        then {
                                            // perf sometimes doesn't output instruction count for whatever reason, retry if this happens
                                            log!("worker {name} retrying seed {seed_idx} due to missing instruction count, stderr:");
                                            log!("{}", String::from_utf8_lossy(stderr));
                                            fs::remove_dir_all(seed_dir).await?;
                                            if let Some(new_workers) = NEVec::try_from_vec(new_workers) {
                                                *worker_names = new_workers;
                                            } else {
                                                seed_states[usize::from(seed_idx)] = SeedState::Pending;
                                            }
                                        } else {
                                            // cancel remaining raced copies of this seed
                                            for name in new_workers {
                                                if let Some(worker) = workers.iter().find(|worker| worker.name == name) {
                                                    if let Some(tx) = &worker.supervisor_tx {
                                                        let _ = tx.send(ootrstats::worker::SupervisorMessage::Cancel(seed_idx)).await;
                                                    }
                                                } else {
                                                    return Err(Error::WorkerNotFound)
                                                }
                                            }
                                            seed_states[usize::from(seed_idx)] = SeedState::Failure {
                                                completed_at: Some(Instant::now()),
                                                worker: name,
                                                instructions: instructions.as_ref().ok().copied(),
                                                rsl_instructions: rsl_instructions.as_ref().ok().copied(),
                                                error_log,
                                            };
                                        }
                                    }
                                }
                            } else {
                                // seed was already rolled but this worker's instance of this seed didn't get cancelled in time so we just ignore it
                            },
                        }
                    } else {
                        return Err(Error::WorkerNotFound)
                    },
                    Event::End => break,
                };
                let pending_seeds = seed_states.iter().enumerate().filter(|(_, state)| matches!(state, SeedState::Pending) || args.race && matches!(state, SeedState::Rolling { .. })).map(|(seed_idx, _)| seed_idx as SeedIdx).collect::<HashSet<_>>();
                if !pending_seeds.is_empty() {
                    if let Some(worker_tx) = &worker_tx {
                        for worker in &mut workers {
                            if worker.supervisor_tx.is_none() && !worker.stopped && pending_seeds.iter().any(|seed_idx| allowed_workers.get(seed_idx).is_none_or(|allowed_workers| allowed_workers.contains(&worker.name))) {
                                let worker::Config { name, kind, .. } = config.workers.iter().find(|config| config.name == worker.name).expect("unconfigured worker");
                                worker_tasks.push(worker.connect(worker_tx.clone(), kind.clone(), rando_rev, &setup, if let Some(Subcommand::Bench { uncompressed, .. }) = args.subcommand {
                                    if args.patch { unimplemented!("The `bench` subcommand currently cannot generate patch files") }
                                    OutputMode::Bench { uncompressed }
                                } else {
                                    OutputMode::Normal {
                                        patch: args.patch,
                                        uncompressed_rom: args.uncompressed_rom,
                                        compressed_rom: args.rom,
                                    }
                                }).map(move |res| (name.clone(), res)));
                            }
                        }
                    }
                }
                'outer: for worker in &mut workers {
                    while worker.error.is_none() && worker.ready > 0 {
                        if let Some((seed_idx, _)) = seed_states.iter().enumerate().find(|(seed_idx, state)| matches!(state, SeedState::Pending) && allowed_workers.get(&(*seed_idx as SeedIdx)).is_none_or(|allowed_workers| allowed_workers.contains(&worker.name))) {
                            if let Err(mpsc::error::SendError(message)) = worker.roll(&mut seed_states, seed_idx.try_into()?).await {
                                worker.error.get_or_insert(worker::Error::Receive { message });
                                cancel!(workers);
                                break 'outer
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
                                    break 'outer
                                }
                            } else {
                                break
                            }
                        } else {
                            break
                        }
                    }
                }
            },
            //TODO use signal-hook-tokio crate to handle interrupts on Unix?
            Some(res) = cli_rx.recv() => {
                log!("received CLI event: {res:?}"); // debugging C/D keys sometimes not being registered
                if let crossterm::event::Event::Key(KeyEvent { code: KeyCode::Char('c' | 'd'), kind: KeyEventKind::Press, .. }) = res.at_unknown()? {
                    cancelled_by_user = true;
                    cancel!(workers);
                }
            }
        }
        Message::Status {
            retry_failures: args.retry_failures,
            seed_states: &seed_states,
            allowed_workers: &allowed_workers,
            workers: &workers,
            label, available_parallelism, completed_readers, start, start_local,
        }.print(args.json_messages, &mut stderr)?;
        if completed_readers == available_parallelism.get() && seed_states.iter().all(|state| match state {
            SeedState::Cancelled | SeedState::Success { .. } | SeedState::Failure { .. } => true,
            SeedState::Unchecked | SeedState::Pending | SeedState::Rolling { .. } => false,
        }) {
            for worker in &mut workers {
                // drop sender so the worker can shut down
                worker.supervisor_tx = None;
                worker.stopping = true;
            }
            // make sure worker_tx is dropped to prevent deadlock
            worker_tx = None;
        }
    }
    drop(cli_rx);
    Message::Done { label, num_workers: workers.len() as u16, stats_dir }.print(args.json_messages, &mut stderr)?;
    match args.subcommand {
        None => {}
        Some(Subcommand::Bench { raw_data: false, uncompressed: _ }) => {
            let mut num_successes = 0u16;
            let mut num_failures = 0u16;
            let mut instructions_success = 0u64;
            let mut instructions_failure = 0u64;
            let mut rsl_instructions_success = 0u64;
            let mut rsl_instructions_failure = 0u64;
            for state in seed_states {
                match state {
                    SeedState::Unchecked | SeedState::Pending | SeedState::Rolling { .. } => unreachable!(),
                    SeedState::Cancelled | SeedState::Success { instructions: None, .. } | SeedState::Failure { instructions: None, .. } => {}
                    SeedState::Success { instructions: Some(instructions), rsl_instructions, .. } => {
                        num_successes += 1;
                        instructions_success += instructions;
                        rsl_instructions_success += rsl_instructions.unwrap_or_default();
                    }
                    SeedState::Failure { instructions: Some(instructions), rsl_instructions, .. } => {
                        num_failures += 1;
                        instructions_failure += instructions;
                        rsl_instructions_failure += rsl_instructions.unwrap_or_default();
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
                Message::Instructions { rsl: false, num_successes, num_failures, success_rate, average_instructions_success, average_instructions_failure, average_failure_count, average_instructions }.print(args.json_messages, &mut stdout)?;
                if rsl_instructions_success + rsl_instructions_failure > 0 {
                    let average_instructions_success = rsl_instructions_success / u64::try_from(num_successes).unwrap();
                    let average_instructions_failure = rsl_instructions_failure.checked_div(u64::try_from(num_failures).unwrap()).unwrap_or_default();
                    let average_instructions = average_failure_count * average_instructions_failure as f64 + average_instructions_success as f64;
                    Message::Instructions { rsl: true, num_successes, num_failures, success_rate, average_instructions_success, average_instructions_failure, average_failure_count, average_instructions }.print(args.json_messages, &mut stdout)?;
                }
            }
        }
        Some(Subcommand::Bench { raw_data: true, uncompressed: _ }) => {
            for state in seed_states {
                match state {
                    SeedState::Unchecked | SeedState::Pending | SeedState::Rolling { .. } => unreachable!(),
                    SeedState::Cancelled | SeedState::Success { instructions: None, .. } | SeedState::Failure { instructions: None, .. } => {}
                    SeedState::Success { worker, instructions: Some(instructions), rsl_instructions, .. } => {
                        crossterm::execute!(stdout,
                            Print(format_args!("s {instructions} {worker}\r\n")),
                        ).at_unknown()?;
                        if let Some(rsl_instructions) = rsl_instructions {
                            crossterm::execute!(stdout,
                                Print(format_args!("S {rsl_instructions} {worker}\r\n")),
                            ).at_unknown()?;
                        }
                    }
                    SeedState::Failure { worker, instructions: Some(instructions), rsl_instructions, .. } => {
                        crossterm::execute!(stdout,
                            Print(format_args!("f {instructions} {worker}\r\n")),
                        ).at_unknown()?;
                        if let Some(rsl_instructions) = rsl_instructions {
                            crossterm::execute!(stdout,
                                Print(format_args!("F {rsl_instructions} {worker}\r\n")),
                            ).at_unknown()?;
                        }
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
                if let SeedState::Failure { worker, error_log, .. } = state {
                    let (location, msg) = parse_traceback(worker, std::str::from_utf8(error_log)?)?;
                    match counts.entry(location).or_default().entry(msg) {
                        hash_map::Entry::Occupied(mut entry) => entry.get_mut().1 += 1,
                        hash_map::Entry::Vacant(entry) => { entry.insert((seed_idx.try_into()?, 1)); }
                    }
                }
            }
            Message::FailuresHeader { failures: counts.values().map(|msgs| msgs.values().map(|&(_, count)| count as u16).sum::<u16>()).sum() }.print(args.json_messages, &mut stdout)?;
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
    let worker_errors = workers.into_iter()
        .filter_map(|worker| Some((worker.name, worker.error?)))
        .collect_vec();
    if !worker_errors.is_empty() {
        log!("worker errors:");
        for (worker, source) in &worker_errors {
            log!("{} in worker {worker}: {source}", if source.is_network_error() { "network error" } else { "error" });
            log!("debug info: {source:?}");
        }
        return Err(Error::Worker { worker_errors, cancelled: cancelled_by_user })
    }
    Ok(cancelled_by_user)
}

#[wheel::main(custom_exit)]
async fn main(args: Args) -> Result<(), Error> {
    if !args.json_messages {
        enable_raw_mode().at_unknown()?;
    }
    let res = 'res: {
        if args.suite {
            let mut first_network_error = None;
            let mut any_cancelled = false;
            for (label, args) in [
                ("Default / Beginner", args.clone()),
                ("Tournament", Args { preset: Some(format!("tournament")), ..args.clone() }),
                ("Multiworld", Args { preset: Some(format!("mw")), ..args.clone() }),
                ("Hell Mode", Args { preset: Some(format!("hell")), ..args.clone() }),
                ("Random Settings", if args.github_user == "fenhl" {
                    Args { rsl: true, branch: args.branch.is_some_and(|branch| branch == "riir").then(|| format!("riir2")), preset: Some(format!("fenhl")), ..args }
                } else {
                    Args { rsl: true, github_user: format!("fenhl"), branch: Some(format!("dev-mvp")), ..args }
                }), //TODO check to make sure plando-random-settings branch is up to date with matthewkirby:master and the randomizer commit specified in rslversion.py is equal to the specified randomizer commit
            ] {
                match cli(Some(label), args).await {
                    Ok(cancelled) => if cancelled {
                        any_cancelled = true;
                        break
                    },
                    Err(e) => if e.is_network_error() {
                        if let Error::Worker { cancelled: true, .. } = e {
                            any_cancelled = true;
                            break
                        }
                        first_network_error.get_or_insert(e);
                    } else {
                        break 'res Err(e)
                    },
                }
            }
            if let Some(e) = first_network_error {
                Err(e)
            } else {
                Ok(any_cancelled)
            }
        } else {
            cli(None, args).await
        }
    };
    disable_raw_mode().at_unknown()?;
    match res {
        Ok(false) => Ok(()),
        Ok(true) => Err(Error::Cancelled),
        Err(e) => Err(e),
    }
}
