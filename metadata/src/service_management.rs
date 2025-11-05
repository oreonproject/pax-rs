use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    process::Command,
};

use utils::{err, get_metadata_dir};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceDefinition {
    pub service_name: String,
    pub package_name: String,
    pub service_type: ServiceType,
    pub unit_file: PathBuf,
    pub enabled: bool,
    pub running: bool,
    pub auto_start: bool,
    pub restart_policy: RestartPolicy,
    pub dependencies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServiceType {
    Systemd,
    SysVInit,
    Upstart,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RestartPolicy {
    Always,
    OnFailure,
    Never,
    UnlessStopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub service_name: String,
    pub status: ServiceState,
    pub pid: Option<u32>,
    pub memory_usage: Option<u64>,
    pub cpu_usage: Option<f64>,
    pub uptime: Option<u64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ServiceState {
    Active,
    Inactive,
    Failed,
    Activating,
    Deactivating,
    Unknown,
}

pub struct ServiceManager {
    services: HashMap<String, ServiceDefinition>,
    service_statuses: HashMap<String, ServiceStatus>,
}

impl ServiceManager {
    pub fn new() -> Self {
        Self {
            services: HashMap::new(),
            service_statuses: HashMap::new(),
        }
    }

    pub fn register_service(&mut self, service: ServiceDefinition) -> Result<(), String> {
        let service_name = service.service_name.clone();
        self.services.insert(service_name.clone(), service);
        
        // Initialize service status
        let status = ServiceStatus {
            service_name: service_name.clone(),
            status: ServiceState::Unknown,
            pid: None,
            memory_usage: None,
            cpu_usage: None,
            uptime: None,
            last_error: None,
        };
        self.service_statuses.insert(service_name, status);
        
        self.save_services()?;
        Ok(())
    }

    pub fn start_service(&mut self, service_name: &str) -> Result<(), String> {
        let service = self.services.get(service_name)
            .ok_or_else(|| format!("Service {} not found", service_name))?;

        match service.service_type {
            ServiceType::Systemd => {
                self.start_systemd_service(service_name)?;
            }
            ServiceType::SysVInit => {
                self.start_sysv_service(service_name)?;
            }
            ServiceType::Upstart => {
                self.start_upstart_service(service_name)?;
            }
            ServiceType::Custom => {
                self.start_custom_service(service_name)?;
            }
        }

        // Update service status
        if let Some(status) = self.service_statuses.get_mut(service_name) {
            status.status = ServiceState::Active;
            status.last_error = None;
        }

        Ok(())
    }

    pub fn stop_service(&mut self, service_name: &str) -> Result<(), String> {
        let service = self.services.get(service_name)
            .ok_or_else(|| format!("Service {} not found", service_name))?;

        match service.service_type {
            ServiceType::Systemd => {
                self.stop_systemd_service(service_name)?;
            }
            ServiceType::SysVInit => {
                self.stop_sysv_service(service_name)?;
            }
            ServiceType::Upstart => {
                self.stop_upstart_service(service_name)?;
            }
            ServiceType::Custom => {
                self.stop_custom_service(service_name)?;
            }
        }

        // Update service status
        if let Some(status) = self.service_statuses.get_mut(service_name) {
            status.status = ServiceState::Inactive;
            status.pid = None;
        }

        Ok(())
    }

    pub fn restart_service(&mut self, service_name: &str) -> Result<(), String> {
        self.stop_service(service_name)?;
        std::thread::sleep(std::time::Duration::from_secs(1)); // Brief pause
        self.start_service(service_name)?;
        Ok(())
    }

    pub fn enable_service(&mut self, service_name: &str) -> Result<(), String> {
        let service = self.services.get_mut(service_name)
            .ok_or_else(|| format!("Service {} not found", service_name))?;

        match service.service_type {
            ServiceType::Systemd => {
                let output = Command::new("systemctl")
                    .args(&["enable", service_name])
                    .output()
                    .map_err(|_| "Failed to execute systemctl enable")?;

                if !output.status.success() {
                    return err!("Failed to enable systemd service: {}", 
                        String::from_utf8_lossy(&output.stderr));
                }
            }
            ServiceType::SysVInit => {
                // Enable SysV init service
                let output = Command::new("update-rc.d")
                    .args(&[service_name, "defaults"])
                    .output()
                    .map_err(|_| "Failed to execute update-rc.d")?;

                if !output.status.success() {
                    return err!("Failed to enable SysV service: {}", 
                        String::from_utf8_lossy(&output.stderr));
                }
            }
            ServiceType::Upstart => {
                // Upstart services are typically enabled by default
                println!("Upstart services are typically enabled by default");
            }
            ServiceType::Custom => {
                // Custom service enabling logic
                println!("Custom service enabling not implemented");
            }
        }

        service.enabled = true;
        self.save_services()?;
        Ok(())
    }

    pub fn disable_service(&mut self, service_name: &str) -> Result<(), String> {
        let service = self.services.get_mut(service_name)
            .ok_or_else(|| format!("Service {} not found", service_name))?;

        match service.service_type {
            ServiceType::Systemd => {
                let output = Command::new("systemctl")
                    .args(&["disable", service_name])
                    .output()
                    .map_err(|_| "Failed to execute systemctl disable")?;

                if !output.status.success() {
                    return err!("Failed to disable systemd service: {}", 
                        String::from_utf8_lossy(&output.stderr));
                }
            }
            ServiceType::SysVInit => {
                let output = Command::new("update-rc.d")
                    .args(&["-f", service_name, "remove"])
                    .output()
                    .map_err(|_| "Failed to execute update-rc.d")?;

                if !output.status.success() {
                    return err!("Failed to disable SysV service: {}", 
                        String::from_utf8_lossy(&output.stderr));
                }
            }
            ServiceType::Upstart => {
                println!("Upstart service disabling not implemented");
            }
            ServiceType::Custom => {
                println!("Custom service disabling not implemented");
            }
        }

        service.enabled = false;
        self.save_services()?;
        Ok(())
    }

    pub fn get_service_status(&mut self, service_name: &str) -> Result<ServiceStatus, String> {
        let service = self.services.get(service_name)
            .ok_or_else(|| format!("Service {} not found", service_name))?;

        let mut status = self.service_statuses.get(service_name)
            .cloned()
            .unwrap_or_else(|| ServiceStatus {
                service_name: service_name.to_string(),
                status: ServiceState::Unknown,
                pid: None,
                memory_usage: None,
                cpu_usage: None,
                uptime: None,
                last_error: None,
            });

        // Update status based on service type
        match service.service_type {
            ServiceType::Systemd => {
                self.update_systemd_status(service_name, &mut status)?;
            }
            ServiceType::SysVInit => {
                self.update_sysv_status(service_name, &mut status)?;
            }
            ServiceType::Upstart => {
                self.update_upstart_status(service_name, &mut status)?;
            }
            ServiceType::Custom => {
                self.update_custom_status(service_name, &mut status)?;
            }
        }

        self.service_statuses.insert(service_name.to_string(), status.clone());
        Ok(status)
    }

    pub fn list_services(&self) -> Vec<&ServiceDefinition> {
        let mut services: Vec<&ServiceDefinition> = self.services.values().collect();
        services.sort_by(|a, b| a.service_name.cmp(&b.service_name));
        services
    }

    pub fn get_services_for_package(&self, package_name: &str) -> Vec<&ServiceDefinition> {
        self.services.values()
            .filter(|service| service.package_name == package_name)
            .collect()
    }

    fn start_systemd_service(&self, service_name: &str) -> Result<(), String> {
        let output = Command::new("systemctl")
            .args(&["start", service_name])
            .output()
            .map_err(|_| "Failed to execute systemctl start")?;

        if !output.status.success() {
            return err!("Failed to start systemd service: {}", 
                String::from_utf8_lossy(&output.stderr));
        }

        Ok(())
    }

    fn stop_systemd_service(&self, service_name: &str) -> Result<(), String> {
        let output = Command::new("systemctl")
            .args(&["stop", service_name])
            .output()
            .map_err(|_| "Failed to execute systemctl stop")?;

        if !output.status.success() {
            return err!("Failed to stop systemd service: {}", 
                String::from_utf8_lossy(&output.stderr));
        }

        Ok(())
    }

    fn start_sysv_service(&self, service_name: &str) -> Result<(), String> {
        let output = Command::new("service")
            .args(&[service_name, "start"])
            .output()
            .map_err(|_| "Failed to execute service start")?;

        if !output.status.success() {
            return err!("Failed to start SysV service: {}", 
                String::from_utf8_lossy(&output.stderr));
        }

        Ok(())
    }

    fn stop_sysv_service(&self, service_name: &str) -> Result<(), String> {
        let output = Command::new("service")
            .args(&[service_name, "stop"])
            .output()
            .map_err(|_| "Failed to execute service stop")?;

        if !output.status.success() {
            return err!("Failed to stop SysV service: {}", 
                String::from_utf8_lossy(&output.stderr));
        }

        Ok(())
    }

    fn start_upstart_service(&self, service_name: &str) -> Result<(), String> {
        let output = Command::new("start")
            .arg(service_name)
            .output()
            .map_err(|_| "Failed to execute start command")?;

        if !output.status.success() {
            return err!("Failed to start Upstart service: {}", 
                String::from_utf8_lossy(&output.stderr));
        }

        Ok(())
    }

    fn stop_upstart_service(&self, service_name: &str) -> Result<(), String> {
        let output = Command::new("stop")
            .arg(service_name)
            .output()
            .map_err(|_| "Failed to execute stop command")?;

        if !output.status.success() {
            return err!("Failed to stop Upstart service: {}", 
                String::from_utf8_lossy(&output.stderr));
        }

        Ok(())
    }

    fn start_custom_service(&self, service_name: &str) -> Result<(), String> {
        // Custom service start logic would go here
        println!("Starting custom service: {}", service_name);
        Ok(())
    }

    fn stop_custom_service(&self, service_name: &str) -> Result<(), String> {
        // Custom service stop logic would go here
        println!("Stopping custom service: {}", service_name);
        Ok(())
    }

    fn update_systemd_status(&self, service_name: &str, status: &mut ServiceStatus) -> Result<(), String> {
        let output = Command::new("systemctl")
            .args(&["is-active", service_name])
            .output()
            .map_err(|_| "Failed to execute systemctl is-active")?;

        let is_active = String::from_utf8_lossy(&output.stdout).trim() == "active";
        
        status.status = if is_active {
            ServiceState::Active
        } else {
            ServiceState::Inactive
        };

        // Get more detailed status
        let status_output = Command::new("systemctl")
            .args(&["show", service_name, "--property=MainPID,MemoryCurrent,CPUUsageNSec"])
            .output()
            .ok();

        if let Some(output) = status_output {
            let status_text = String::from_utf8_lossy(&output.stdout);
            for line in status_text.lines() {
                if line.starts_with("MainPID=") {
                    if let Some(pid_str) = line.strip_prefix("MainPID=") {
                        status.pid = pid_str.parse().ok();
                    }
                } else if line.starts_with("MemoryCurrent=") {
                    if let Some(mem_str) = line.strip_prefix("MemoryCurrent=") {
                        status.memory_usage = mem_str.parse().ok();
                    }
                }
            }
        }

        Ok(())
    }

    fn update_sysv_status(&self, service_name: &str, status: &mut ServiceStatus) -> Result<(), String> {
        let output = Command::new("service")
            .args(&[service_name, "status"])
            .output()
            .map_err(|_| "Failed to execute service status")?;

        let status_text = String::from_utf8_lossy(&output.stdout);
        
        status.status = if status_text.contains("running") || status_text.contains("active") {
            ServiceState::Active
        } else {
            ServiceState::Inactive
        };

        Ok(())
    }

    fn update_upstart_status(&self, service_name: &str, status: &mut ServiceStatus) -> Result<(), String> {
        let output = Command::new("status")
            .arg(service_name)
            .output()
            .map_err(|_| "Failed to execute status command")?;

        let status_text = String::from_utf8_lossy(&output.stdout);
        
        status.status = if status_text.contains("running") || status_text.contains("start/running") {
            ServiceState::Active
        } else {
            ServiceState::Inactive
        };

        Ok(())
    }

    fn update_custom_status(&self, _service_name: &str, status: &mut ServiceStatus) -> Result<(), String> {
        // Custom status checking logic would go here
        status.status = ServiceState::Unknown;
        Ok(())
    }

    fn save_services(&self) -> Result<(), String> {
        let mut services_path = get_metadata_dir()?;
        services_path.push("services.yaml");

        let mut file = File::create(&services_path)
            .map_err(|_| "Failed to create services file")?;

        let yaml = serde_norway::to_string(&self.services)
            .map_err(|_| "Failed to serialize services")?;

        file.write_all(yaml.as_bytes())
            .map_err(|_| "Failed to write services file")?;

        Ok(())
    }

    pub fn load_services(&mut self) -> Result<(), String> {
        let mut services_path = get_metadata_dir()?;
        services_path.push("services.yaml");

        if services_path.exists() {
            let mut file = File::open(&services_path)
                .map_err(|_| "Failed to open services file")?;

            let mut contents = String::new();
            file.read_to_string(&mut contents)
                .map_err(|_| "Failed to read services file")?;

            self.services = serde_norway::from_str(&contents)
                .map_err(|_| "Failed to parse services file")?;
        }

        Ok(())
    }
}

impl Default for ServiceManager {
    fn default() -> Self {
        Self::new()
    }
}
