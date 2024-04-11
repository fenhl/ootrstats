use {
    std::{
        path::PathBuf,
        pin::{
            Pin,
            pin,
        },
        time::Duration,
    },
    either::Either,
    futures::{
        SinkExt as _,
        stream::{
            self,
            FusedStream,
            StreamExt as _,
        },
    },
    if_chain::if_chain,
    semver::Version,
    serde::Deserialize,
    tokio::{
        select,
        sync::mpsc,
        task::JoinHandle,
        time::{
            MissedTickBehavior,
            interval,
            timeout,
        },
    },
    tokio_tungstenite::tungstenite,
    ootrstats::{
        RandoSetup,
        SeedIdx,
        websocket,
        worker::{
            Message,
            SupervisorMessage,
        },
    },
};

fn make_neg_one() -> i8 { -1 }
fn make_true() -> bool { true }

#[derive(Deserialize)]
pub(crate) struct Config {
    pub(crate) name: String,
    #[serde(flatten)]
    pub(crate) kind: Kind,
    #[serde(default = "make_true")]
    pub(crate) bench: bool,
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum Kind {
    #[serde(rename_all = "camelCase")]
    Local {
        base_rom_path: PathBuf,
        wsl_base_rom_path: Option<PathBuf>,
        #[serde(default = "make_neg_one")] // default to keeping one core free to avoid slowing down the supervisor too much
        cores: i8,
    },
    #[serde(rename_all = "camelCase")]
    WebSocket {
        #[serde(default = "make_true")]
        tls: bool,
        hostname: String,
        password: String,
        base_rom_path: String,
        wsl_base_rom_path: Option<String>,
    },
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)] Elapsed(#[from] tokio::time::error::Elapsed),
    #[error(transparent)] Local(#[from] ootrstats::worker::Error),
    #[error(transparent)] Read(#[from] async_proto::ReadError),
    #[error(transparent)] Semver(#[from] semver::Error),
    #[error(transparent)] Send(#[from] mpsc::error::SendError<(String, Message)>),
    #[error(transparent)] WebSocket(#[from] tungstenite::Error),
    #[error(transparent)] Write(#[from] async_proto::WriteError),
    #[error("{display}")]
    Remote {
        debug: String,
        display: String,
    },
}

impl Kind {
    async fn run(self, name: String, tx: mpsc::Sender<(String, Message)>, mut rx: mpsc::Receiver<SupervisorMessage>, rando_rev: git2::Oid, setup: RandoSetup, bench: bool) -> Result<(), Error> {
        match self {
            Self::Local { base_rom_path, wsl_base_rom_path, cores } => {
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
                let (inner_tx, mut inner_rx) = mpsc::channel(256);
                let mut work = pin!(ootrstats::worker::work(inner_tx, rx, base_rom_path, cores, rando_rev, setup, bench));
                loop {
                    select! {
                        res = &mut work => {
                            let () = res?;
                            while let Some(msg) = inner_rx.recv().await {
                                tx.send((name.clone(), msg)).await?;
                            }
                            break
                        }
                        msg = inner_rx.recv() => if let Some(msg) = msg {
                            tx.send((name.clone(), msg)).await?;
                        } else {
                            drop(tx);
                            let () = work.await?;
                            break
                        },
                    }
                }
            }
            Self::WebSocket { tls, hostname, password, base_rom_path, wsl_base_rom_path } => {
                tx.send((name.clone(), Message::Init(format!("connecting WebSocket")))).await?;
                let (sink, stream) = async_proto::websocket(format!("{}://{hostname}/v{}", if tls { "wss" } else { "ws" }, Version::parse(env!("CARGO_PKG_VERSION"))?.major)).await?;
                let mut sink = pin!(sink);
                let mut stream = Box::pin(stream.fuse()) as Pin<Box<dyn FusedStream<Item = _> + Send>>;
                tx.send((name.clone(), Message::Init(format!("handshaking")))).await?;
                sink.send(websocket::ClientMessage::Handshake { password, base_rom_path, wsl_base_rom_path, rando_rev, setup, bench }).await?;
                tx.send((name.clone(), Message::Init(format!("waiting for reply from worker")))).await?;
                let mut ping_interval = interval(Duration::from_secs(30));
                ping_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
                loop {
                    select! {
                        _ = ping_interval.tick() => sink.send(websocket::ClientMessage::Ping).await?,
                        res = timeout(Duration::from_secs(60), stream.select_next_some()) => match res? {
                            Ok(websocket::ServerMessage::Init(msg)) => tx.send((name.clone(), Message::Init(msg))).await?,
                            Ok(websocket::ServerMessage::Ready(ready)) => tx.send((name.clone(), Message::Ready(ready))).await?,
                            Ok(websocket::ServerMessage::Success { seed_idx, instructions, spoiler_log, ready }) => tx.send((name.clone(), Message::Success {
                                spoiler_log: Either::Right(spoiler_log),
                                seed_idx, instructions, ready,
                            })).await?,
                            Ok(websocket::ServerMessage::Failure { seed_idx, instructions, error_log, ready }) => tx.send((name.clone(), Message::Failure { seed_idx, instructions, error_log, ready })).await?,
                            Ok(websocket::ServerMessage::Error { display, debug }) => return Err(Error::Remote { debug, display }),
                            Ok(websocket::ServerMessage::Ping) => {}
                            Err(async_proto::ReadError { kind: async_proto::ReadErrorKind::Tungstenite(tungstenite::Error::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake)), .. }) => stream = Box::pin(stream::empty()),
                            Err(e) => return Err(e.into()),
                        },
                        res = rx.recv() => if let Some(msg) = res {
                            sink.send(websocket::ClientMessage::Supervisor(msg)).await?;
                        } else {
                            sink.send(websocket::ClientMessage::Goodbye).await?;
                            drop(sink);
                            while let Some(res) = timeout(Duration::from_secs(60), stream.next()).await? {
                                match res {
                                    Ok(websocket::ServerMessage::Init(msg)) => tx.send((name.clone(), Message::Init(msg))).await?,
                                    Ok(websocket::ServerMessage::Ready(ready)) => tx.send((name.clone(), Message::Ready(ready))).await?,
                                    Ok(websocket::ServerMessage::Success { seed_idx, instructions, spoiler_log, ready }) => tx.send((name.clone(), Message::Success {
                                        spoiler_log: Either::Right(spoiler_log),
                                        seed_idx, instructions, ready,
                                    })).await?,
                                    Ok(websocket::ServerMessage::Failure { seed_idx, instructions, error_log, ready }) => tx.send((name.clone(), Message::Failure { seed_idx, instructions, error_log, ready })).await?,
                                    Ok(websocket::ServerMessage::Error { display, debug }) => return Err(Error::Remote { debug, display }),
                                    Ok(websocket::ServerMessage::Ping) => {}
                                    Err(async_proto::ReadError { kind: async_proto::ReadErrorKind::Tungstenite(tungstenite::Error::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake)), .. }) => break,
                                    Err(e) => return Err(e.into()),
                                }
                            }
                            break
                        },
                    }
                }
            }
        }
        Ok(())
    }
}

pub(crate) struct State {
    pub(crate) name: String,
    pub(crate) msg: Option<String>,
    pub(crate) ready: u8,
    pub(crate) running: u8,
    pub(crate) completed: u16,
    pub(crate) supervisor_tx: mpsc::Sender<SupervisorMessage>,
    pub(crate) stopped: bool,
}

impl State {
    pub(crate) fn new(worker_tx: mpsc::Sender<(String, Message)>, name: String, kind: Kind, rando_rev: git2::Oid, setup: &RandoSetup, bench: bool) -> (JoinHandle<Result<(), Error>>, Self) {
        let (supervisor_tx, supervisor_rx) = mpsc::channel(256);
        (
            tokio::spawn(kind.run(name.clone(), worker_tx, supervisor_rx, rando_rev, setup.clone(), bench)),
            Self {
                msg: None,
                ready: 0,
                running: 0,
                completed: 0,
                stopped: false,
                name, supervisor_tx,
            }
        )
    }

    pub(crate) async fn roll(&mut self, seed_idx: SeedIdx) -> Result<(), mpsc::error::SendError<SupervisorMessage>> {
        self.supervisor_tx.send(SupervisorMessage::Roll(seed_idx)).await?;
        self.ready -= 1;
        self.running += 1;
        Ok(())
    }
}
