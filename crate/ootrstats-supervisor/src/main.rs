use {
    std::{
        collections::VecDeque,
        io::stderr,
        mem,
        num::NonZeroUsize,
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
        },
    },
    futures::stream::{
        FuturesUnordered,
        StreamExt as _,
    },
    git2::Repository,
    if_chain::if_chain,
    itertools::Itertools as _,
    lazy_regex::regex_is_match,
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
    ootrstats::RandoSettings,
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

type SeedIdx = u16;

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

#[derive(clap::Parser)]
#[clap(version)]
struct Args {
    /// Sample size — how many seeds to roll.
    #[clap(short, long, default_value_t = 16384)]
    num_seeds: SeedIdx,
    #[clap(short, long)]
    preset: Option<String>,
    #[clap(subcommand)]
    subcommand: Subcommand,
}

#[derive(clap::Subcommand)]
enum Subcommand {
    /// Benchmark — measure average CPU instructions to generate a seed.
    Bench,
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)] Config(#[from] config::Error),
    #[error(transparent)] Git(#[from] git2::Error),
    #[error(transparent)] Json(#[from] serde_json::Error),
    #[error(transparent)] Task(#[from] JoinError),
    #[error(transparent)] ReaderSend(#[from] mpsc::error::SendError<ReaderMessage>),
    #[error(transparent)] Worker(#[from] worker::Error),
    #[error(transparent)] WorkerSend(#[from] mpsc::error::SendError<worker::SupervisorMessage>),
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[cfg(unix)] #[error(transparent)] Xdg(#[from] xdg::BaseDirectoriesError),
    #[cfg(windows)]
    #[error("user folder not found")]
    MissingHomeDir,
    #[error("requested benchmark but worker did not report instructions")]
    MissingInstructions,
    #[error("found both spoiler and error logs for a seed")]
    SuccessAndFailure,
    #[error("received a message from an unknown worker")]
    WorkerNotFound,
}

impl wheel::CustomExit for Error {
    fn exit(self, cmd_name: &'static str) -> ! {
        eprintln!("\r");
        match self {
            Self::Worker(worker::Error::Roll(ootrstats::RollError::PerfSyntax(stderr))) => {
                eprintln!("{cmd_name}: roll error: failed to parse `perf` output\r");
                eprintln!("stderr:\r");
                eprintln!("{}\r", String::from_utf8_lossy(&stderr).lines().filter(|line| !regex_is_match!("^[0-9]+ files remaining$", line)).format("\r\n"));
            }
            _ => {
                eprintln!("{cmd_name}: {self}\r");
                eprintln!("debug info: {self:?}\r");
            }
        }
        std::process::exit(1)
    }
}

#[wheel::main(custom_exit)]
async fn main(args: Args) -> Result<(), Error> {
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
    let rando_rev = {
        #[cfg(windows)] let dir_parent = UserDirs::new().ok_or(Error::MissingHomeDir)?.home_dir().join("git").join("github.com").join("OoTRandomizer").join("OoT-Randomizer");
        #[cfg(unix)] let dir_parent = Path::new("/opt/git/github.com").join("OoTRandomizer").join("OoT-Randomizer"); //TODO respect GITDIR envar and allow ~/git fallback
        let dir = dir_parent.join("main");
        if fs::exists(&dir).await? {
            Command::new("git")
                .arg("pull")
                .current_dir(&dir)
                .check("git pull").await?;
        } else {
            Command::new("git")
                .arg("clone")
                .arg("--depth=1")
                .arg("https://github.com/OoTRandomizer/OoT-Randomizer.git")
                .arg("main")
                .current_dir(dir_parent)
                .check("git clone").await?;
        }
        Repository::open(dir)?.head()?.peel_to_commit()?.id()
    };
    let settings = if let Some(preset) = args.preset {
        RandoSettings::Preset(preset)
    } else {
        RandoSettings::Default
    };
    let stats_dir = {
        #[cfg(windows)] let project_dirs = ProjectDirs::from("net", "Fenhl", "ootrstats").ok_or(Error::MissingHomeDir)?;
        #[cfg(windows)] let stats_root = project_dirs.data_dir();
        #[cfg(unix)] let stats_root = BaseDirectories::new()?.place_config_file("ootrstats").at_unknown()?;
        stats_root.join(rando_rev.to_string()).join(settings.stats_dir())
    };
    let available_parallelism = std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN).get().try_into().unwrap_or(SeedIdx::MAX).min(args.num_seeds);
    let bench = matches!(args.subcommand, Subcommand::Bench);
    let start = Instant::now();
    let start_local = Local::now();
    let mut skipped = 0u16; //TODO remove? (redundant with workers tracking their completed seeds)
    let mut instructions_success = Vec::with_capacity(if bench { args.num_seeds.into() } else { 0 });
    let mut instructions_failure = Vec::with_capacity(if bench { args.num_seeds.into() } else { 0 });
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
                        let instructions = if bench {
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
                        let instructions = if bench {
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
    let mut workers = Err(worker_tx);
    let mut pending_seeds = VecDeque::default();
    loop {
        enum Event {
            ReaderDone(Result<Result<(), Error>, JoinError>),
            ReaderMessage(ReaderMessage),
            WorkerDone(Result<Result<(), worker::Error>, JoinError>),
            WorkerMessage(String, worker::Message),
            End,
        }

        select! {
            event = async {
                select! {
                    Some(res) = readers.next() => Event::ReaderDone(res),
                    Some(msg) = reader_rx.recv() => Event::ReaderMessage(msg),
                    Some(res) = worker_tasks.next() => Event::WorkerDone(res),
                    Some((name, msg)) = worker_rx.recv() => Event::WorkerMessage(name, msg),
                    else => Event::End,
                }
            } => match event {
                Event::ReaderDone(res) => { let () = res??; }
                Event::ReaderMessage(msg) => {
                    let seed_idx = match msg {
                        ReaderMessage::Pending(seed_idx) => Some(seed_idx),
                        ReaderMessage::Success { seed_idx, instructions } => match args.subcommand {
                            Subcommand::Bench => if let Some(instructions) = instructions {
                                skipped += 1;
                                instructions_success.push(instructions);
                                None
                            } else {
                                // seed was already rolled but not benchmarked, roll a new seed instead
                                fs::remove_dir_all(stats_dir.join(seed_idx.to_string())).await?;
                                Some(seed_idx)
                            },
                        },
                        ReaderMessage::Failure { seed_idx, instructions } => match args.subcommand {
                            Subcommand::Bench => if let Some(instructions) = instructions {
                                skipped += 1;
                                instructions_failure.push(instructions);
                                None
                            } else {
                                // seed was already rolled but not benchmarked, roll a new seed instead
                                fs::remove_dir_all(stats_dir.join(seed_idx.to_string())).await?;
                                Some(seed_idx)
                            },
                        },
                        ReaderMessage::Done => {
                            completed_readers += 1;
                            None
                        }
                    };
                    if let Some(seed_idx) = seed_idx {
                        let workers = match workers {
                            Ok(ref mut workers) => workers,
                            Err(worker_tx) => {
                                let (new_worker_tasks, new_workers) = mem::take(&mut config.workers).into_iter()
                                    .map(|worker::Config { name, kind }| worker::State::new(worker_tx.clone(), name, kind, rando_rev, &settings, bench))
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
                }
                Event::WorkerDone(res) => { let () = res??; }
                Event::WorkerMessage(name, msg) => if_chain! {
                    if let Ok(ref mut workers) = workers;
                    if let Some(worker) = workers.iter_mut().find(|worker| worker.name == name);
                    then {
                        match msg {
                            worker::Message::Init(msg) => worker.msg = Some(msg),
                            worker::Message::Ready(ready) => {
                                worker.ready = ready;
                                while worker.ready > 0 {
                                    worker.msg = None;
                                    let Some(seed_idx) = pending_seeds.pop_front() else { break };
                                    worker.roll(seed_idx).await?;
                                }
                            }
                            worker::Message::LocalSuccess { seed_idx, instructions, spoiler_log_path, ready } => {
                                worker.running -= 1;
                                worker.completed += 1;
                                if ready {
                                    worker.ready += 1;
                                    worker.msg = None;
                                    if let Some(seed_idx) = pending_seeds.pop_front() {
                                        worker.roll(seed_idx).await?;
                                    }
                                }
                                let seed_dir = stats_dir.join(seed_idx.to_string());
                                fs::create_dir_all(&seed_dir).await?;
                                let stats_spoiler_log_path = seed_dir.join("spoiler.json");
                                fs::rename(spoiler_log_path, &stats_spoiler_log_path).await?;
                                fs::write(seed_dir.join("metadata.json"), serde_json::to_vec_pretty(&Metadata {
                                    instructions,
                                })?).await?;
                                match args.subcommand {
                                    Subcommand::Bench => instructions_success.push(instructions.ok_or(Error::MissingInstructions)?),
                                }
                            }
                            worker::Message::Failure { seed_idx, instructions, error_log, ready } => {
                                worker.running -= 1;
                                worker.completed += 1;
                                if ready {
                                    worker.ready += 1;
                                    worker.msg = None;
                                    if let Some(seed_idx) = pending_seeds.pop_front() {
                                        worker.roll(seed_idx).await?;
                                    }
                                }
                                let seed_dir = stats_dir.join(seed_idx.to_string());
                                fs::create_dir_all(&seed_dir).await?;
                                let stats_error_log_path = seed_dir.join("error.log");
                                fs::write(stats_error_log_path, &error_log).await?;
                                fs::write(seed_dir.join("metadata.json"), serde_json::to_vec_pretty(&Metadata {
                                    instructions,
                                })?).await?;
                                match args.subcommand {
                                    Subcommand::Bench => instructions_failure.push(instructions.ok_or(Error::MissingInstructions)?),
                                }
                            }
                        }
                    } else {
                        return Err(Error::WorkerNotFound)
                    }
                },
                Event::End => break,
            },
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
                if let Some(ref msg) = worker.msg {
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
                match args.subcommand {
                    Subcommand::Bench => {
                        let rolled = instructions_success.len() + instructions_failure.len();
                        let started = rolled + workers.as_ref().map(|workers| workers.iter().map(|worker| usize::from(worker.running)).sum::<usize>()).unwrap_or_default();
                        let total = started + pending_seeds.len();
                        format!(
                            "{started}/{total} seeds started, {rolled} rolled, {} failures ({}%), ETA {}",
                            instructions_failure.len(),
                            if !instructions_success.is_empty() || !instructions_failure.is_empty() { 100 * instructions_failure.len() / (instructions_success.len() + instructions_failure.len()) } else { 100 },
                            if instructions_success.len() + instructions_failure.len() > skipped.into() { (start_local + TimeDelta::from_std(start.elapsed().mul_f64((args.num_seeds - skipped) as f64 / (instructions_success.len() + instructions_failure.len() - usize::from(skipped)) as f64)).expect("ETA too long")).format("%Y-%m-%d %H:%M:%S").to_string() } else { format!("unknown") },
                        )
                    }
                }
            } else {
                match args.subcommand {
                    Subcommand::Bench => {
                        let rolled = instructions_success.len() + instructions_failure.len();
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
    match args.subcommand {
        Subcommand::Bench => if instructions_success.is_empty() {
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
                Print(format_args!("average instructions (success): {average_instructions_success}\r\n")),
                Print(format_args!("average instructions (failure): {}\r\n", if instructions_failure.is_empty() { format!("N/A") } else { average_instructions_failure.to_string() })),
                Print(format_args!("average total instructions until success: {average_instructions}\r\n")),
            ).at_unknown()?;
        },
    }
    Ok(())
}
