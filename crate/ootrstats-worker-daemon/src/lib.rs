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
    itertools::Itertools as _,
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
    #[cfg(windows)] #[error(transparent)] Windows(#[from] windows_result::Error),
    #[error(transparent)] Worker(#[from] ootrstats::worker::Error),
    #[error(transparent)] WorkerSend(#[from] mpsc::error::SendError<ootrstats::worker::SupervisorMessage>),
    #[error(transparent)] Write(#[from] async_proto::WriteError),
    #[error("patch file with unexpected file name structure")]
    PatchFilename,
}

async fn work(correct_password: &str, sink: Arc<Mutex<SplitSink<rocket_ws::stream::DuplexStream, rocket_ws::Message>>>, stream: &mut SplitStream<rocket_ws::stream::DuplexStream>, #[cfg_attr(not(windows), allow(unused))] unhide_reboot: &mut bool, #[cfg_attr(not(windows), allow(unused))] unhide_sleep: &mut bool) -> Result<(), Error> {
    let websocket::ClientMessage::Handshake { password: received_password, base_rom_path, wsl_distro, rando_rev, setup, output_mode, min_disk, min_disk_percent, min_disk_mount_points, priority_users, race, hide_reboot, hide_sleep } = websocket::ClientMessage::read_ws021(stream).await? else { return Ok(()) };
    if !constant_time_eq(received_password.as_bytes(), correct_password.as_bytes()) { return Ok(()) }
    #[cfg(windows)] {
        if hide_reboot {
            windows_registry::LOCAL_MACHINE.create("SOFTWARE\\Microsoft\\PolicyManager\\default\\Start\\HideRestart")?.set_u32("value", 1)?;
            *unhide_reboot = true;
        }
        if hide_sleep {
            windows_registry::LOCAL_MACHINE.create("SOFTWARE\\Microsoft\\PolicyManager\\default\\Start\\HideSleep")?.set_u32("value", 1)?;
            *unhide_sleep = true;
        }
    }
    #[cfg(not(windows))] {
        let _ = hide_reboot;
        let _ = hide_sleep;
    }
    let (worker_tx, mut worker_rx) = mpsc::channel(256);
    let (mut supervisor_tx, supervisor_rx) = mpsc::channel(256);
    let mut stream = Some(stream);
    let min_disk_mount_points = min_disk_mount_points.map(|mp| mp.into_iter().map(PathBuf::from).collect_vec());
    let mut work = pin!(ootrstats::worker::work(true, worker_tx, supervisor_rx, PathBuf::from(base_rom_path.clone()), 0, wsl_distro, rando_rev, setup, output_mode, min_disk, min_disk_percent, min_disk_mount_points.as_deref(), &priority_users, race));
    loop {
        let next_msg = if let Some(ref mut stream) = stream {
            Either::Left(timeout(Duration::from_secs(60), websocket::ClientMessage::read_ws021(*stream)))
        } else {
            Either::Right(future::pending())
        };
        println!("daemon loop, worker_rx = {worker_rx:?}, stream = {}", match next_msg { Either::Left(_) => "Some(_)", Either::Right(_) => "None" });
        select! {
            res = &mut work => {
                let () = res?;
                println!("work task finished, processing remaining messages...");
                while let Some(msg) = worker_rx.recv().await {
                    match msg {
                        ootrstats::worker::Message::Init(msg) => lock!(sink = sink; websocket::ServerMessage::Init(msg).write_ws021(&mut *sink).await)?,
                        ootrstats::worker::Message::Ready(ready) => lock!(sink = sink; websocket::ServerMessage::Ready(ready).write_ws021(&mut *sink).await)?,
                        ootrstats::worker::Message::Success { seed_idx, instructions, rsl_instructions, spoiler_log, patch, compressed_rom, uncompressed_rom, rsl_plando } => {
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
                            let compressed_rom = match compressed_rom {
                                Some(Either::Left((wsl, compressed_rom_path))) => Some(if let Some(wsl_distro) = wsl {
                                    let mut cmd = Command::new(WSL);
                                    if let Some(wsl_distro) = &wsl_distro {
                                        cmd.arg("--distribution");
                                        cmd.arg(wsl_distro);
                                    }
                                    cmd.arg("cat");
                                    cmd.arg(&compressed_rom_path);
                                    let compressed_rom = cmd.check("wsl cat").await?.stdout.into();
                                    let mut cmd = Command::new(WSL);
                                    if let Some(wsl_distro) = &wsl_distro {
                                        cmd.arg("--distribution");
                                        cmd.arg(wsl_distro);
                                    }
                                    cmd.arg("rm");
                                    cmd.arg(compressed_rom_path);
                                    cmd.check("wsl rm").await?;
                                    compressed_rom
                                } else {
                                    let compressed_rom = fs::read(&compressed_rom_path).await?.into();
                                    fs::remove_file(compressed_rom_path).await?;
                                    compressed_rom
                                }),
                                Some(Either::Right(compressed_rom)) => Some(compressed_rom),
                                None => None,
                            };
                            let uncompressed_rom = match uncompressed_rom {
                                Some(Either::Left(uncompressed_rom_path)) => {
                                    let uncompressed_rom = fs::read(&uncompressed_rom_path).await?.into();
                                    fs::remove_file(uncompressed_rom_path).await?;
                                    Some(uncompressed_rom)
                                }
                                Some(Either::Right(uncompressed_rom)) => Some(uncompressed_rom),
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
                            lock!(sink = sink; websocket::ServerMessage::Success { seed_idx, instructions, rsl_instructions, spoiler_log, patch, compressed_rom, uncompressed_rom, rsl_plando }.write_ws021(&mut *sink).await)?;
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
                println!("done processing remaining messages");
                break
            }
            Some(msg) = worker_rx.recv() => match msg {
                ootrstats::worker::Message::Init(msg) => lock!(sink = sink; websocket::ServerMessage::Init(msg).write_ws021(&mut *sink).await)?,
                ootrstats::worker::Message::Ready(ready) => lock!(sink = sink; websocket::ServerMessage::Ready(ready).write_ws021(&mut *sink).await)?,
                ootrstats::worker::Message::Success { seed_idx, instructions, rsl_instructions, spoiler_log, patch, compressed_rom, uncompressed_rom, rsl_plando } => {
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
                    let compressed_rom = match compressed_rom {
                        Some(Either::Left((wsl, compressed_rom_path))) => Some(if let Some(wsl_distro) = wsl {
                            let mut cmd = Command::new(WSL);
                            if let Some(wsl_distro) = &wsl_distro {
                                cmd.arg("--distribution");
                                cmd.arg(wsl_distro);
                            }
                            cmd.arg("cat");
                            cmd.arg(&compressed_rom_path);
                            let compressed_rom = cmd.check("wsl cat").await?.stdout.into();
                            let mut cmd = Command::new(WSL);
                            if let Some(wsl_distro) = &wsl_distro {
                                cmd.arg("--distribution");
                                cmd.arg(wsl_distro);
                            }
                            cmd.arg("rm");
                            cmd.arg(compressed_rom_path);
                            cmd.check("wsl rm").await?;
                            compressed_rom
                        } else {
                            let compressed_rom = fs::read(&compressed_rom_path).await?.into();
                            fs::remove_file(compressed_rom_path).await?;
                            compressed_rom
                        }),
                        Some(Either::Right(compressed_rom)) => Some(compressed_rom),
                        None => None,
                    };
                    let uncompressed_rom = match uncompressed_rom {
                        Some(Either::Left(uncompressed_rom_path)) => {
                            let uncompressed_rom = fs::read(&uncompressed_rom_path).await?.into();
                            fs::remove_file(uncompressed_rom_path).await?;
                            Some(uncompressed_rom)
                        }
                        Some(Either::Right(uncompressed_rom)) => Some(uncompressed_rom),
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
                    lock!(sink = sink; websocket::ServerMessage::Success { seed_idx, instructions, rsl_instructions, spoiler_log, patch, compressed_rom, uncompressed_rom, rsl_plando }.write_ws021(&mut *sink).await)?;
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
                websocket::ClientMessage::Supervisor(msg) => {
                    println!("got supervisor message: {msg:?}");
                    supervisor_tx.send(msg).await?;
                }
                websocket::ClientMessage::Ping => {}
                websocket::ClientMessage::Goodbye => {
                    println!("got goodbye message");
                    // drop sender so the worker can shut down
                    supervisor_tx = mpsc::channel(1).0;
                    stream = None;
                }
            },
        }
    }
    println!("end of WebSocket session");
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
        let mut unhide_reboot = false;
        let mut unhide_sleep = false;
        #[cfg_attr(not(windows), allow(unused_mut))] let mut work_result = work(&correct_password, sink.clone(), &mut stream, &mut unhide_reboot, &mut unhide_sleep).await;
        ping_task.abort();
        #[cfg(windows)] {
            if unhide_reboot {
                if let Err(e) = windows_registry::LOCAL_MACHINE.create("SOFTWARE\\Microsoft\\PolicyManager\\default\\Start\\HideRestart").and_then(|key| key.set_u32("value", 0)) {
                    work_result = work_result.and_then(|_| Err(e.into()));
                }
            }
            if unhide_sleep {
                if let Err(e) = windows_registry::LOCAL_MACHINE.create("SOFTWARE\\Microsoft\\PolicyManager\\default\\Start\\HideSleep").and_then(|key| key.set_u32("value", 0)) {
                    work_result = work_result.and_then(|_| Err(e.into()));
                }
            }
        }
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
