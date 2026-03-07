#![allow(unused_crate_dependencies)] // lib/bin combo crate

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[wheel::main(rocket)]
async fn main() -> Result<(), ootrstats_worker_daemon::MainError> {
    let default_panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = wheel::night_report_sync("/net/ootrstats/error", Some("thread panic"));
        default_panic_hook(info)
    }));
    ootrstats_worker_daemon::rocket().await?.launch().await?;
    Ok(())
}
