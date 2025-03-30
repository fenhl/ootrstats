#![allow(unused_crate_dependencies)] // lib/bin combo crate

use {
    std::{
        io,
        path::PathBuf,
        pin::pin,
        sync::Arc,
        time::Duration,
    },
    async_proto::Protocol as _,
    constant_time_eq::constant_time_eq,
    either::Either,
    futures::{
        future,
        stream::{
            SplitSink,
            SplitStream,
            StreamExt as _,
        },
    },
    log_lock::*,
    rocket::State,
    rocket_ws::WebSocket,
    tokio::{
        select,
        process::Command,
        sync::mpsc,
        time::{
            sleep,
            timeout,
        },
    },
    wheel::{
        fs,
        traits::AsyncCommandOutputExt as _,
    },
    ootrstats::{
        WSL,
        websocket,
    },
    crate::config::Config,
};

mod config;

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)] Elapsed(#[from] tokio::time::error::Elapsed),
    #[error(transparent)] Read(#[from] async_proto::ReadError),
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[error(transparent)] Worker(#[from] ootrstats::worker::Error),
    #[error(transparent)] WorkerSend(#[from] mpsc::error::SendError<ootrstats::worker::SupervisorMessage>),
    #[error(transparent)] Write(#[from] async_proto::WriteError),
    #[error("patch file with unexpected file name structure")]
    PatchFilename,
}

async fn work(correct_password: &str, sink: Arc<Mutex<SplitSink<rocket_ws::stream::DuplexStream, rocket_ws::Message>>>, stream: &mut SplitStream<rocket_ws::stream::DuplexStream>) -> Result<(), Error> {
    let websocket::ClientMessage::Handshake { password: received_password, base_rom_path, wsl_distro, rando_rev, setup, output_mode, priority_users } = websocket::ClientMessage::read_ws021(stream).await? else { return Ok(()) };
    if !constant_time_eq(received_password.as_bytes(), correct_password.as_bytes()) { return Ok(()) }
    let (worker_tx, mut worker_rx) = mpsc::channel(256);
    let (mut supervisor_tx, supervisor_rx) = mpsc::channel(256);
    let mut stream = Some(stream);
    let mut work = pin!(ootrstats::worker::work(worker_tx, supervisor_rx, PathBuf::from(base_rom_path.clone()), 0, wsl_distro, rando_rev, setup, output_mode, &priority_users));
    loop {
        let next_msg = if let Some(ref mut stream) = stream {
            Either::Left(timeout(Duration::from_secs(60), websocket::ClientMessage::read_ws021(*stream)))
        } else {
            Either::Right(future::pending())
        };
        select! {
            res = &mut work => {
                let () = res?;
                while let Some(msg) = worker_rx.recv().await {
                    match msg {
                        ootrstats::worker::Message::Init(msg) => lock!(sink = sink; websocket::ServerMessage::Init(msg).write_ws021(&mut *sink).await)?,
                        ootrstats::worker::Message::Ready(ready) => lock!(sink = sink; websocket::ServerMessage::Ready(ready).write_ws021(&mut *sink).await)?,
                        ootrstats::worker::Message::Success { seed_idx, instructions, rsl_instructions, spoiler_log, patch, rsl_plando } => {
                            let spoiler_log = match spoiler_log {
                                Either::Left(spoiler_log_path) => {
                                    let spoiler_log = fs::read(&spoiler_log_path).await?.into();
                                    fs::remove_file(spoiler_log_path).await?;
                                    spoiler_log
                                }
                                Either::Right(spoiler_log) => spoiler_log,
                            };
                            let patch = match patch {
                                Some(Either::Left((wsl, patch_path))) => Some((patch_path.extension().ok_or(Error::PatchFilename)?.to_str().ok_or(Error::PatchFilename)?.to_owned(), if let Some(wsl_distro) = wsl {
                                    let mut cmd = Command::new(WSL);
                                    if let Some(wsl_distro) = &wsl_distro {
                                        cmd.arg("--distribution");
                                        cmd.arg(wsl_distro);
                                    }
                                    cmd.arg("cat");
                                    cmd.arg(&patch_path);
                                    let patch = cmd.check("wsl cat").await?.stdout.into();
                                    let mut cmd = Command::new(WSL);
                                    if let Some(wsl_distro) = &wsl_distro {
                                        cmd.arg("--distribution");
                                        cmd.arg(wsl_distro);
                                    }
                                    cmd.arg("rm");
                                    cmd.arg(patch_path);
                                    cmd.check("wsl rm").await?;
                                    patch
                                } else {
                                    let patch = fs::read(&patch_path).await?.into();
                                    fs::remove_file(patch_path).await?;
                                    patch
                                })),
                                Some(Either::Right((ext, patch))) => Some((ext, patch)),
                                None => None,
                            };
                            let rsl_plando = match rsl_plando {
                                Some(Either::Left(rsl_plando_path)) => {
                                    let rsl_plando = fs::read(&rsl_plando_path).await?.into();
                                    fs::remove_file(rsl_plando_path).await?;
                                    Some(rsl_plando)
                                }
                                Some(Either::Right(rsl_plando)) => Some(rsl_plando),
                                None => None,
                            };
                            lock!(sink = sink; websocket::ServerMessage::Success { seed_idx, instructions, rsl_instructions, spoiler_log, patch, rsl_plando }.write_ws021(&mut *sink).await)?;
                        }
                        ootrstats::worker::Message::Failure { seed_idx, instructions, rsl_instructions, error_log, rsl_plando } => {
                            let rsl_plando = match rsl_plando {
                                Some(Either::Left(rsl_plando_path)) => {
                                    let rsl_plando = fs::read(&rsl_plando_path).await?.into();
                                    fs::remove_file(rsl_plando_path).await?;
                                    Some(rsl_plando)
                                }
                                Some(Either::Right(rsl_plando)) => Some(rsl_plando),
                                None => None,
                            };
                            lock!(sink = sink; websocket::ServerMessage::Failure { seed_idx, instructions, rsl_instructions, error_log, rsl_plando }.write_ws021(&mut *sink).await)?;
                        }
                    }
                }
                break
            }
            Some(msg) = worker_rx.recv() => match msg {
                ootrstats::worker::Message::Init(msg) => lock!(sink = sink; websocket::ServerMessage::Init(msg).write_ws021(&mut *sink).await)?,
                ootrstats::worker::Message::Ready(ready) => lock!(sink = sink; websocket::ServerMessage::Ready(ready).write_ws021(&mut *sink).await)?,
                ootrstats::worker::Message::Success { seed_idx, instructions, rsl_instructions, spoiler_log, patch, rsl_plando } => {
                    let spoiler_log = match spoiler_log {
                        Either::Left(spoiler_log_path) => {
                            let spoiler_log = fs::read(&spoiler_log_path).await?.into();
                            fs::remove_file(spoiler_log_path).await?;
                            spoiler_log
                        }
                        Either::Right(spoiler_log) => spoiler_log,
                    };
                    let patch = match patch {
                        Some(Either::Left((wsl, patch_path))) => Some((patch_path.extension().ok_or(Error::PatchFilename)?.to_str().ok_or(Error::PatchFilename)?.to_owned(), if let Some(wsl_distro) = wsl {
                            let mut cmd = Command::new(WSL);
                            if let Some(wsl_distro) = &wsl_distro {
                                cmd.arg("--distribution");
                                cmd.arg(wsl_distro);
                            }
                            cmd.arg("cat");
                            cmd.arg(&patch_path);
                            let patch = cmd.check("wsl cat").await?.stdout.into();
                            let mut cmd = Command::new(WSL);
                            if let Some(wsl_distro) = &wsl_distro {
                                cmd.arg("--distribution");
                                cmd.arg(wsl_distro);
                            }
                            cmd.arg("rm");
                            cmd.arg(patch_path);
                            cmd.check("wsl rm").await?;
                            patch
                        } else {
                            let patch = fs::read(&patch_path).await?.into();
                            fs::remove_file(patch_path).await?;
                            patch
                        })),
                        Some(Either::Right((ext, patch))) => Some((ext, patch)),
                        None => None,
                    };
                    let rsl_plando = match rsl_plando {
                        Some(Either::Left(rsl_plando_path)) => {
                            let rsl_plando = fs::read(&rsl_plando_path).await?.into();
                            fs::remove_file(rsl_plando_path).await?;
                            Some(rsl_plando)
                        }
                        Some(Either::Right(rsl_plando)) => Some(rsl_plando),
                        None => None,
                    };
                    lock!(sink = sink; websocket::ServerMessage::Success { seed_idx, instructions, rsl_instructions, spoiler_log, patch, rsl_plando }.write_ws021(&mut *sink).await)?;
                }
                ootrstats::worker::Message::Failure { seed_idx, instructions, rsl_instructions, error_log, rsl_plando } => {
                    let rsl_plando = match rsl_plando {
                        Some(Either::Left(rsl_plando_path)) => {
                            let rsl_plando = fs::read(&rsl_plando_path).await?.into();
                            fs::remove_file(rsl_plando_path).await?;
                            Some(rsl_plando)
                        }
                        Some(Either::Right(rsl_plando)) => Some(rsl_plando),
                        None => None,
                    };
                    lock!(sink = sink; websocket::ServerMessage::Failure { seed_idx, instructions, rsl_instructions, error_log, rsl_plando }.write_ws021(&mut *sink).await)?;
                }
            },
            res = next_msg => match res?? {
                websocket::ClientMessage::Handshake { .. } => break,
                websocket::ClientMessage::Supervisor(msg) => supervisor_tx.send(msg).await?,
                websocket::ClientMessage::Ping => {}
                websocket::ClientMessage::Goodbye => {
                    // drop sender so the worker can shut down
                    supervisor_tx = mpsc::channel(1).0;
                    stream = None;
                }
            },
        }
    }
    Ok(())
}

#[ootrstats_macros::current_version]
fn index(correct_password: &State<String>, ws: WebSocket) -> rocket_ws::Channel<'static> {
    let correct_password = (*correct_password).clone();
    ws.channel(move |stream| Box::pin(async move {
        let (sink, mut stream) = stream.split();
        let sink = Arc::new(Mutex::new(sink));
        let ping_sink = sink.clone();
        let ping_task = tokio::spawn(async move {
            while lock!(sink = ping_sink; websocket::ServerMessage::Ping.write_ws021(&mut *sink).await).is_ok() {
                sleep(Duration::from_secs(30)).await;
            }
        });
        let work_result = work(&correct_password, sink.clone(), &mut stream).await;
        ping_task.abort();
        match work_result {
            Ok(()) => {}
            Err(e) => lock!(sink = sink; websocket::ServerMessage::Error {
                display: e.to_string(),
                debug: format!("{e:?}"),
            }.write_ws021(&mut *sink).await).map_err(io::Error::from)?,
        }
        Ok(())
    }))
}

#[derive(Debug, thiserror::Error)]
pub enum MainError {
    #[error(transparent)] Config(#[from] config::Error),
    #[error(transparent)] Rocket(#[from] rocket::Error),
}

pub async fn main() -> Result<(), MainError> {
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = wheel::night_report_sync("/net/ootrstats/error", Some("thread panic"));
        default_panic_hook(info)
    }));
    //TODO on Windows, use the `windows-service` crate to run as a service?
    let config = Config::load().await?;
    rocket::custom(rocket::Config {
        address: config.address,
        port: 18826,
        ..rocket::Config::default()
    })
    .mount("/", rocket::routes![
        index,
    ])
    .manage(config.password)
    .launch().await?;
    Ok(())
}
