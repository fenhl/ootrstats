use {
    std::{
        ffi::OsString,
        iter,
        net::IpAddr,
        path::PathBuf,
        pin::{
            Pin,
            pin,
        },
        sync::Arc,
        time::Duration,
    },
    bytesize::ByteSize,
    either::Either,
    futures::{
        SinkExt as _,
        future::{
            self,
            FutureExt as _,
        },
        stream::{
            self,
            FusedStream,
            StreamExt as _,
        },
    },
    if_chain::if_chain,
    itertools::Itertools as _,
    linode_rs::LinodeApi,
    nonempty_collections::nev,
    rand::{
        distr::{
            Alphanumeric,
            SampleString as _,
        },
        rng,
    },
    semver::Version,
    serde::Serialize,
    serde_with::SerializeDisplay,
    tokio::{
        process::Command,
        select,
        sync::mpsc,
        task::JoinHandle,
        time::{
            MissedTickBehavior,
            interval,
            sleep,
            timeout,
        },
    },
    tokio_tungstenite::tungstenite,
    wheel::{
        fs::File,
        traits::{
            AsyncCommandOutputExt as _,
            IoResultExt as _,
            IsNetworkError,
        },
    },
    ootrstats::{
        OutputMode,
        RandoSetup,
        SeedIdx,
        websocket,
        worker::{
            Message,
            SupervisorMessage,
        },
    },
    crate::SeedState,
};
#[cfg(windows)] use ootrstats::WSL;
#[cfg(not(windows))] use {
    futures::stream::TryStreamExt as _,
    wheel::fs,
};

pub(crate) type Config = crate::config::Worker;
pub(crate) type Kind = crate::config::WorkerKind;

fn display_websocket_error(e: &tungstenite::Error) -> String {
    if_chain! {
        if let tungstenite::Error::Http(response) = e;
        if response.status() == tungstenite::http::StatusCode::NOT_FOUND;
        then {
            format!("{e}. Perhaps the worker needs to be updated?")
        } else {
            e.to_string()
        }
    }
}

#[derive(Debug, thiserror::Error, SerializeDisplay)]
pub(crate) enum Error {
    #[error(transparent)] Elapsed(#[from] tokio::time::error::Elapsed),
    #[error(transparent)] Linode(#[from] linode_rs::LinodeError),
    #[error(transparent)] Local(#[from] ootrstats::worker::Error),
    #[error(transparent)] Read(#[from] async_proto::ReadError),
    #[error(transparent)] Reqwest(#[from] reqwest::Error),
    #[error(transparent)] Semver(#[from] semver::Error),
    #[error(transparent)] Send(#[from] mpsc::error::SendError<(Arc<str>, Message)>),
    #[error("{}", display_websocket_error(.0))] WebSocket(#[from] tungstenite::Error),
    #[error(transparent)] Wheel(#[from] wheel::Error),
    #[error(transparent)] Write(#[from] async_proto::WriteError),
    #[error("{source} ({addr})")]
    AddrParse {
        source: std::net::AddrParseError,
        addr: String,
    },
    #[error("IPv6 address did not contain slash")]
    AddrSplit,
    #[error("failed to identify linode configuration")]
    IdentifyConfig,
    #[error("failed to identify linode disks")]
    IdentifyDisks,
    #[error("failed to upload linode image: {0}")]
    LinodeUploadImage(linode_rs::LinodeError),
    #[cfg(not(windows))]
    #[error("Linode image not found in Nix build output")]
    MissingLinodeImage,
    #[error("non-UTF-8 string")]
    OsString(OsString),
    #[error("worker has stopped listening to commands")]
    Receive {
        message: SupervisorMessage,
    },
    #[error("{display}")]
    Remote {
        debug: String,
        display: String,
    },
}

impl From<OsString> for Error {
    fn from(value: OsString) -> Self {
        Self::OsString(value)
    }
}

impl IsNetworkError for Error {
    fn is_network_error(&self) -> bool {
        match self {
            | Self::LinodeUploadImage(_) // retrying image upload seems to create extra images
            | Self::Local(_)
            | Self::Semver(_)
            | Self::Send(_)
            | Self::AddrParse { .. }
            | Self::AddrSplit
            | Self::IdentifyConfig
            | Self::IdentifyDisks
            | Self::OsString(_)
            | Self::Receive { .. }
            | Self::Remote { .. }
                => false,
            #[cfg(not(windows))] Self::MissingLinodeImage => false,
            Self::Elapsed(_) => true,
            Self::Linode(e) => e.is_network_error(),
            Self::Read(e) => e.is_network_error(),
            Self::Reqwest(e) => e.is_network_error(),
            Self::WebSocket(e) => e.is_network_error() || if let tungstenite::Error::Http(response) = e { response.status() == tungstenite::http::StatusCode::NOT_FOUND } else { false },
            Self::Wheel(e) => e.is_network_error(),
            Self::Write(e) => e.is_network_error(),
        }
    }
}

impl Kind {
    async fn run(self, name: Arc<str>, tx: mpsc::Sender<(Arc<str>, Message)>, mut rx: mpsc::Receiver<SupervisorMessage>, rando_rev: gix::ObjectId, setup: RandoSetup, output_mode: OutputMode, min_disk: ByteSize, min_disk_percent: f64, min_disk_mount_points: Option<Vec<PathBuf>>, race: bool) -> Result<(), Error> {
        match self {
            Self::Local { base_rom_path, wsl_distro, cores } => {
                let (inner_tx, mut inner_rx) = mpsc::channel(256);
                let mut work = pin!(ootrstats::worker::work(false, inner_tx, rx, base_rom_path.clone(), cores, wsl_distro, rando_rev, setup, output_mode, min_disk, min_disk_percent, min_disk_mount_points.as_deref(), &[], race));
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
            Self::WebSocket { tls, hostname, password, wsl_distro, priority_users, hide_reboot, hide_sleep } => {
                tx.send((name.clone(), Message::Init(format!("connecting WebSocket")))).await?;
                let (sink, stream) = async_proto::websocket029(format!("{}://{hostname}/v{}", if tls { "wss" } else { "ws" }, Version::parse(env!("CARGO_PKG_VERSION"))?.major)).await?;
                let mut sink = pin!(sink);
                let mut stream = Box::pin(stream.fuse()) as Pin<Box<dyn FusedStream<Item = _> + Send>>;
                tx.send((name.clone(), Message::Init(format!("handshaking")))).await?;
                sink.send(websocket::ClientMessage::Handshake {
                    min_disk_mount_points: min_disk_mount_points.map(|mp| mp.into_iter().map(|p| p.into_os_string().into_string()).collect::<Result<_, _>>()).transpose()?,
                    password, wsl_distro, rando_rev, setup, output_mode, min_disk, min_disk_percent, priority_users, race, hide_reboot, hide_sleep,
                }).await?;
                tx.send((name.clone(), Message::Init(format!("waiting for reply from worker")))).await?;
                let mut ping_interval = interval(Duration::from_secs(30));
                ping_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
                loop {
                    select! {
                        _ = ping_interval.tick() => sink.send(websocket::ClientMessage::Ping).await?,
                        res = timeout(Duration::from_secs(60), stream.next().then(|opt| if let Some(res) = opt { Either::Left(future::ready(res)) } else { Either::Right(future::pending()) })) => match res? {
                            Ok(websocket::ServerMessage::Init(msg)) => tx.send((name.clone(), Message::Init(msg))).await?,
                            Ok(websocket::ServerMessage::Ready(ready)) => tx.send((name.clone(), Message::Ready(ready))).await?,
                            Ok(websocket::ServerMessage::Success { seed_idx, instructions, rsl_instructions, spoiler_log, patch, rsl_plando }) => tx.send((name.clone(), Message::Success {
                                spoiler_log: Either::Right(spoiler_log),
                                patch: patch.map(Either::Right),
                                rsl_plando: rsl_plando.map(Either::Right),
                                seed_idx, instructions, rsl_instructions,
                            })).await?,
                            Ok(websocket::ServerMessage::Failure { seed_idx, instructions, rsl_instructions, error_log, rsl_plando }) => tx.send((name.clone(), Message::Failure {
                                rsl_plando: rsl_plando.map(Either::Right),
                                seed_idx, instructions, rsl_instructions, error_log,
                            })).await?,
                            Ok(websocket::ServerMessage::Error { display, debug }) => return Err(Error::Remote { debug, display }),
                            Ok(websocket::ServerMessage::Ping) => {}
                            Err(async_proto::ReadError { kind: async_proto::ReadErrorKind::Tungstenite029(tungstenite::Error::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake)), .. }) => stream = Box::pin(stream::empty()),
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
                                    Ok(websocket::ServerMessage::Success { seed_idx, instructions, rsl_instructions, spoiler_log, patch, rsl_plando }) => tx.send((name.clone(), Message::Success {
                                        spoiler_log: Either::Right(spoiler_log),
                                        patch: patch.map(Either::Right),
                                        rsl_plando: rsl_plando.map(Either::Right),
                                        seed_idx, instructions, rsl_instructions,
                                    })).await?,
                                    Ok(websocket::ServerMessage::Failure { seed_idx, instructions, rsl_instructions, error_log, rsl_plando }) => tx.send((name.clone(), Message::Failure {
                                        rsl_plando: rsl_plando.map(Either::Right),
                                        seed_idx, instructions, rsl_instructions, error_log,
                                    })).await?,
                                    Ok(websocket::ServerMessage::Error { display, debug }) => return Err(Error::Remote { debug, display }),
                                    Ok(websocket::ServerMessage::Ping) => {}
                                    Err(async_proto::ReadError { kind: async_proto::ReadErrorKind::Tungstenite029(tungstenite::Error::Protocol(tungstenite::error::ProtocolError::ResetWithoutClosingHandshake)), .. }) => break,
                                    Err(e) => return Err(e.into()),
                                }
                            }
                            break
                        },
                    }
                }
            }
            Self::Linode { api_token, image_region, label, linode_region, plan, #[cfg_attr(not(windows), allow(unused))] wsl_distro } => {
                let http_client = reqwest::Client::builder()
                    .user_agent(concat!("ootrstats/", env!("CARGO_PKG_VERSION"), " (https://github.com/fenhl/ootrstats)"))
                    .use_rustls_tls()
                    .https_only(true)
                    .build()?;
                let api = LinodeApi::new(api_token);
                let ip_addrs;
                let instance_id = {
                    tx.send((name.clone(), Message::Init(format!("building linode image")))).await?;
                    let temp_dir = tempfile::tempdir().at_unknown()?;
                    let mut cmd = {
                        #[cfg(windows)] {
                            let mut cmd = Command::new(WSL);
                            if let Some(wsl_distro) = &wsl_distro {
                                cmd.arg("--distribution");
                                cmd.arg(wsl_distro);
                            }
                            cmd.arg("nixos-rebuild");
                            cmd
                        }
                        #[cfg(not(windows))] { Command::new("nixos-rebuild") }
                    };
                    cmd.arg("build-image");
                    cmd.arg("--image-variant=linode");
                    cmd.arg("--flake=github:fenhl/ootrstats#bootstrap");
                    cmd.current_dir(&temp_dir);
                    cmd.check("nixos-rebuild").await?;
                    let image = {
                        #[cfg(windows)] {
                            let mut cmd = Command::new(WSL);
                            if let Some(wsl_distro) = wsl_distro {
                                cmd.arg("--distribution");
                                cmd.arg(wsl_distro);
                            }
                            cmd.arg("sh");
                            cmd.arg("-c");
                            cmd.arg("cp result/nixos-image-linode-*.img.gz nixos-linode.img.gz");
                            cmd.current_dir(&temp_dir);
                            cmd.check("wsl cp").await?;
                            temp_dir.path().join("nixos-linode.img.gz")
                        }
                        #[cfg(not(windows))] {
                            pin!(fs::read_dir(temp_dir.path().join("result"))
                                .try_filter(|entry| {
                                    let filename = entry.file_name();
                                    async move {
                                        let Ok(filename) = filename.into_string() else { return false };
                                        filename.starts_with("nixos-image-linode-") && filename.ends_with(".img.gz")
                                    }
                                }))
                                .try_next().await?
                                .ok_or(Error::MissingLinodeImage)?
                                .path()
                        }
                    };
                    tx.send((name.clone(), Message::Init(format!("uploading linode image")))).await?;
                    let image_id = api.upload_image_async(&http_client, false, None, "ootrstats", &image_region, &[], File::open(image).await?.into_inner()).await.map_err(Error::LinodeUploadImage)?;
                    let temp_path = temp_dir.path().to_owned();
                    temp_dir.close().at(temp_path)?;
                    tx.send((name.clone(), Message::Init(format!("creating linode")))).await?;
                    let instance = loop {
                        let root_pass = Alphanumeric.sample_string(&mut rng(), 16); // never used but required by Linode API
                        match api.create_instance(&linode_region, &plan).booted(false).image(&image_id).label(&label).root_pass(&root_pass).run_async(&http_client).await {
                            Ok(instance) => break instance,
                            Err(linode_rs::LinodeError::Api { result: linode_rs::LinodeApiError { errors }, .. }) if errors.iter().all(|e| e.reason == "Image is not available yet. Please try again.") => sleep(Duration::from_secs(5)).await,
                            Err(e) => return Err(e.into()),
                        }
                    };
                    api.delete_image_async(&http_client, &image_id).await?;
                    let disks = api.list_disks_async(&http_client, instance.id).await?;
                    let (swap, main_disk) = disks.into_iter().partition::<Vec<_>, _>(|disk| matches!(disk.filesystem, linode_rs::FileSystem::Swap));
                    let swap = swap.into_iter().exactly_one().map_err(|_| Error::IdentifyDisks)?;
                    if swap.size < 1024 {
                        let main_disk = main_disk.into_iter().exactly_one().map_err(|_| Error::IdentifyDisks)?;
                        api.resize_disk_async(&http_client, instance.id, main_disk.id, main_disk.size + swap.size - 1024).await?;
                        api.resize_disk_async(&http_client, instance.id, swap.id, 1024).await?;
                    }
                    let config = api.list_configs_async(&http_client, instance.id).await?.into_iter().exactly_one().map_err(|_| Error::IdentifyConfig)?;
                    api.edit_config_async(&http_client, instance.id, config.id, None, Some("linode/grub2"), None, None, Some(linode_rs::ConfigHelpers {
                        devtmpfs_automount: false,
                        distro: false,
                        modules_dep: false,
                        network: None,
                        updatedb_disabled: false,
                    })).await?;
                    api.boot_instance_async(&http_client, instance.id, None).await?;
                    ip_addrs = iter::once(instance.ipv6.split_once('/').ok_or(Error::AddrSplit)?.0.parse::<IpAddr>().map_err(|source| Error::AddrParse { source, addr: instance.ipv6 }))
                        .chain(instance.ipv4.into_iter().map(|addr| addr.parse().map_err(|source| Error::AddrParse { source, addr })))
                        .collect::<Result<Vec<_>, _>>()?;
                    instance.id
                };
                tx.send((name.clone(), Message::Init(format!("setup done, linode ID: {instance_id}, IP addresses: {ip_addrs:?}")))).await?; //DEBUG
                //TODO connect to the worker via WebSocket
                api.delete_instance_async(&http_client, instance_id).await?;
            }
        }
        Ok(())
    }
}

#[derive(Serialize)]
pub(crate) struct State {
    pub(crate) name: Arc<str>,
    pub(crate) msg: Option<String>,
    pub(crate) error: Option<Error>,
    pub(crate) prev_error: Option<Error>,
    pub(crate) ready: u8,
    #[serde(skip)]
    pub(crate) supervisor_tx: Option<mpsc::Sender<SupervisorMessage>>,
    pub(crate) stopping: bool,
    pub(crate) stopped: bool,
}

impl State {
    pub(crate) fn new(name: Arc<str>) -> Self {
        Self {
            msg: None,
            error: None,
            prev_error: None,
            ready: 0,
            supervisor_tx: None,
            stopping: false,
            stopped: false,
            name,
        }
    }

    pub(crate) fn connect(&mut self, worker_tx: mpsc::Sender<(Arc<str>, Message)>, kind: Kind, rando_rev: gix::ObjectId, setup: &RandoSetup, output_mode: OutputMode, min_disk: ByteSize, min_disk_percent: f64, min_disk_mount_points: Option<Vec<PathBuf>>, race: bool) -> JoinHandle<Result<(), Error>> {
        self.prev_error = self.error.take();
        let (supervisor_tx, supervisor_rx) = mpsc::channel(256);
        self.supervisor_tx = Some(supervisor_tx);
        tokio::spawn(kind.run(self.name.clone(), worker_tx, supervisor_rx, rando_rev, setup.clone(), output_mode, min_disk, min_disk_percent, min_disk_mount_points, race))
    }

    pub(crate) async fn roll(&mut self, seed_states: &mut [SeedState], seed_idx: SeedIdx) -> Result<(), mpsc::error::SendError<SupervisorMessage>> {
        self.supervisor_tx.as_ref().expect("attempted to roll a seed on an uninitialized worker").send(SupervisorMessage::Roll(seed_idx)).await?;
        self.ready -= 1;
        if let SeedState::Rolling { ref mut workers } = seed_states[usize::from(seed_idx)] {
            workers.push(self.name.clone());
        } else {
            seed_states[usize::from(seed_idx)] = SeedState::Rolling { workers: nev![self.name.clone()] };
        }
        Ok(())
    }
}
