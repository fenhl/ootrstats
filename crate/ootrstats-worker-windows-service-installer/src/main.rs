use {
    directories::UserDirs,
    windows_service::{
        service::*,
        service_manager::{
            ServiceManager,
            ServiceManagerAccess,
        },
    },
};

fn main() -> windows_service::Result<()> {
    let service_manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    let service_info = ServiceInfo {
        name: "ootrstats_worker".into(),
        display_name: "ootrstats worker".into(),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: UserDirs::new().expect("failed to determine user folder path").home_dir().join(".cargo").join("bin").join("ootrstats-worker-windows-service.exe"),
        launch_arguments: Vec::default(),
        dependencies: Vec::default(),
        account_name: None,
        account_password: None,
    };
    let service = service_manager.create_service(&service_info, ServiceAccess::CHANGE_CONFIG)?;
    service.set_description("Ocarina of Time Randomizer stats worker")?;
    Ok(())
}
