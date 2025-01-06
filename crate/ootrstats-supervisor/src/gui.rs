use {
    std::{
        any::TypeId,
        hash::Hash as _,
        pin::Pin,
        time::Duration,
    },
    chrono::TimeDelta,
    futures::stream::Stream,
    iced::{
        Element,
        Subscription,
        advanced::subscription,
        widget::Text,
    },
    tokio::sync::mpsc,
    tokio_stream::wrappers::ReceiverStream,
    crate::*,
};

struct Runner {
    args: Args,
}

impl subscription::Recipe for Runner {
    type Output = Message;

    fn hash(&self, state: &mut subscription::Hasher) {
        TypeId::of::<Self>().hash(state);
    }

    fn stream(self: Box<Self>, _: subscription::EventStream) -> Pin<Box<dyn Stream<Item = Message> + Send>> {
        let Self { args } = *self;
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(run_inner(None, args, tx)); //TODO handle `--suite`
        //TODO send message when task is done
        Box::pin(ReceiverStream::new(rx))
    }
}

pub(crate) struct State {
    args: Args,
    last_message: Option<Message>,
}

impl State {
    pub(crate) fn new(args: Args) -> Self {
        Self {
            last_message: None,
            args,
        }
    }

    pub(crate) fn subscription(&self) -> Subscription<Message> {
        subscription::from_recipe(Runner { args: self.args.clone() })
    }

    pub(crate) fn update(&mut self, msg: Message) {
        self.last_message = Some(msg);
    }

    pub(crate) fn view(&self) -> Element<'_, Message> {
        match &self.last_message {
            None => Text::new("Initializing…").into(),
            Some(Message::Preparing(None)) => Text::new(format!("Preparing…")).into(),
            Some(Message::Preparing(Some(label))) => Text::new(format!("{label}: Preparing…")).into(),
            Some(Message::Status { label, available_parallelism, completed_readers, retry_failures, seed_states, start, start_local, workers }) => {
                let mut text = String::default();
                if let Some(workers) = workers {
                    for worker in &*workers {
                        if let Some(ref e) = worker.error {
                            text.push_str(&format!("\n{}: error: {e}", worker.name));
                        } else {
                            let mut running = 0u16;
                            let mut completed = 0u16;
                            let mut total_completed = 0u16;
                            let mut failures = 0u16;
                            for state in &*seed_states {
                                match state {
                                    SeedState::Success { worker: Some(name), .. } => {
                                        total_completed += 1;
                                        if *name == worker.name { completed += 1 }
                                    }
                                    SeedState::Failure { worker: Some(name), .. } => {
                                        total_completed += 1;
                                        if *name == worker.name {
                                            completed += 1;
                                            failures += 1;
                                        }
                                    }
                                    SeedState::Rolling { workers } => running += workers.iter().into_iter().filter(|name| **name == worker.name).count() as u16,
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
                                if failures > 0 {
                                    text.push_str(&format!(
                                        "\n{}: {completed} rolled ({}%), failure rate {}%, {state}",
                                        worker.name,
                                        100 * u32::from(completed) / u32::from(total_completed),
                                        100 * u32::from(failures) / u32::from(completed),
                                    ));
                                } else {
                                    text.push_str(&format!(
                                        "\n{}: {completed} rolled ({}%), {state}",
                                        worker.name,
                                        100 * u32::from(completed) / u32::from(total_completed),
                                    ));
                                }
                            } else {
                                text.push_str(&format!(
                                    "\r\n{}: 0 rolled, {state}",
                                    worker.name,
                                ));
                            }
                        }
                    }
                }
                Text::new(format!("{}{text}", if completed_readers == available_parallelism {
                    // list of pending seeds fully initialized
                    let mut num_successes = 0u16;
                    let mut num_failures = 0u16;
                    let mut started = 0u16;
                    let mut total = 0u16;
                    let mut completed = 0u16;
                    let mut skipped = 0u16;
                    for state in &*seed_states {
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
                        "{}{started}/{total} seeds started, {rolled} rolled{}, ETA {}",
                        if let Some(label) = label { format!("{label}: ") } else { String::default() },
                        if *retry_failures {
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
                                (*start_local + estimated_duration).format("%Y-%m-%d %H:%M:%S").to_string()
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
                    for state in &*seed_states {
                        match state {
                            SeedState::Unchecked => unchecked += 1,
                            SeedState::Pending => pending += 1,
                            SeedState::Rolling { .. } => started += 1,
                            SeedState::Cancelled => {}
                            SeedState::Success { .. } | SeedState::Failure { .. } => rolled += 1,
                        }
                    }
                    format!(
                        "{}checking for existing seeds: {rolled} rolled, {started} running, {pending} pending, {unchecked} still being checked",
                        if let Some(label) = label { format!("{label}: ") } else { String::default() },
                    )
                })).into()
            },
            Some(Message::CloseStatus { .. }) => Text::new("All seeds rolled, analyzing results…").into(),
            Some(Message::Done { stats_dir }) => Text::new(format!("stats saved to {}", stats_dir.display())).into(),
            Some(Message::InstructionsNoSuccesses) => Text::new("No successful seeds, so average instruction count is infinite").into(),
            Some(Message::Instructions { num_successes, num_failures, success_rate, average_instructions_success, average_instructions_failure, average_failure_count: _, average_instructions }) => Text::new(format!(
                "success rate: {num_successes}/{} ({:.02}%)\naverage instructions (success): {average_instructions_success} ({average_instructions_success:.3e})\naverage instructions (failure): {}\naverage total instructions until success: {average_instructions} ({average_instructions:.3e})",
                num_successes + num_failures,
                success_rate * 100.0,
                if *num_failures == 0 { format!("N/A") } else { format!("{average_instructions_failure} ({average_instructions_failure:.3e})") },
            )).into(),
            Some(msg) => Text::new(format!("{msg:?}")).into(),
        }
    }
}
