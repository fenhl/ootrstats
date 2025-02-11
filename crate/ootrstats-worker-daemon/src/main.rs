#![allow(unused_crate_dependencies)] // lib/bin combo crate

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[wheel::main(rocket)]
async fn main() -> Result<(), ootrstats_worker_daemon::MainError> {
    ootrstats_worker_daemon::main().await
}
