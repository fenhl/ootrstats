use {
    std::{
        thread::sleep,
        time::{
            Duration,
            Instant,
        },
    },
    windows_service::{
        service::*,
        service_manager::{
            ServiceManager,
            ServiceManagerAccess,
        },
    },
    windows_sys::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST,
};

fn main() -> windows_service::Result<()> {
    let service_manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = service_manager.open_service("ootrstats_worker", ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE)?;
    service.delete()?;
    if service.query_status()?.current_state != ServiceState::Stopped {
        service.stop()?;
    }
    drop(service);
    let start = Instant::now();
    let timeout = Duration::from_secs(5);
    while start.elapsed() < timeout {
        if let Err(windows_service::Error::Winapi(e)) = service_manager.open_service("ootrstats_worker", ServiceAccess::QUERY_STATUS) {
            if e.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) {
                println!("ootrstats_worker is deleted.");
                return Ok(());
            }
        }
        sleep(Duration::from_secs(1));
    }
    Ok(())
}
