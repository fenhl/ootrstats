use {
    async_proto::Protocol,
    bytes::Bytes,
    crate::{
        RandoSettings,
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
        rando_rev: git2::Oid,
        settings: RandoSettings,
        bench: bool,
    },
    Supervisor(SupervisorMessage),
}

#[derive(Protocol)]
pub enum ServerMessage {
    Init(String),
    Ready(u8),
    Success {
        seed_idx: SeedIdx,
        /// present iff the `bench` parameter was set.
        instructions: Option<u64>,
        spoiler_log: Bytes,
        ready: bool,
    },
    Failure {
        seed_idx: SeedIdx,
        /// present iff the `bench` parameter was set.
        instructions: Option<u64>,
        error_log: Bytes,
        ready: bool,
    },
}
