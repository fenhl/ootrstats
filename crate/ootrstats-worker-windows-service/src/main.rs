#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use {
    std::{
        ffi::OsString,
        result::Result,
        time::Duration,
    },
    windows_service::{
        *,
        service::*,
        service_control_handler::ServiceControlHandlerResult,
    },
};

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)] Main(#[from] ootrstats_worker_daemon::MainError),
    #[error(transparent)] Service(#[from] windows_service::Error),
}

async fn run_service() -> Result<(), Error> {
    let event_handler = |control_event| match control_event {
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        //TODO clean shutdown on ServiceControl::Stop
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status_handle = service_control_handler::register("ootrstats_worker", event_handler)?;
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::default(),
        process_id: None,
    })?;
    ootrstats_worker_daemon::main().await?;
    Ok(())
}

fn service_main(_: Vec<OsString>) {
    rocket::async_main(run_service()).expect("error in ootrstats worker daemon")
}

define_windows_service!(ffi_service_main, service_main);

fn main() -> windows_service::Result<()> {
    service_dispatcher::start("ootrstats_worker", ffi_service_main)?;
    Ok(())
}
