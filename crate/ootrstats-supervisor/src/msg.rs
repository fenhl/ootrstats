use {
    std::{
        borrow::Cow,
        io::prelude::*,
        path::PathBuf,
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
            Clear,
            ClearType,
        },
    },
    if_chain::if_chain,
    serde::Serialize,
    serde_json::Value as Json,
    tokio::time::Instant,
    wheel::traits::IoResultExt as _,
    ootrstats::SeedIdx,
    crate::{
        Error,
        SeedState,
        worker,
    },
};

#[derive(Serialize)]
pub(crate) enum Message<'a> {
    Preparing,
    Status {
        available_parallelism: u16,
        completed_readers: u16,
        retry_failures: bool,
        seed_states: &'a [SeedState],
        #[serde(skip)]
        start: Instant,
        #[serde(skip)]
        start_local: DateTime<Local>,
        workers: Option<&'a [worker::State]>,
    },
    Done {
        stats_dir: PathBuf,
    },
    InstructionsNoSuccesses,
    Instructions {
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
        stats_dir: PathBuf,
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
            match self {
                Self::Preparing => crossterm::execute!(writer,
                    Print("preparing..."),
                ).at_unknown()?,
                Self::Status { available_parallelism, completed_readers, retry_failures, seed_states, start, start_local, workers } => {
                    if let Some(workers) = workers {
                        for worker in workers {
                            if let Some(ref e) = worker.error {
                                let e = e.to_string();
                                if_chain! {
                                    if let Ok((width, _)) = terminal::size();
                                    let mut prefix_end = usize::from(width) - worker.name.len() - 13;
                                    if prefix_end + 3 < e.len();
                                    then {
                                        while !e.is_char_boundary(prefix_end) {
                                            prefix_end -= 1;
                                        }
                                        crossterm::execute!(writer,
                                            Print(format_args!("\r\n{}: error: {}[…]", worker.name, &e[..prefix_end])),
                                            Clear(ClearType::UntilNewLine),
                                        ).at_unknown()?;
                                    } else {
                                        crossterm::execute!(writer,
                                            Print(format_args!("\r\n{}: error: {e}", worker.name)),
                                            Clear(ClearType::UntilNewLine),
                                        ).at_unknown()?;
                                    }
                                }
                            } else {
                                let mut running = 0u16;
                                let mut completed = 0u16;
                                let mut total_completed = 0u16;
                                for state in seed_states {
                                    match state {
                                        SeedState::Success { worker: Some(name), .. } | SeedState::Failure { worker: Some(name), .. } => {
                                            total_completed += 1;
                                            if *name == worker.name { completed += 1 }
                                        }
                                        SeedState::Rolling { workers } => running += u16::try_from(workers.iter().into_iter().filter(|name| **name == worker.name).count())?,
                                        | SeedState::Unchecked
                                        | SeedState::Pending
                                        | SeedState::Success { worker: None, .. }
                                        | SeedState::Failure { worker: None, .. }
                                        | SeedState::Cancelled
                                            => {}
                                    }
                                }
                                let state = if worker.stopped {
                                    Cow::Borrowed("done")
                                } else if let Some(ref msg) = worker.msg {
                                    if running > 0 {
                                        Cow::Owned(format!("{running} running, {msg}"))
                                    } else {
                                        Cow::Borrowed(&**msg)
                                    }
                                } else {
                                    Cow::Owned(format!("{running} running"))
                                };
                                if total_completed > 0 {
                                    crossterm::execute!(writer,
                                        Print(format_args!(
                                            "\r\n{}: {completed} rolled ({}%), {state}",
                                            worker.name,
                                            100 * u32::from(completed) / u32::from(total_completed),
                                        )),
                                        Clear(ClearType::UntilNewLine),
                                    ).at_unknown()?;
                                } else {
                                    crossterm::execute!(writer,
                                        Print(format_args!(
                                            "\r\n{}: 0 rolled, {state}",
                                            worker.name,
                                        )),
                                        Clear(ClearType::UntilNewLine),
                                    ).at_unknown()?;
                                }
                            }
                        }
                        crossterm::execute!(writer,
                            MoveUp(workers.len() as u16),
                        ).at_unknown()?;
                    }
                    crossterm::execute!(writer,
                        MoveToColumn(0),
                        Print(if completed_readers == available_parallelism {
                            // list of pending seeds fully initialized
                            let mut num_successes = 0u16;
                            let mut num_failures = 0u16;
                            let mut started = 0u16;
                            let mut total = 0u16;
                            let mut completed = 0u16;
                            let mut skipped = 0u16;
                            for state in seed_states {
                                match state {
                                    SeedState::Unchecked => unreachable!(),
                                    SeedState::Pending => total += 1,
                                    SeedState::Rolling { .. } => {
                                        total += 1;
                                        started += 1;
                                    }
                                    SeedState::Cancelled => {}
                                    SeedState::Success { worker, .. } => {
                                        total += 1;
                                        started += 1;
                                        num_successes += 1;
                                        if worker.is_some() { completed += 1 } else { skipped += 1 }
                                    }
                                    SeedState::Failure { worker, .. } => {
                                        total += 1;
                                        started += 1;
                                        num_failures += 1;
                                        if worker.is_some() { completed += 1 } else { skipped += 1 }
                                    }
                                }
                            }
                            let rolled = num_successes + num_failures;
                            format!(
                                "{started}/{total} seeds started, {rolled} rolled{}, ETA {}",
                                if retry_failures {
                                    String::default()
                                } else {
                                    format!(
                                        ", {num_failures} failure{} ({}%)",
                                        if num_failures == 1 { "" } else { "s" },
                                        if num_successes > 0 || num_failures > 0 { 100 * u32::from(num_failures) / u32::from(num_successes + num_failures) } else { 100 },
                                    )
                                },
                                if_chain! {
                                    if completed > 0;
                                    let ratio = (total - skipped) as f64 / completed as f64;
                                    if let Ok(estimated_duration) = Duration::try_from_secs_f64(start.elapsed().as_secs_f64() * ratio);
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
                            let summary = format!("checking for existing seeds: {rolled} rolled, {started} running, {pending} pending, {unchecked} still being checked");
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
                Self::Done { stats_dir } => crossterm::execute!(writer,
                    Print(format_args!("stats saved to {}\r\n", stats_dir.display())),
                ).at_unknown()?,
                Self::InstructionsNoSuccesses => crossterm::execute!(writer,
                    Print("No successful seeds, so average instruction count is infinite\r\n"),
                ).at_unknown()?,
                Self::Instructions { num_successes, num_failures, success_rate, average_instructions_success, average_instructions_failure, average_failure_count: _, average_instructions } => crossterm::execute!(writer,
                    Print(format_args!("success rate: {num_successes}/{} ({:.02}%)\r\n", num_successes + num_failures, success_rate * 100.0)),
                    Print(format_args!("average instructions (success): {average_instructions_success} ({average_instructions_success:.3e})\r\n")),
                    Print(format_args!("average instructions (failure): {}\r\n", if num_failures == 0 { format!("N/A") } else { format!("{average_instructions_failure} ({average_instructions_failure:.3e})") })),
                    Print(format_args!("average total instructions until success: {average_instructions} ({average_instructions:.3e})\r\n")),
                ).at_unknown()?,
                Self::Category { count, output } => crossterm::execute!(writer,
                    Print(format_args!("{count}x: {output}\r\n")),
                ).at_unknown()?,
                Self::FailuresHeader { stats_dir } => crossterm::execute!(writer,
                    Print(format_args!("Output directory: {}\r\n", stats_dir.display())),
                    Print("Top failure reasons by last line:\r\n"),
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
        }
        Ok(())
    }
}
