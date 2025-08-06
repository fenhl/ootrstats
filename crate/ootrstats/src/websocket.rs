use {
    async_proto::Protocol,
    bytes::Bytes,
    bytesize::ByteSize,
    crate::{
        OutputMode,
        RandoSetup,
        SeedIdx,
        worker::SupervisorMessage,
    },
};

#[derive(Protocol)]
pub enum ClientMessage {
    Handshake {
        password: String,
        base_rom_path: String,
        rando_rev: gix_hash::ObjectId,
        setup: RandoSetup,
        output_mode: OutputMode,
        min_disk: ByteSize,
        min_disk_percent: f64,
        min_disk_mount_points: Option<Vec<String>>,
        priority_users: Vec<String>,
        race: bool,
        wsl_distro: Option<String>,
        hide_reboot: bool,
        hide_sleep: bool,
    },
    Supervisor(SupervisorMessage),
    Ping,
    Goodbye,
}

#[derive(Protocol)]
pub enum ServerMessage {
    Init(String),
    Ready(u8),
    Success {
        seed_idx: SeedIdx,
        /// present if the `bench` parameter was set and `perf` output was parsed successfully.
        instructions: Result<u64, Bytes>,
        rsl_instructions: Result<u64, Bytes>,
        spoiler_log: Bytes,
        patch: Option<(String, Bytes)>,
        compressed_rom: Option<Bytes>,
        uncompressed_rom: Option<Bytes>,
        rsl_plando: Option<Bytes>,
    },
    Failure {
        seed_idx: SeedIdx,
        /// present if the `bench` parameter was set and `perf` output was parsed successfully.
        instructions: Result<u64, Bytes>,
        rsl_instructions: Result<u64, Bytes>,
        error_log: Bytes,
        rsl_plando: Option<Bytes>,
    },
    Error {
        display: String,
        debug: String,
    },
    Ping,
}
