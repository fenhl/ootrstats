#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use {
    std::{
        ffi::OsString,
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
    #[error(transparent)] Rocket(#[from] rocket::Error),
    #[error(transparent)] Service(#[from] windows_service::Error),
}

async fn run_service() -> std::result::Result<(), Error> {
    let rocket = ootrstats_worker_daemon::rocket().await?;
    let mut shutdown = Some(rocket.shutdown());
    let event_handler = move |control_event| match control_event {
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        ServiceControl::Stop | ServiceControl::Preshutdown | ServiceControl::Shutdown => {
            if let Some(shutdown) = shutdown.take() {
                shutdown.notify();
            }
            ServiceControlHandlerResult::NoError
        }
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status_handle = service_control_handler::register("ootrstats_worker", event_handler)?;
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(1),
        process_id: None,
    })?;
    rocket.launch().await?;
    status_handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::STOP,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::from_secs(1),
        process_id: None,
    })?;
    Ok(())
}

fn service_main(_: Vec<OsString>) {
    rocket::async_main(run_service()).expect("error in ootrstats worker daemon") //TODO log error to file
}

define_windows_service!(ffi_service_main, service_main);

fn main() -> Result<()> {
    service_dispatcher::start("ootrstats_worker", ffi_service_main)?;
    Ok(())
}
