use {
    std::{
        io,
        path::PathBuf,
        pin::pin,
    },
    async_proto::Protocol as _,
    either::Either,
    futures::stream::{
        SplitSink,
        SplitStream,
        StreamExt as _,
    },
    if_chain::if_chain,
    rocket::State,
    rocket_ws::WebSocket,
    tokio::{
        select,
        sync::mpsc,
    },
    wheel::fs,
    ootrstats::websocket,
};
#[cfg(unix)] use xdg::BaseDirectories;
#[cfg(windows)] use directories::ProjectDirs;

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)] Read(#[from] async_proto::ReadError),
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[error(transparent)] Worker(#[from] ootrstats::worker::Error),
    #[error(transparent)] WorkerSend(#[from] mpsc::error::SendError<ootrstats::worker::SupervisorMessage>),
    #[error(transparent)] Write(#[from] async_proto::WriteError),
}

async fn work(correct_password: &str, sink: &mut SplitSink<rocket_ws::stream::DuplexStream, rocket_ws::Message>, stream: &mut SplitStream<rocket_ws::stream::DuplexStream>) -> Result<(), Error> {
    let websocket::ClientMessage::Handshake { password: received_password, base_rom_path, wsl_base_rom_path, rando_rev, setup, bench } = websocket::ClientMessage::read_ws(stream).await? else { return Ok(()) };
    if received_password != correct_password { return Ok(()) }
    let (worker_tx, mut worker_rx) = mpsc::channel(256);
    let (supervisor_tx, supervisor_rx) = mpsc::channel(256);
    let base_rom_path = if_chain! {
        if cfg!(windows);
        if bench;
        if let Some(wsl_base_rom_path) = wsl_base_rom_path;
        then {
            wsl_base_rom_path
        } else {
            base_rom_path
        }
    };
    let mut work = pin!(ootrstats::worker::work(worker_tx, supervisor_rx, PathBuf::from(base_rom_path), 0, rando_rev, setup, bench));
    loop {
        select! {
            res = &mut work => {
                let () = res?;
                while let Some(msg) = worker_rx.recv().await {
                    match msg {
                        ootrstats::worker::Message::Init(msg) => websocket::ServerMessage::Init(msg).write_ws(sink).await?,
                        ootrstats::worker::Message::Ready(ready) => websocket::ServerMessage::Ready(ready).write_ws(sink).await?,
                        ootrstats::worker::Message::Success { seed_idx, instructions, spoiler_log, ready } => {
                            let spoiler_log = match spoiler_log {
                                Either::Left(spoiler_log_path) => {
                                    let spoiler_log = fs::read(&spoiler_log_path).await?.into();
                                    fs::remove_file(spoiler_log_path).await?;
                                    spoiler_log
                                }
                                Either::Right(spoiler_log) => spoiler_log,
                            };
                            websocket::ServerMessage::Success { seed_idx, instructions, spoiler_log, ready }.write_ws(sink).await?;
                        }
                        ootrstats::worker::Message::Failure { seed_idx, instructions, error_log, ready } => websocket::ServerMessage::Failure { seed_idx, instructions, error_log, ready }.write_ws(sink).await?,
                    }
                }
                break
            }
            Some(msg) = worker_rx.recv() => match msg {
                ootrstats::worker::Message::Init(msg) => websocket::ServerMessage::Init(msg).write_ws(sink).await?,
                ootrstats::worker::Message::Ready(ready) => websocket::ServerMessage::Ready(ready).write_ws(sink).await?,
                ootrstats::worker::Message::Success { seed_idx, instructions, spoiler_log, ready } => {
                    let spoiler_log = match spoiler_log {
                        Either::Left(spoiler_log_path) => {
                            let spoiler_log = fs::read(&spoiler_log_path).await?.into();
                            fs::remove_file(spoiler_log_path).await?;
                            spoiler_log
                        }
                        Either::Right(spoiler_log) => spoiler_log,
                    };
                    websocket::ServerMessage::Success { seed_idx, instructions, spoiler_log, ready }.write_ws(sink).await?;
                }
                ootrstats::worker::Message::Failure { seed_idx, instructions, error_log, ready } => websocket::ServerMessage::Failure { seed_idx, instructions, error_log, ready }.write_ws(sink).await?,
            },
            res = websocket::ClientMessage::read_ws(stream) => match res? {
                websocket::ClientMessage::Handshake { .. } => break,
                websocket::ClientMessage::Supervisor(msg) => supervisor_tx.send(msg).await?,
            },
        }
    }
    Ok(())
}

#[rocket::get("/v3")] //TODO ensure this matches the major crate version
fn index(correct_password: &State<String>, ws: WebSocket) -> rocket_ws::Channel<'static> {
    let correct_password = (*correct_password).clone();
    ws.channel(move |stream| Box::pin(async move {
        let (mut sink, mut stream) = stream.split();
        match work(&correct_password, &mut sink, &mut stream).await {
            Ok(()) => {}
            Err(e) => websocket::ServerMessage::Error {
                display: e.to_string(),
                debug: format!("{e:?}"),
            }.write_ws(&mut sink).await.map_err(io::Error::from)?,
        }
        Ok(())
    }))
}

#[derive(Debug, thiserror::Error)]
enum MainError {
    #[error(transparent)] Rocket(#[from] rocket::Error),
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[cfg(unix)] #[error(transparent)] Xdg(#[from] xdg::BaseDirectoriesError),
    #[cfg(windows)]
    #[error("user folder not found")]
    MissingHomeDir,
    #[cfg(unix)]
    #[error("password file not found")]
    MissingPasswordFile,
}

#[wheel::main(rocket)]
async fn main() -> Result<(), MainError> {
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = wheel::night_report_sync("/net/ootrstats/error", Some("thread panic"));
        default_panic_hook(info)
    }));
    rocket::custom(rocket::Config {
        port: 18826,
        ..rocket::Config::default()
    })
    .mount("/", rocket::routes![
        index,
    ])
    .manage(fs::read_to_string({
        #[cfg(unix)] { BaseDirectories::new()?.find_config_file("ootrstats-worker-daemon-password.txt").ok_or(MainError::MissingPasswordFile)? }
        #[cfg(windows)] { ProjectDirs::from("net", "Fenhl", "ootrstats").ok_or(MainError::MissingHomeDir)?.config_dir().join("worker-daemon-password.txt") }
    }).await?.trim().to_owned())
    .launch().await?;
    Ok(())
}
