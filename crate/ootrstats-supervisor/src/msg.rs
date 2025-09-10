use {
    std::{
        borrow::Cow,
        collections::HashMap,
        io::prelude::*,
        num::NonZero,
        path::PathBuf,
        sync::Arc,
        time::Duration,
    },
    chrono::{
        prelude::*,
        TimeDelta,
    },
    crossterm::{
        cursor::{
            MoveToColumn,
            MoveUp,
        },
        style::Print,
        terminal::{
            self,
            BeginSynchronizedUpdate,
            Clear,
            ClearType,
            EndSynchronizedUpdate,
        },
    },
    if_chain::if_chain,
    itertools::Itertools as _,
    nonempty_collections::NEVec,
    serde::Serialize,
    serde_json::Value as Json,
    tokio::time::Instant,
    wheel::traits::{
        IoResultExt as _,
        IsNetworkError as _,
    },
    ootrstats::SeedIdx,
    crate::{
        Error,
        SeedState,
        worker,
    },
};

#[derive(Serialize)]
pub(crate) enum Message<'a> {
    Preparing(Option<&'static str>),
    Status {
        label: Option<&'static str>,
        available_parallelism: NonZero<u16>,
        completed_readers: u16,
        retry_failures: bool,
        world_counts: bool,
        seed_states: &'a [SeedState],
        allowed_workers: &'a HashMap<SeedIdx, NEVec<Arc<str>>>,
        retried_failures: &'a [u32],
        #[serde(skip)]
        start: Instant,
        #[serde(skip)]
        start_local: DateTime<Local>,
        workers: &'a [worker::State],
    },
    Done {
        label: Option<&'static str>,
        num_workers: u16,
        stats_dir: PathBuf,
    },
    InstructionsNoSuccesses,
    Instructions {
        rsl: bool,
        num_successes: u16,
        num_failures: u16,
        success_rate: f64,
        average_instructions_success: u64,
        average_instructions_failure: u64,
        average_failure_count: f64,
        average_instructions: f64,
    },
    Category {
        count: usize,
        output: Json,
    },
    FailuresHeader {
        failures: u16,
    },
    Failure {
        count: usize,
        top_msg: &'a str,
        top_count: usize,
        seed_idx: SeedIdx,
        msgs: Vec<(&'a str, (SeedIdx, usize))>,
    },
}

impl Message<'_> {
    pub(crate) fn print(self, json: bool, writer: &mut impl Write) -> Result<(), Error> {
        if json {
            serde_json::to_writer(writer, &self)?;
            eprintln!();
        } else {
            crossterm::execute!(writer,
                BeginSynchronizedUpdate,
            ).at_unknown()?;
            match self {
                Self::Preparing(None) => crossterm::execute!(writer,
                    Print("preparing..."),
                ).at_unknown()?,
                Self::Preparing(Some(label)) => crossterm::execute!(writer,
                    Print(format_args!("{label}: preparing...")),
                ).at_unknown()?,
                Self::Status { label, available_parallelism, completed_readers, retry_failures, world_counts, seed_states, allowed_workers, retried_failures, start, start_local, workers } => {
                    let all_assigned = seed_states.iter()
                        .enumerate()
                        .all(|(seed_idx, seed_state)| matches!(seed_state, SeedState::Unchecked) || allowed_workers.get(&(seed_idx as SeedIdx)).is_some_and(|assigned_workers| assigned_workers.len() == NonZero::<usize>::MIN));
                    for worker in workers {
                        if let Some(ref e) = worker.error {
                            let kind = if e.is_network_error() { "network error" } else { "error" };
                            let e = e.to_string();
                            if_chain! {
                                if let Ok((width, _)) = terminal::size();
                                let mut prefix_end = e.len().min(usize::from(width) - worker.name.len() - kind.len() - 8);
                                if prefix_end + 3 < e.len() || e.contains('\n');
                                then {
                                    if let Some(idx) = e[..prefix_end].find('\n') {
                                        prefix_end = idx;
                                    } else {
                                        while !e.is_char_boundary(prefix_end) {
                                            prefix_end -= 1;
                                        }
                                    }
                                    crossterm::execute!(writer,
                                        Print(format_args!("\r\n{}: {kind}: {}[…]", worker.name, &e[..prefix_end])),
                                        Clear(ClearType::UntilNewLine),
                                    ).at_unknown()?;
                                } else {
                                    crossterm::execute!(writer,
                                        Print(format_args!("\r\n{}: {kind}: {e}", worker.name)),
                                        Clear(ClearType::UntilNewLine),
                                    ).at_unknown()?;
                                }
                            }
                        } else {
                            let mut running = Vec::default();
                            let mut completed = 0u16;
                            let mut total_completed = 0u16;
                            let mut failures = 0u16;
                            let mut assigned = 0u16;
                            for (seed_idx, state) in seed_states.into_iter().enumerate() {
                                match state {
                                    SeedState::Success { worker: name, .. } => {
                                        total_completed += 1;
                                        if *name == worker.name { completed += 1 }
                                    }
                                    SeedState::Failure { worker: name, .. } => {
                                        total_completed += 1;
                                        if *name == worker.name {
                                            completed += 1;
                                            failures += 1;
                                        }
                                    }
                                    SeedState::Rolling { workers } => running.extend(workers.iter().into_iter().filter(|name| **name == worker.name).map(|_| seed_idx)),
                                    | SeedState::Unchecked
                                    | SeedState::Pending
                                    | SeedState::Cancelled
                                        => {}
                                }
                                if let Some(assigned_workers) = allowed_workers.get(&(seed_idx as SeedIdx)) {
                                    if assigned_workers.len() == NonZero::<usize>::MIN {
                                        if *assigned_workers.first() == worker.name { assigned += 1 }
                                    }
                                }
                            }
                            let state = if worker.stopped {
                                Cow::Borrowed("done")
                            } else if worker.stopping {
                                if let Some(ref msg) = worker.msg {
                                    Cow::Owned(format!("stopping, {msg}"))
                                } else {
                                    Cow::Borrowed("stopping")
                                }
                            } else if worker.supervisor_tx.is_none() {
                                Cow::Borrowed("not started")
                            } else if let Some(ref msg) = worker.msg {
                                if running.is_empty() {
                                    Cow::Borrowed(&**msg)
                                } else {
                                    Cow::Owned(format!("{} running, {msg}", running.len()))
                                }
                            } else {
                                if running.is_empty() {
                                    Cow::Borrowed("0 running")
                                } else {
                                    if world_counts {
                                        Cow::Owned(format!("{} running: {}", running.len(), running.into_iter().map(|seed_idx| seed_idx + 1).format(", ")))
                                    } else {
                                        Cow::Owned(format!("{} running", running.len()))
                                    }
                                }
                            };
                            crossterm::execute!(writer,
                                Print(format_args!(
                                    "\r\n{}: {completed}{}{}, {state}",
                                    worker.name,
                                    if all_assigned {
                                        format!("/{assigned} rolled")
                                    } else if total_completed > 0 {
                                        format!(" rolled ({}%)", 100 * u32::from(completed) / u32::from(total_completed))
                                    } else {
                                        format!(" rolled")
                                    },
                                    if failures > 0 { format!(", failure rate {}%", 100 * u32::from(failures) / u32::from(completed)) } else { String::default() },
                                )),
                                Clear(ClearType::UntilNewLine),
                            ).at_unknown()?;
                        }
                    }
                    crossterm::execute!(writer,
                        MoveUp(workers.len() as u16),
                        MoveToColumn(0),
                        Print(if completed_readers == available_parallelism.get() {
                            // list of pending seeds fully initialized
                            let mut num_successes = 0u16;
                            let mut num_failures = 0u16;
                            let mut started = 0u16;
                            let mut total = 0u16;
                            let mut completed = 0u16;
                            let mut last_completed = None;
                            let mut skipped = 0u16;
                            let mut continuous = 0;
                            for (seed_idx, state) in seed_states.into_iter().enumerate() {
                                match *state {
                                    SeedState::Unchecked => unreachable!(),
                                    SeedState::Pending => total += 1,
                                    SeedState::Rolling { .. } => {
                                        total += 1;
                                        started += 1;
                                    }
                                    SeedState::Cancelled => {}
                                    SeedState::Success { completed_at, .. } => {
                                        total += 1;
                                        started += 1;
                                        num_successes += 1;
                                        if continuous == seed_idx {
                                            continuous += 1;
                                        }
                                        if let Some(completed_at) = completed_at {
                                            completed += 1;
                                            let last_completed = last_completed.get_or_insert(completed_at);
                                            *last_completed = completed_at.max(*last_completed);
                                        } else {
                                            skipped += 1;
                                        }
                                    }
                                    SeedState::Failure { completed_at, .. } => {
                                        total += 1;
                                        started += 1;
                                        num_failures += 1;
                                        if continuous == seed_idx {
                                            continuous += 1;
                                        }
                                        if let Some(completed_at) = completed_at {
                                            completed += 1;
                                            let last_completed = last_completed.get_or_insert(completed_at);
                                            *last_completed = completed_at.max(*last_completed);
                                        } else {
                                            skipped += 1;
                                        }
                                    }
                                }
                            }
                            let rolled = num_successes + num_failures;
                            format!(
                                "{}{started}/{total} seeds started, {rolled} rolled{}{}, ETA {}",
                                if let Some(label) = label { format!("{label}: ") } else { String::default() },
                                if world_counts {
                                    format!(" (continuous up to {continuous} worlds)")
                                } else {
                                    String::default()
                                },
                                if retry_failures {
                                    let num_failures = retried_failures.iter().sum::<u32>();
                                    format!(
                                        ", {num_failures} failure{} retried",
                                        if num_failures == 1 { "" } else { "s" },
                                    )
                                } else {
                                    format!(
                                        ", {num_failures} failure{} ({}%)",
                                        if num_failures == 1 { "" } else { "s" },
                                        if num_successes > 0 || num_failures > 0 { 100 * u32::from(num_failures) / u32::from(num_successes + num_failures) } else { 100 },
                                    )
                                },
                                if_chain! {
                                    if let Some(estimated_duration) = if all_assigned {
                                        workers.iter()
                                            .map(|worker| {
                                                let mut total = 0u16;
                                                let mut completed = 0u16;
                                                let mut last_completed = None;
                                                for (seed_idx, state) in seed_states.iter().enumerate() {
                                                    if allowed_workers.get(&(seed_idx as SeedIdx)).is_none_or(|allowed_workers| allowed_workers.contains(&worker.name)) {
                                                        match *state {
                                                            SeedState::Unchecked => unreachable!(),
                                                            SeedState::Pending | SeedState::Rolling { .. } => total += 1,
                                                            SeedState::Cancelled | SeedState::Success { completed_at: None, .. } | SeedState::Failure { completed_at: None, .. } => {}
                                                            SeedState::Success { completed_at: Some(completed_at), .. } | SeedState::Failure { completed_at: Some(completed_at), .. } => {
                                                                total += 1;
                                                                completed += 1;
                                                                let last_completed = last_completed.get_or_insert(completed_at);
                                                                *last_completed = completed_at.max(*last_completed);
                                                            }
                                                        }
                                                    }
                                                }
                                                if total == 0 {
                                                    Some(Duration::default())
                                                } else {
                                                    last_completed.map(|last_completed| (last_completed - start) * u32::from(total) / u32::from(completed))
                                                }
                                            })
                                            .collect::<Option<Vec<_>>>()
                                            .and_then(|worker_durations| worker_durations.into_iter().max())
                                    } else {
                                        last_completed.map(|last_completed| (last_completed - start) * u32::from(total - skipped) / u32::from(completed))
                                    };
                                    if let Ok(estimated_duration) = TimeDelta::from_std(estimated_duration);
                                    then {
                                        (start_local + estimated_duration).format("%Y-%m-%d %H:%M:%S").to_string()
                                    } else {
                                        format!("unknown")
                                    }
                                },
                            )
                        } else {
                            let mut rolled = 0u16;
                            let mut started = 0u16;
                            let mut pending = 0u16;
                            let mut unchecked = 0u16;
                            for state in seed_states {
                                match state {
                                    SeedState::Unchecked => unchecked += 1,
                                    SeedState::Pending => pending += 1,
                                    SeedState::Rolling { .. } => started += 1,
                                    SeedState::Cancelled => {}
                                    SeedState::Success { .. } | SeedState::Failure { .. } => rolled += 1,
                                }
                            }
                            let summary = format!(
                                "{}checking for existing seeds: {rolled} rolled, {started} running, {pending} pending, {unchecked} still being checked",
                                if let Some(label) = label { format!("{label}: ") } else { String::default() },
                            );
                            if_chain! {
                                if let Ok((width, _)) = terminal::size();
                                let mut prefix_end = usize::from(width) - 4;
                                if prefix_end + 3 < summary.len();
                                then {
                                    while !summary.is_char_boundary(prefix_end) {
                                        prefix_end -= 1;
                                    }
                                    format!("{}[…]", &summary[..prefix_end])
                                } else {
                                    summary
                                }
                            }
                        }),
                        Clear(ClearType::UntilNewLine),
                    ).at_unknown()?;
                }
                Self::Done { label, num_workers, stats_dir } => {
                    for _ in 0..num_workers {
                        crossterm::execute!(writer,
                            Print("\r\n"),
                            Clear(ClearType::UntilNewLine),
                        ).at_unknown()?;
                    }
                    crossterm::execute!(writer,
                        MoveUp(num_workers),
                        Print(format_args!("{}stats saved to {}", if let Some(label) = label { format!("{label}: ") } else { String::default() }, stats_dir.display())),
                        Clear(ClearType::UntilNewLine),
                        Print("\r\n"),
                    ).at_unknown()?;
                }
                Self::InstructionsNoSuccesses => crossterm::execute!(writer,
                    Print("No successful seeds, so average instruction count is infinite\r\n"),
                ).at_unknown()?,
                Self::Instructions { rsl, num_successes, num_failures, success_rate, average_instructions_success, average_instructions_failure, average_failure_count: _, average_instructions } => crossterm::execute!(writer,
                    Print(format_args!("success rate{}: {num_successes}/{} ({:.02}%)\r\n", if rsl { " (RSL script)" } else { "" }, num_successes + num_failures, success_rate * 100.0)),
                    Print(format_args!("average instructions (success){}: {average_instructions_success} ({average_instructions_success:.3e})\r\n", if rsl { " (RSL script)" } else { "" })),
                    Print(format_args!("average instructions (failure){}: {}\r\n", if rsl { " (RSL script)" } else { "" }, if num_failures == 0 { format!("N/A") } else { format!("{average_instructions_failure} ({average_instructions_failure:.3e})") })),
                    Print(format_args!("average total instructions until success{}: {average_instructions} ({average_instructions:.3e})\r\n", if rsl { " (RSL script)" } else { "" })),
                ).at_unknown()?,
                Self::Category { count, output } => crossterm::execute!(writer,
                    Print(format_args!("{count}x: {output}\r\n")),
                ).at_unknown()?,
                Self::FailuresHeader { failures } => crossterm::execute!(writer,
                    Print(format_args!("{failures} failures, top failure reasons by last line:\r\n")),
                ).at_unknown()?,
                Self::Failure { count, top_msg, top_count, seed_idx, msgs } => if msgs.is_empty() {
                    crossterm::execute!(writer,
                        Print(format_args!("{count}x: {top_msg} (e.g. seed {seed_idx})\r\n")),
                    ).at_unknown()?;
                } else {
                    crossterm::execute!(writer,
                        Print(format_args!("{count}x: {top_msg} ({top_count}x, e.g. seed {seed_idx}, and {} other variants)\r\n", msgs.len())),
                    ).at_unknown()?;
                },
            }
            crossterm::execute!(writer,
                EndSynchronizedUpdate,
            ).at_unknown()?;
        }
        Ok(())
    }
}
