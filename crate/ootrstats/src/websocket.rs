use {
    async_proto::Protocol,
    bytes::Bytes,
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
        wsl_base_rom_path: Option<String>,
        rando_rev: gix_hash::ObjectId,
        setup: RandoSetup,
        output_mode: OutputMode,
        priority_users: Vec<String>,
        wsl_distro: Option<String>,
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
        spoiler_log: Bytes,
        patch: Option<(String, Bytes)>,
    },
    Failure {
        seed_idx: SeedIdx,
        /// present if the `bench` parameter was set and `perf` output was parsed successfully.
        instructions: Result<u64, Bytes>,
        error_log: Bytes,
    },
    Error {
        display: String,
        debug: String,
    },
    Ping,
}
