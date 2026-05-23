use anyhow::Result;
use std::env;

pub fn install() -> Result<()> {
    let exe = env::current_exe()?;

    #[cfg(target_os = "macos")]
    {
        use anyhow::bail;
        let exe_str = exe.to_string_lossy();
        let plist_dir = dirs::home_dir().unwrap().join("Library/LaunchAgents");
        std::fs::create_dir_all(&plist_dir)?;
        let plist_path = plist_dir.join("com.PowerEXT.plist");
        std::fs::write(&plist_path, format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.PowerEXT</string>
  <key>ProgramArguments</key>
  <array><string>{exe_str}</string><string>start</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>/tmp/PowerEXT.log</string>
  <key>StandardErrorPath</key><string>/tmp/PowerEXT.err</string>
</dict></plist>"#))?;
        let status = std::process::Command::new("launchctl")
            .args(["load", &plist_path.to_string_lossy()])
            .status()?;
        if !status.success() { bail!("launchctl load failed"); }
        println!("Installed and started via launchd.");
    }

    #[cfg(target_os = "linux")]
    {
        use anyhow::bail;
        let exe_str = exe.to_string_lossy();
        let svc_dir = dirs::home_dir().unwrap().join(".config/systemd/user");
        std::fs::create_dir_all(&svc_dir)?;
        std::fs::write(svc_dir.join("PowerEXT.service"), format!(
            "[Unit]\nDescription=PowerEXT filesystem monitor\n\n[Service]\nExecStart={exe_str} start\nRestart=always\n\n[Install]\nWantedBy=default.target\n"
        ))?;
        std::process::Command::new("systemctl").args(["--user", "daemon-reload"]).status()?;
        let status = std::process::Command::new("systemctl")
            .args(["--user", "enable", "--now", "PowerEXT"])
            .status()?;
        if !status.success() { bail!("systemctl enable failed"); }
        println!("Installed and started via systemd user service.");
    }

    #[cfg(target_os = "windows")]
    {
        use windows_service::{service::*, service_manager::*};
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CREATE_SERVICE)?;
        manager.create_service(
            &ServiceInfo {
                name: "PowerEXT".into(),
                display_name: "PowerEXT filesystem monitor".into(),
                service_type: ServiceType::OWN_PROCESS,
                start_type: ServiceStartType::AutoStart,
                error_control: ServiceErrorControl::Normal,
                executable_path: exe.clone(),
                launch_arguments: vec!["start".into()],
                dependencies: vec![],
                account_name: None,
                account_password: None,
            },
            ServiceAccess::all(),
        )?;
        println!("Installed as Windows service. Start with: sc start PowerEXT");
    }

    Ok(())
}

pub fn uninstall() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let plist_path = dirs::home_dir().unwrap().join("Library/LaunchAgents/com.PowerEXT.plist");
        std::process::Command::new("launchctl").args(["unload", &plist_path.to_string_lossy()]).status()?;
        std::fs::remove_file(&plist_path)?;
        println!("Uninstalled launchd agent.");
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("systemctl").args(["--user", "disable", "--now", "PowerEXT"]).status()?;
        let _ = std::fs::remove_file(dirs::home_dir().unwrap().join(".config/systemd/user/PowerEXT.service"));
        std::process::Command::new("systemctl").args(["--user", "daemon-reload"]).status()?;
        println!("Uninstalled systemd user service.");
    }
    #[cfg(target_os = "windows")]
    {
        use windows_service::{service_manager::*};
        let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
        let svc = manager.open_service("PowerEXT", windows_service::service::ServiceAccess::DELETE)?;
        svc.delete()?;
        println!("Uninstalled Windows service.");
    }
    Ok(())
}

pub fn status() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("launchctl").args(["list", "com.PowerEXT"]).output()?;
        print!("{}", String::from_utf8_lossy(&out.stdout));
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("systemctl").args(["--user", "status", "PowerEXT"]).status()?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("sc").args(["query", "PowerEXT"]).status()?;
    }
    Ok(())
}
