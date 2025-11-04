use commands::Command;
use flags::Flag;
use metadata::get_packages;
use settings::{SettingsYaml, acquire_lock, OriginKind};
use statebox::StateBox;
use tokio::runtime::Runtime;
use utils::{PostAction, choice};
use std::path::{Path, PathBuf};
use std::fs;
use std::process::Command as RunCommand;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IsoTemplate {
    packages: Option<Vec<String>>,
    repositories: Option<Vec<TemplateRepository>>,
    config: Option<TemplateConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TemplateRepository {
    #[serde(rename = "type")]
    repo_type: String,
    url: Option<String>,
    path: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TemplateConfig {
    hostname: Option<String>,
    username: Option<String>,
    password: Option<String>,
    #[serde(flatten)]
    extra: serde_json::Value,
}

pub fn build(hierarchy: &[String]) -> Command {
    let output = Flag::new(
        Some('o'),
        "output",
        "Output ISO file path (default: oreon-live.iso)",
        true,
        false,
        |states, value| {
            if let Some(output) = value {
                states.shove("output", output);
            }
        },
    );
    
    let packages = Flag::new(
        Some('p'),
        "packages",
        "Comma-separated list of packages to include in the ISO",
        true,
        false,
        |states, value| {
            if let Some(packages) = value {
                states.shove("packages", packages);
            }
        },
    );
    
    let template = Flag::new(
        Some('t'),
        "template",
        "Path to ISO template file (YAML or JSON)",
        true,
        false,
        |states, value| {
            if let Some(template) = value {
                states.shove("template", template);
            }
        },
    );
    
    Command::new(
        "isocreate",
        vec![],
        "Build a live ISO image for Oreon or other pax-based distros",
        vec![output, packages, template, utils::yes_flag()],
        None,
        run,
        hierarchy,
    )
}

fn run(states: &StateBox, _args: Option<&[String]>) -> PostAction {
    match acquire_lock() {
        Ok(Some(action)) => return action,
        Err(fault) => return PostAction::Fuck(fault),
        _ => (),
    }
    
    if !utils::is_root() {
        return PostAction::Elevate;
    }
    
    let output_path = states.get::<String>("output")
        .map(|s| PathBuf::from(s.as_str()))
        .unwrap_or_else(|| PathBuf::from("oreon-live.iso"));
    
    // Load template file if provided
    let template = if let Some(template_path) = states.get::<String>("template") {
        match load_template(&PathBuf::from(template_path.as_str())) {
            Ok(t) => Some(t),
            Err(e) => return PostAction::Fuck(format!("Failed to load template: {}", e)),
        }
    } else {
        None
    };
    
    // Get packages from template or command line
    let packages_str = states.get::<String>("packages")
        .map(|s| s.as_str().to_string())
        .unwrap_or_else(|| String::from(""));
    
    let package_list: Vec<String> = if let Some(ref tmpl) = template {
        tmpl.packages.clone().unwrap_or_default()
    } else if !packages_str.is_empty() {
        packages_str
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        // Default packages for a minimal live system
        vec![
            "busybox".to_string(),
            "bash".to_string(),
            "coreutils".to_string(),
        ]
    };
    
    // Get repositories from template or use system settings
    let repositories: Vec<OriginKind> = if let Some(ref tmpl) = template {
        tmpl.repositories
            .as_ref()
            .map(|repos| parse_template_repositories(repos))
            .unwrap_or_else(|| {
                let settings = SettingsYaml::get_settings().ok();
                settings.map(|s| s.sources).unwrap_or_default()
            })
    } else {
        let settings = match SettingsYaml::get_settings() {
            Ok(settings) => settings,
            Err(_) => return PostAction::PullSources,
        };
        if settings.sources.is_empty() && settings.mirror_list.is_none() {
            return PostAction::PullSources;
        }
        settings.sources
    };
    
    println!("Building live ISO image...");
    println!("Output: {}", output_path.display());
    println!("Packages to include: {}", package_list.join(", "));
    if !repositories.is_empty() {
        println!("Repositories: {}", repositories.len());
    }
    
    if states.get("yes").is_none_or(|x: &bool| !*x) {
        match choice("Proceed with ISO creation?", true) {
            Err(message) => return PostAction::Fuck(message),
            Ok(false) => return PostAction::Fuck(String::from("Aborted.")),
            Ok(true) => (),
        }
    }
    
    let Ok(runtime) = Runtime::new() else {
        return PostAction::Fuck(String::from("Error creating runtime!"));
    };
    
    match build_iso(&runtime, &package_list, &repositories, &output_path, template.as_ref()) {
        Ok(()) => {
            println!("\n\x1B[92mISO created successfully: {}\x1B[0m", output_path.display());
            PostAction::Return
        }
        Err(fault) => PostAction::Fuck(fault),
    }
}

fn load_template(path: &Path) -> Result<IsoTemplate, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read template file: {}", e))?;
    
    if path.extension().and_then(|s| s.to_str()) == Some("json") {
        serde_json::from_str::<IsoTemplate>(&content)
            .map_err(|e| format!("Failed to parse JSON template: {}", e))
    } else {
        serde_yaml::from_str::<IsoTemplate>(&content)
            .map_err(|e| format!("Failed to parse YAML template: {}", e))
    }
}

fn parse_template_repositories(repos: &[TemplateRepository]) -> Vec<OriginKind> {
    let mut result = Vec::new();
    
    for repo in repos {
        let origin = match repo.repo_type.to_lowercase().as_str() {
            "pax" | "pax-repo" => {
                if let Some(url) = &repo.url {
                    Some(OriginKind::Pax(url.clone()))
                } else {
                    None
                }
            },
            "apt" | "deb" => {
                if let Some(url) = &repo.url {
                    Some(OriginKind::Apt(url.clone()))
                } else {
                    None
                }
            },
            "rpm" | "yum" | "dnf" => {
                if let Some(url) = &repo.url {
                    Some(OriginKind::Yum(url.clone()))
                } else {
                    None
                }
            },
            "github" => {
                // Parse GitHub repo from url or extra fields
                if let Some(url) = &repo.url {
                    if let Some((user, repo_name)) = url.trim_start_matches("https://github.com/")
                        .trim_start_matches("http://github.com/")
                        .split_once('/')
                    {
                        Some(OriginKind::Github {
                            user: user.to_string(),
                            repo: repo_name.trim_end_matches(".git").to_string(),
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            },
            "local" | "localdir" | "directory" | "dir" | "file" => {
                let dir_path = repo.path.as_ref()
                    .or_else(|| repo.url.as_ref())
                    .map(|s| {
                        if s.starts_with("file://") {
                            s.strip_prefix("file://").unwrap()
                        } else {
                            s
                        }
                    });
                
                if let Some(path) = dir_path {
                    let dir = Path::new(path);
                    if dir.exists() && dir.is_dir() {
                        Some(OriginKind::LocalDir(path.to_string()))
                    } else {
                        eprintln!("Warning: Local directory repository does not exist: {}", path);
                        None
                    }
                } else {
                    None
                }
            },
            "cloudflare-r2" | "r2" => {
                // Parse R2 configuration
                let bucket = repo.extra.get("bucket")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let account_id = repo.extra.get("account_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                
                if let (Some(bucket), Some(account_id)) = (bucket, account_id) {
                    Some(OriginKind::CloudflareR2 {
                        bucket,
                        account_id,
                        access_key_id: repo.extra.get("access_key_id")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        secret_access_key: repo.extra.get("secret_access_key")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        region: repo.extra.get("region")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                    })
                } else {
                    None
                }
            },
            _ => {
                eprintln!("Warning: Unknown repository type: {}", repo.repo_type);
                None
            }
        };
        
        if let Some(origin) = origin {
            result.push(origin);
        }
    }
    
    result
}

fn build_iso(
    runtime: &Runtime,
    package_list: &[String],
    repositories: &[OriginKind],
    output_path: &Path,
    template: Option<&IsoTemplate>,
) -> Result<(), String> {
    // Create temporary directory for ISO structure
    let temp_dir = tempfile::tempdir()
        .map_err(|e| format!("Failed to create temp directory: {}", e))?;
    let iso_root = temp_dir.path().join("iso-root");
    fs::create_dir_all(&iso_root)
        .map_err(|e| format!("Failed to create ISO root: {}", e))?;
    
    // Create rootfs directory for squashfs
    let rootfs_dir = temp_dir.path().join("rootfs");
    fs::create_dir_all(&rootfs_dir)
        .map_err(|e| format!("Failed to create rootfs directory: {}", e))?;
    
    println!("Creating root filesystem structure...");
    
    // Create basic filesystem structure in rootfs
    let dirs = [
        "bin", "sbin", "usr/bin", "usr/sbin", "usr/lib", "lib", "lib64",
        "etc", "var", "tmp", "root", "home", "proc", "sys", "dev", "mnt",
        "boot", "boot/grub", "boot/grub/i386-pc", "boot/grub/x86_64-efi",
        "live", // For live ISO mounting
    ];
    
    for dir in &dirs {
        let dir_path = rootfs_dir.join(dir);
        fs::create_dir_all(&dir_path)
            .map_err(|e| format!("Failed to create directory {}: {}", dir, e))?;
    }
    
    // Fetch and install packages to rootfs
    if !package_list.is_empty() {
        println!("Fetching packages from {} repository(ies)...", repositories.len());
        let remote_data = runtime.block_on(fetch_packages_from_repos(
            package_list.to_vec(),
            repositories,
        ))
        .map_err(|e| format!("Failed to get packages: {}", e))?;
        
        // Validate that critical packages were found
        let found_package_names_lower: std::collections::HashSet<String> = remote_data
            .iter()
            .map(|p| p.metadata.name.to_lowercase())
            .collect();
        
        let requested_package_names_lower: std::collections::HashSet<String> = package_list
            .iter()
            .map(|s| s.to_lowercase())
            .collect();
        
        // Find missing packages
        let missing_packages: Vec<String> = requested_package_names_lower
            .iter()
            .filter(|name| !found_package_names_lower.contains(*name))
            .cloned()
            .collect();
        
        // Check for missing critical packages (kernel-related)
        let kernel_package_names = vec!["linux-kernel", "kernel", "linux", "linux-image"];
        let mut missing_kernel_packages = Vec::new();
        let mut found_kernel_package = false;
        
        for kernel_name in &kernel_package_names {
            let kernel_name_lower = kernel_name.to_lowercase();
            if requested_package_names_lower.contains(&kernel_name_lower) {
                if !found_package_names_lower.contains(&kernel_name_lower) {
                    missing_kernel_packages.push(kernel_name.to_string());
                } else {
                    found_kernel_package = true;
                }
            }
        }
        
        // Also check if any kernel-related package was found (even if not specifically requested)
        if !found_kernel_package {
            found_kernel_package = found_package_names_lower.iter().any(|n| {
                n.contains("kernel") || n.contains("linux-image") || n.starts_with("linux-")
            });
        }
        
        // Warn about missing packages
        if !missing_packages.is_empty() {
            eprintln!("\n\x1B[93mWARNING: The following packages were not found in repositories:\x1B[0m");
            for pkg in &missing_packages {
                eprintln!("  - {}", pkg);
            }
        }
        
        // Fail if critical kernel packages are missing
        if !missing_kernel_packages.is_empty() {
            let mut error_msg = format!(
                "\n\x1B[91mERROR: Critical kernel package(s) were requested but not found in repositories!\x1B[0m\n\n"
            );
            for pkg in &missing_kernel_packages {
                error_msg.push_str(&format!("  - {}\n", pkg));
            }
            error_msg.push_str("\nCannot build ISO without a kernel. Please ensure the kernel package exists in your repositories.\n");
            return Err(error_msg);
        }
        
        // Warn if no kernel package was found at all
        if !found_kernel_package && missing_packages.is_empty() {
            eprintln!("\n\x1B[93mWARNING: No kernel-related package detected in installed packages!\x1B[0m");
            eprintln!("The ISO may not be bootable without a kernel.\n");
        }
        
        println!("Installing packages to rootfs...");
        use std::io::Write;
        std::io::stdout().flush().unwrap();
        
        println!("[DEBUG] Fetched {} packages from repositories (requested {} packages)", 
            remote_data.len(), package_list.len());
        if !missing_packages.is_empty() {
            println!("[DEBUG] {} package(s) were not found", missing_packages.len());
        }
        std::io::stdout().flush().unwrap();
        
        for (idx, package) in remote_data.iter().enumerate() {
            println!("===== INSTALLING PACKAGE {} of {}: {} (version: {}) ======", 
                idx + 1, remote_data.len(), package.metadata.name, package.metadata.version);
            println!("[DEBUG] Package install_kind: {:?}", package.metadata.install_kind);
            std::io::stdout().flush().unwrap();
            install_package_to_root(runtime, package, &rootfs_dir)
                .map_err(|e| format!("Failed to install {}: {}", package.metadata.name, e))?;
            println!("===== COMPLETED PACKAGE: {} ======", package.metadata.name);
            std::io::stdout().flush().unwrap();
        }
    }
    
    // Create /etc/ld.so.conf to tell the dynamic linker where to find systemd libraries
    println!("Configuring dynamic linker paths...");
    let ld_so_conf = rootfs_dir.join("etc/ld.so.conf");
    let ld_so_conf_content = r#"# Dynamic linker configuration for Oreon
/lib
/lib64
/usr/lib
/usr/lib64
/usr/lib/systemd
/usr/lib64/systemd
/usr/local/lib
/usr/local/lib64
"#;
    if let Err(e) = fs::write(&ld_so_conf, ld_so_conf_content) {
        println!("Warning: Failed to create /etc/ld.so.conf: {}", e);
    } else {
        println!("Created /etc/ld.so.conf with systemd library paths");
    }
    
    // Run ldconfig to update library cache in rootfs
    println!("Updating library cache in rootfs...");
    let ldconfig_paths = vec![
        "/usr/sbin/ldconfig",
        "/sbin/ldconfig",
        "/usr/bin/ldconfig",
    ];
    
    let mut ldconfig_found = false;
    for ldconfig_path in &ldconfig_paths {
        if rootfs_dir.join(ldconfig_path.trim_start_matches('/')).exists() {
            println!("Running ldconfig at {}", ldconfig_path);
            let ldconfig_result = RunCommand::new("chroot")
                .arg(&rootfs_dir)
                .arg(ldconfig_path)
                .status();
            
            match ldconfig_result {
                Ok(status) if status.success() => {
                    println!("Library cache updated successfully");
                    ldconfig_found = true;
                    break;
                }
                Ok(status) => println!("Warning: ldconfig exited with status {}", status),
                Err(e) => println!("Warning: Failed to run ldconfig: {}", e),
            }
        }
    }
    
    if !ldconfig_found {
        println!("Warning: ldconfig not found in rootfs, library cache not updated");
    }
    
    // Create essential system files for live boot
    println!("Creating essential system files...");
    
    // Create essential directories for systemd
    println!("Creating essential system directories...");
    let dirs_to_create = [
        "var/lib", "var/cache", "var/log", "var/tmp",
        "run/systemd", "run/lock", "run/user",
        "run/systemd/seats", "run/systemd/sessions", "run/systemd/users",  // For systemd-logind
        "sys/fs/cgroup",
    ];
    for dir in &dirs_to_create {
        let dir_path = rootfs_dir.join(dir);
        fs::create_dir_all(&dir_path).ok();
    }
    
    // Create /etc/machine-id (required by systemd)
    let machine_id_path = rootfs_dir.join("etc/machine-id");
    if !machine_id_path.exists() {
        fs::write(&machine_id_path, "").ok();
        println!("Created empty /etc/machine-id");
    }
    
    // Create minimal /etc/fstab
    let fstab_path = rootfs_dir.join("etc/fstab");
    if !fstab_path.exists() {
        let fstab_content = "# Live CD fstab\noverlay / overlay defaults 0 0\ntmpfs /tmp tmpfs defaults 0 0\n";
        fs::write(&fstab_path, fstab_content).ok();
        println!("Created /etc/fstab");
    }
    
    // Ensure /sbin/init exists and points to systemd
    let init_link = rootfs_dir.join("sbin/init");
    if !init_link.exists() {
        fs::create_dir_all(rootfs_dir.join("sbin")).ok();
        if rootfs_dir.join("usr/lib/systemd/systemd").exists() {
            std::os::unix::fs::symlink("../usr/lib/systemd/systemd", &init_link).ok();
            println!("Created /sbin/init -> systemd symlink");
        }
    }
    
    // CRITICAL FIX: Ensure dynamic linker is accessible at /lib64/ld-linux-x86-64.so.2
    // Many binaries are compiled with this hardcoded interpreter path
    println!("Ensuring dynamic linker is accessible at /lib64/...");
    let lib64_dir = rootfs_dir.join("lib64");
    fs::create_dir_all(&lib64_dir).ok();
    
    let ld_dest = lib64_dir.join("ld-linux-x86-64.so.2");
    if !ld_dest.exists() {
        // Check common locations for ld-linux in the rootfs
        let ld_src_paths = vec![
            rootfs_dir.join("usr/lib/ld-linux-x86-64.so.2"),
            rootfs_dir.join("lib/ld-linux-x86-64.so.2"),
            rootfs_dir.join("lib64/ld-linux-x86-64.so.2"),
        ];
        
        for ld_src in ld_src_paths {
            if ld_src.exists() {
                // Create symlink from /lib64 to wherever the actual file is
                let relative_path = if ld_src.starts_with(rootfs_dir.join("usr/lib")) {
                    "../usr/lib/ld-linux-x86-64.so.2"
                } else if ld_src.starts_with(rootfs_dir.join("lib/")) {
                    "../lib/ld-linux-x86-64.so.2"
                } else {
                    "ld-linux-x86-64.so.2"
                };
                
                if std::os::unix::fs::symlink(relative_path, &ld_dest).is_ok() {
                    println!("Created /lib64/ld-linux-x86-64.so.2 -> {}", relative_path);
                }
                break;
            }
        }
    }
    
    // Also ensure critical glibc libraries are accessible in /lib64
    for lib_name in &["libc.so.6", "libm.so.6", "libresolv.so.2"] {
        let lib_dest = lib64_dir.join(lib_name);
        if !lib_dest.exists() {
            let usr_lib_src = rootfs_dir.join("usr/lib").join(lib_name);
            if usr_lib_src.exists() {
                let relative_path = format!("../usr/lib/{}", lib_name);
                std::os::unix::fs::symlink(&relative_path, &lib_dest).ok();
            }
        }
    }
    
    // CRITICAL: Create symlinks for systemd libraries in standard locations
    // This ensures they're found without needing ldconfig to run
    println!("Creating symlinks for systemd libraries...");
    let usr_lib64_dir = rootfs_dir.join("usr/lib64");
    if usr_lib64_dir.exists() {
        let systemd_lib_dir = usr_lib64_dir.join("systemd");
        if systemd_lib_dir.exists() {
            if let Ok(entries) = fs::read_dir(&systemd_lib_dir) {
                for entry in entries.flatten() {
                    let file_name = entry.file_name();
                    let name_str = file_name.to_string_lossy();
                    // Only symlink .so files
                    if name_str.starts_with("libsystemd-") && name_str.contains(".so") {
                        let src = entry.path();
                        let dest = usr_lib64_dir.join(&file_name);
                        if !dest.exists() && src.is_file() {
                            let relative_path = format!("systemd/{}", name_str);
                            if std::os::unix::fs::symlink(&relative_path, &dest).is_ok() {
                                println!("  Created /usr/lib64/{} -> systemd/{}", name_str, name_str);
                            }
                        }
                    }
                }
            }
        }
    }
    
    // Disable ONLY non-critical systemd services that cause errors on read-only parts
    println!("Masking non-critical systemd update services...");
    let systemd_system_dir = rootfs_dir.join("usr/lib/systemd/system");
    if systemd_system_dir.exists() {
        // Only mask services that try to write to read-only areas and aren't critical
        let services_to_mask = vec![
            "systemd-hwdb-update.service",      // Hardware database update - not critical
            "systemd-machine-id-commit.service", // Tries to write permanent machine-id
            "systemd-journal-catalog-update.service", // Journal catalog - not critical
            "systemd-update-done.service",      // Update marker - not critical  
        ];
        
        for service in &services_to_mask {
            let service_link = systemd_system_dir.join(service);
            if !service_link.exists() {
                if std::os::unix::fs::symlink("/dev/null", &service_link).is_ok() {
                    println!("  Masked {}", service);
                }
            }
        }
    }
    
    // Create minimal PAM configuration (CRITICAL for systemd-logind)
    println!("Creating PAM configuration for live CD...");
    let pam_d_dir = rootfs_dir.join("etc/pam.d");
    fs::create_dir_all(&pam_d_dir).ok();
    
    // system-auth: base authentication
    let system_auth_content = r#"#%PAM-1.0
auth     sufficient pam_permit.so
account  sufficient pam_permit.so
password sufficient pam_permit.so
session  sufficient pam_permit.so
"#;
    fs::write(pam_d_dir.join("system-auth"), system_auth_content).ok();
    
    // systemd-user: for systemd user sessions
    let systemd_user_content = r#"#%PAM-1.0
account  sufficient pam_permit.so
session  sufficient pam_permit.so
"#;
    fs::write(pam_d_dir.join("systemd-user"), systemd_user_content).ok();
    
    // other: fallback for services without specific config
    let other_content = r#"#%PAM-1.0
auth     sufficient pam_permit.so
account  sufficient pam_permit.so
password sufficient pam_permit.so
session  sufficient pam_permit.so
"#;
    fs::write(pam_d_dir.join("other"), other_content).ok();
    
    // login: for getty/login
    let login_content = r#"#%PAM-1.0
auth     sufficient pam_permit.so
account  sufficient pam_permit.so
password sufficient pam_permit.so
session  sufficient pam_permit.so
"#;
    fs::write(pam_d_dir.join("login"), login_content).ok();
    println!("Created PAM configuration files");
    
    // Create getty@.service if it doesn't exist (some systemd builds don't include it)
    let systemd_system_dir = rootfs_dir.join("usr/lib/systemd/system");
    let getty_service_path = systemd_system_dir.join("getty@.service");
    
    if !getty_service_path.exists() {
        println!("Creating getty@.service (not provided by systemd package)...");
        fs::create_dir_all(&systemd_system_dir).ok();
        
        let getty_service_content = r#"[Unit]
Description=Getty on %I
Documentation=man:agetty(8) man:systemd-getty-generator(8)
After=systemd-user-sessions.service plymouth-quit-wait.service
After=rc-local.service
Before=getty.target
IgnoreOnIsolate=yes
ConditionPathExists=/dev/tty0

[Service]
ExecStart=-/sbin/agetty -o '-p -- \\u' --noclear %I $TERM
Type=idle
Restart=always
RestartSec=0
UtmpIdentifier=%I
TTYPath=/dev/%I
TTYReset=yes
TTYVHangup=yes
TTYVTDisallocate=yes
KillMode=process
IgnoreSIGPIPE=no
SendSIGHUP=yes

[Install]
WantedBy=getty.target
"#;
        if let Err(e) = fs::write(&getty_service_path, getty_service_content) {
            println!("Warning: Failed to create getty@.service: {}", e);
        } else {
            println!("Created getty@.service");
        }
    }
    
    // Configure autologin for root on tty1 for live CD
    println!("Configuring autologin for live CD...");
    let getty_override_dir = rootfs_dir.join("etc/systemd/system/getty@tty1.service.d");
    fs::create_dir_all(&getty_override_dir).ok();
    
    let autologin_conf = r#"[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin root --noclear %I $TERM
"#;
    fs::write(getty_override_dir.join("autologin.conf"), autologin_conf).ok();
    
    // Enable getty@tty1.service by creating symlink
    let getty_target_dir = rootfs_dir.join("etc/systemd/system/getty.target.wants");
    if let Err(e) = fs::create_dir_all(&getty_target_dir) {
        println!("Warning: Failed to create getty.target.wants directory: {}", e);
    }
    
    let getty_service_link = getty_target_dir.join("getty@tty1.service");
    if !getty_service_link.exists() {
        // Use relative symlink
        if let Err(e) = std::os::unix::fs::symlink("../../usr/lib/systemd/system/getty@.service", &getty_service_link) {
            println!("Warning: Failed to create getty symlink: {}", e);
        } else {
            println!("Enabled getty@tty1.service");
        }
    }
    println!("Configured and enabled autologin for root on tty1");
    
    // Also create a default target override to ensure we actually try to start getty
    let default_target_link = rootfs_dir.join("etc/systemd/system/default.target");
    if !default_target_link.exists() {
        std::os::unix::fs::symlink("/usr/lib/systemd/system/multi-user.target", &default_target_link).ok();
        println!("Set default target to multi-user.target");
    }
    
    // Ensure bash is accessible at /bin/bash (where getty expects it)
    println!("Ensuring bash is accessible at /bin/bash...");
    let bin_bash = rootfs_dir.join("bin/bash");
    if !bin_bash.exists() {
        // Check if bash is in /usr/bin
        let usr_bin_bash = rootfs_dir.join("usr/bin/bash");
        if usr_bin_bash.exists() {
            println!("Found bash at /usr/bin/bash, creating symlink at /bin/bash");
            std::os::unix::fs::symlink("../usr/bin/bash", &bin_bash).ok();
        } else {
            println!("WARNING: bash not found in rootfs!");
        }
    } else {
        println!("bash exists at /bin/bash");
    }
    
    // Ensure sh is accessible at /bin/sh
    let bin_sh = rootfs_dir.join("bin/sh");
    if !bin_sh.exists() {
        if rootfs_dir.join("usr/bin/bash").exists() {
            std::os::unix::fs::symlink("../usr/bin/bash", &bin_sh).ok();
            println!("Created /bin/sh -> /usr/bin/bash symlink");
        } else if bin_bash.exists() {
            std::os::unix::fs::symlink("bash", &bin_sh).ok();
            println!("Created /bin/sh -> bash symlink");
        }
    }
    
    // Ensure minimal /etc/passwd and /etc/group exist (required for any auth/login)
    let passwd_path = rootfs_dir.join("etc/passwd");
    if !passwd_path.exists() {
        let passwd_content = "root:x:0:0:root:/root:/bin/bash\n";
        fs::write(&passwd_path, passwd_content).ok();
        println!("Created minimal /etc/passwd");
    }
    
    let group_path = rootfs_dir.join("etc/group");
    if !group_path.exists() {
        let group_content = "root:x:0:\n";
        fs::write(&group_path, group_content).ok();
        println!("Created minimal /etc/group");
    }
    
    // Create a basic bashrc for root with a visible prompt
    let root_dir = rootfs_dir.join("root");
    fs::create_dir_all(&root_dir).ok();
    let bashrc_path = root_dir.join(".bashrc");
    if !bashrc_path.exists() {
        let bashrc_content = r#"# Bash configuration for live CD
PS1='\[\033[01;32m\]\u@oreon-live\[\033[00m\]:\[\033[01;34m\]\w\[\033[00m\]\$ '
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
alias ls='ls --color=auto'
alias ll='ls -lah'
echo "Welcome to Oreon Live CD!"
echo "Type 'poweroff' to shutdown or 'reboot' to restart."
"#;
        fs::write(&bashrc_path, bashrc_content).ok();
        println!("Created /root/.bashrc with prompt configuration");
    }
    
    // Auto-fix missing library versions by creating compatibility symlinks
    println!("Checking for missing library dependencies...");
    match fix_library_dependencies(&rootfs_dir) {
        Ok(_) => {},
        Err(e) => println!("Warning: Library dependency check had issues: {}", e),
    }
    
    // Force create known compatibility symlinks for common library version mismatches
    println!("Creating critical library symlinks...");
    
    // Libraries that commonly need version compatibility symlinks: (base_name, older_version, newer_version)
    // Note: Only map truly compatible versions (e.g. libcrypt 1->2), not major version jumps
    let lib_mappings = [
        ("libcrypt.so", "1", "2"),
    ];
    
    for (lib_base, old_ver, new_ver) in &lib_mappings {
        // Find where the library actually exists
        let mut lib_found_in: Vec<&str> = Vec::new();
        for &lib_dir in &["usr/lib", "usr/lib64", "lib", "lib64"] {
            let lib_path = rootfs_dir.join(lib_dir);
            if lib_path.exists() {
                if let Ok(entries) = fs::read_dir(&lib_path) {
                    for entry in entries.flatten() {
                        let name_str = entry.file_name().to_string_lossy().to_string();
                        if name_str.starts_with(lib_base) && entry.path().is_file() {
                            lib_found_in.push(lib_dir);
                            break;
                        }
                    }
                }
            }
        }
        
        // Create symlinks in ALL lib directories
        for &lib_dir in &["usr/lib", "usr/lib64", "lib", "lib64"] {
            let lib_path = rootfs_dir.join(lib_dir);
            if lib_path.exists() {
                let old_lib = lib_path.join(format!("{}.{}", lib_base, old_ver));
                let new_lib = lib_path.join(format!("{}.{}", lib_base, new_ver));
                
                // If newer version doesn't exist, create it
                if !new_lib.exists() {
                    if old_lib.exists() {
                        // Link to the older version in the same directory
                        std::os::unix::fs::symlink(format!("{}.{}", lib_base, old_ver), &new_lib).ok();
                        println!("  Created: {} -> {}.{}", new_lib.display(), lib_base, old_ver);
                    } else if !lib_found_in.is_empty() {
                        // Link to absolute path where we found the library
                        let target = if lib_found_in.contains(&"usr/lib64") {
                            format!("/usr/lib64/{}.{}", lib_base, old_ver)
                        } else if lib_found_in.contains(&"lib64") {
                            format!("/lib64/{}.{}", lib_base, old_ver)
                        } else if lib_found_in.contains(&"usr/lib") {
                            format!("/usr/lib/{}.{}", lib_base, old_ver)
                        } else {
                            format!("/lib/{}.{}", lib_base, old_ver)
                        };
                        std::os::unix::fs::symlink(&target, &new_lib).ok();
                        println!("  Created: {} -> {}", new_lib.display(), target);
                    }
                }
            }
        }
    }
    
    // Extract kernel and initrd from rootfs to ISO boot directory
    println!("Setting up kernel and initrd...");
    setup_kernel_and_initrd(&rootfs_dir, &iso_root)?;
    
    // Create squashfs compressed rootfs
    println!("Creating squashfs rootfs...");
    let squashfs_path = iso_root.join("live").join("rootfs.squashfs");
    fs::create_dir_all(squashfs_path.parent().unwrap())
        .map_err(|e| format!("Failed to create live directory: {}", e))?;
    
    create_squashfs(&rootfs_dir, &squashfs_path)?;
    
    // Set up bootloader (GRUB) - update to load from squashfs
    println!("Setting up bootloader...");
    setup_grub(&iso_root)?;
    
    // Create initrd/init script for live environment
    setup_live_init(&iso_root, template)?;
    
    // Apply out-of-box configuration to rootfs before squashing (if needed in initrd)
    if let Some(config) = template.and_then(|t| t.config.as_ref()) {
        apply_template_config(&rootfs_dir, config)?;
    }
    
    // Create ISO
    println!("Creating ISO image...");
    create_iso_image(&iso_root, output_path)?;
    
    Ok(())
}

/// Automatically fix missing library dependencies by creating compatibility symlinks
fn fix_library_dependencies(rootfs: &Path) -> Result<(), String> {
    use std::collections::{HashMap, HashSet};
    
    println!("Scanning for executables and checking library dependencies...");
    
    // Find all executables in critical paths
    let critical_paths = vec![
        "sbin", "usr/sbin", "bin", "usr/bin",
        "usr/lib/systemd", "lib/systemd"
    ];
    
    let mut missing_libs: HashMap<String, HashSet<String>> = HashMap::new();
    
    for path in &critical_paths {
        let dir = rootfs.join(path);
        if !dir.exists() {
            continue;
        }
        
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let file_path = entry.path();
                if !file_path.is_file() {
                    continue;
                }
                
                // Check dependencies with ldd
                let output = RunCommand::new("ldd")
                    .arg(&file_path)
                    .output();
                
                if let Ok(output) = output {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    for line in stdout.lines() {
                        // Look for "=> not found" indicating missing library
                        if line.contains("=> not found") {
                            if let Some(lib_name) = line.split_whitespace().next() {
                                missing_libs.entry(lib_name.to_string())
                                    .or_insert_with(HashSet::new)
                                    .insert(file_path.display().to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    
    if missing_libs.is_empty() {
        println!("No missing library dependencies found");
        return Ok(());
    }
    
    println!("Found {} missing libraries, attempting to create compatibility symlinks...", missing_libs.len());
    
    // Try to fix each missing library
    let lib_dirs = vec!["usr/lib", "usr/lib64", "lib", "lib64"];
    
    for (missing_lib, binaries) in &missing_libs {
        println!("Missing: {} (needed by {} binaries)", missing_lib, binaries.len());
        
        // Parse library name and version (e.g., libfoo.so.2)
        if let Some(base_name) = missing_lib.strip_suffix(".so").or_else(|| {
            // Handle versioned libs like libfoo.so.2
            let parts: Vec<&str> = missing_lib.split(".so.").collect();
            if parts.len() == 2 {
                Some(parts[0])
            } else {
                None
            }
        }) {
            // Search for alternative versions in lib directories
            for lib_dir in &lib_dirs {
                let dir_path = rootfs.join(lib_dir);
                if !dir_path.exists() {
                    continue;
                }
                
                if let Ok(entries) = fs::read_dir(&dir_path) {
                    for entry in entries.flatten() {
                        let file_name = entry.file_name();
                        let name_str = file_name.to_string_lossy();
                        
                        // Check if this is a compatible version (same base name)
                        if name_str.starts_with(base_name) && name_str.contains(".so") && name_str.as_ref() != missing_lib {
                            // Found a candidate - create symlink
                            let target = entry.path();
                            let link = dir_path.join(missing_lib);
                            
                            if !link.exists() {
                                match std::os::unix::fs::symlink(file_name.as_os_str(), &link) {
                                    Ok(_) => {
                                        println!("  Created: {} -> {}", link.display(), name_str);
                                        break; // Found a solution for this lib
                                    }
                                    Err(e) => {
                                        println!("  Warning: Failed to create symlink: {}", e);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    
    Ok(())
}

fn install_package_to_root(
    runtime: &Runtime,
    package: &metadata::InstallPackage,
    root: &Path,
) -> Result<(), String> {
    // Use pax's actual install system with a custom root via environment variable
    // This ensures scriptlets and all installation logic runs properly
    // Similar to how livemedia-creator uses dnf --installroot
    
    // Install dependencies first
    for dep in &package.run_deps {
        runtime.block_on(install_single_package_to_root(dep.clone(), root))
            .map_err(|e| format!("Failed to install dependency {}: {}", dep.name, e))?;
    }
    
    // Install the main package
    runtime.block_on(install_single_package_to_root(package.metadata.clone(), root))
        .map_err(|e| format!("Failed to install {}: {}", package.metadata.name, e))?;
    
    Ok(())
}

async fn install_single_package_to_root(
    metadata: metadata::ProcessedMetaData,
    root: &Path,
) -> Result<(), String> {
    use std::env;
    use std::io::Write;
    
    println!("[INSTALL] Installing package {} {} to {}", 
        metadata.name, metadata.version, root.display());
    std::io::stdout().flush().unwrap();
    
    // Save original PAX_ROOT if set
    let original_root = env::var("PAX_ROOT").ok();
    
    // Set PAX_ROOT to point to our ISO rootfs
    unsafe {
        env::set_var("PAX_ROOT", root.to_string_lossy().to_string());
    }
    
    println!("[INSTALL] PAX_ROOT set to: {}", root.display());
    std::io::stdout().flush().unwrap();
    
    // Install using pax's install system - it will now use PAX_ROOT
    println!("[INSTALL] Calling metadata.install_package()...");
    std::io::stdout().flush().unwrap();
    let result = metadata.install_package().await;
    
    if result.is_err() {
        println!("[INSTALL] install_package() failed: {:?}", result);
    } else {
        println!("[INSTALL] install_package() succeeded");
    }
    std::io::stdout().flush().unwrap();
    
    // Restore original PAX_ROOT
    unsafe {
        match &original_root {
            Some(r) => env::set_var("PAX_ROOT", r),
            None => env::remove_var("PAX_ROOT"),
        }
    }
    
    result
}

async fn download_package_file(metadata: &metadata::ProcessedMetaData) -> Result<PathBuf, String> {
    use settings::OriginKind;
    
    let tmpfile = utils::tmpfile().ok_or("Failed to reserve temporary file")?;
    
    match &metadata.origin {
        OriginKind::LocalDir(dir_path) => {
            let dir = std::path::Path::new(dir_path);
            let possible_files = vec![
                dir.join(format!("{}-{}.pax", metadata.name, metadata.version)),
                dir.join(format!("{}-{}.deb", metadata.name, metadata.version)),
                dir.join(format!("{}-{}.rpm", metadata.name, metadata.version)),
            ];
            
            for package_path in possible_files {
                if package_path.exists() {
                    std::fs::copy(&package_path, &tmpfile)
                        .map_err(|e| format!("Failed to copy package: {}", e))?;
                    return Ok(tmpfile);
                }
            }
            Err(format!("Package {}-{} not found", metadata.name, metadata.version))
        }
        OriginKind::Pax(url) => {
            if url.starts_with("http://") || url.starts_with("https://") {
                let response = reqwest::get(url).await
                    .map_err(|e| format!("Failed to download: {}", e))?;
                let bytes = response.bytes().await
                    .map_err(|e| format!("Failed to read data: {}", e))?;
                std::fs::write(&tmpfile, bytes)
                    .map_err(|e| format!("Failed to write file: {}", e))?;
                Ok(tmpfile)
            } else {
                std::fs::copy(url, &tmpfile)
                    .map_err(|e| format!("Failed to copy file: {}", e))?;
                Ok(tmpfile)
            }
        }
        _ => Err("Unsupported package source for ISO creation".to_string()),
    }
}

async fn extract_package_to_dir(
    metadata: &metadata::ProcessedMetaData,
    package_file: &Path,
    extract_dir: &Path,
) -> Result<(), String> {
    use settings::OriginKind;
    
    match &metadata.origin {
        OriginKind::Pax(_) | OriginKind::LocalDir(_) => {
            // Extract using tar - by default tar strips leading / from absolute paths
            // Use -P to preserve absolute paths, then we handle them in copy_extracted_to_root
            // Or use --no-same-owner and --no-same-permissions for safer extraction
            let output = RunCommand::new("tar")
                .arg("-xzf")
                .arg(package_file)
                .arg("-C")
                .arg(extract_dir)
                .arg("--no-same-owner")
                .arg("--no-same-permissions")
                .output()
                .map_err(|_| "Failed to execute tar")?;
            
            if !output.status.success() {
                return Err(format!("tar extraction failed: {}", 
                    String::from_utf8_lossy(&output.stderr)));
            }
            Ok(())
        }
        OriginKind::Apt(_) | OriginKind::Deb(_) => {
            RunCommand::new("dpkg-deb")
                .arg("-x")
                .arg(package_file)
                .arg(extract_dir)
                .status()
                .map_err(|_| "Failed to extract DEB")?
                .success()
                .then_some(())
                .ok_or_else(|| "dpkg-deb extraction failed".to_string())
        }
        OriginKind::Rpm(_) | OriginKind::Yum(_) => {
            // rpm2cpio extracts with absolute paths, use --no-absolute-filenames if available
            let cmd = format!("rpm2cpio '{}' | (cd '{}' && cpio -idmv --no-absolute-filenames 2>/dev/null || cpio -idmv)", 
                package_file.display(), extract_dir.display());
            RunCommand::new("bash")
                .arg("-c")
                .arg(cmd)
                .status()
                .map_err(|_| "Failed to extract RPM")?
                .success()
                .then_some(())
                .ok_or_else(|| "rpm extraction failed".to_string())
        }
        _ => Err("Unsupported package type".to_string()),
    }
}

fn copy_extracted_to_root(extract_dir: &Path, root: &Path) -> Result<(), String> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    
    // Walk through extracted directory and copy files to root
    let entries = collect_package_entries(extract_dir)?;
    
    for (src_path, relative) in entries {
        let metadata = fs::symlink_metadata(&src_path)
            .map_err(|e| format!("Failed to inspect {}: {}", src_path.display(), e))?;
        
        // Handle absolute paths in extracted packages
        // Packages may extract with absolute paths like /boot/vmlinuz or /usr/bin/bash
        // We need to strip the leading / and join to root
        let relative_clean = if let Ok(stripped) = relative.strip_prefix("/") {
            stripped.to_path_buf()
        } else {
            relative
        };
        
        let dest_path = root.join(&relative_clean);
        
        if metadata.is_dir() {
            fs::create_dir_all(&dest_path)
                .map_err(|e| format!("Failed to create directory {}: {}", dest_path.display(), e))?;
            
            let mode = metadata.permissions().mode();
            fs::set_permissions(&dest_path, fs::Permissions::from_mode(mode))
                .map_err(|e| format!("Failed to set permissions on {}: {}", dest_path.display(), e))?;
        } else if metadata.file_type().is_symlink() {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent dir: {}", e))?;
            }
            
            let target = fs::read_link(&src_path)
                .map_err(|e| format!("Failed to read symlink: {}", e))?;
            
            // Remove existing symlink if it exists
            let _ = fs::remove_file(&dest_path);
            std::os::unix::fs::symlink(&target, &dest_path)
                .map_err(|e| format!("Failed to create symlink: {}", e))?;
        } else if metadata.is_file() {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create parent dir: {}", e))?;
            }
            
            fs::copy(&src_path, &dest_path)
                .map_err(|e| format!("Failed to copy file: {}", e))?;
            
            let mode = metadata.permissions().mode();
            fs::set_permissions(&dest_path, fs::Permissions::from_mode(mode))
                .map_err(|e| format!("Failed to set permissions: {}", e))?;
        }
    }
    
    Ok(())
}

fn collect_package_entries(dir: &Path) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    use std::fs;
    
    let mut entries = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    
    while let Some(current) = stack.pop() {
        for entry in fs::read_dir(&current)
            .map_err(|e| format!("Failed to read dir {}: {}", current.display(), e))? {
            let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
            let path = entry.path();
            
            let relative = path.strip_prefix(dir)
                .map_err(|e| format!("Failed to strip prefix: {}", e))?
                .to_path_buf();
            
            entries.push((path.clone(), relative.clone()));
            
            if path.is_dir() {
                stack.push(path);
            }
        }
    }
    
    Ok(entries)
}

fn find_file_recursive(root: &Path, filename: &str) -> Result<Option<PathBuf>, String> {
    use std::fs;
    
    if !root.exists() {
        return Ok(None);
    }
    
    let mut stack = vec![root.to_path_buf()];
    let mut visited = std::collections::HashSet::new();
    
    while let Some(current) = stack.pop() {
        if visited.contains(&current) {
            continue;
        }
        visited.insert(current.clone());
        
        let entries = match fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            
            if path.is_dir() {
                let path_str = path.to_string_lossy();
                if !path_str.contains("/proc") && !path_str.contains("/sys") && 
                   !path_str.contains("/dev") && !path_str.contains("/tmp") &&
                   !path_str.contains("/run") {
                    stack.push(path);
                }
            } else if path.file_name().and_then(|n| n.to_str()) == Some(filename) {
                return Ok(Some(path));
            }
        }
    }
    
    Ok(None)
}

fn find_file_starting_with(root: &Path, prefix: &str) -> Result<Option<PathBuf>, String> {
    use std::fs;
    
    if !root.exists() {
        return Ok(None);
    }
    
    let mut stack = vec![root.to_path_buf()];
    let mut visited = std::collections::HashSet::new();
    
    while let Some(current) = stack.pop() {
        if visited.contains(&current) {
            continue;
        }
        visited.insert(current.clone());
        
        let entries = match fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        
        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            
            if path.is_dir() {
                let path_str = path.to_string_lossy();
                if !path_str.contains("/proc") && !path_str.contains("/sys") && 
                   !path_str.contains("/dev") && !path_str.contains("/tmp") &&
                   !path_str.contains("/run") {
                    stack.push(path);
                }
            } else if path.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(prefix))
                .unwrap_or(false) {
                return Ok(Some(path));
            }
        }
    }
    
    Ok(None)
}

fn setup_grub(iso_root: &Path) -> Result<(), String> {
    fs::create_dir_all(iso_root.join("boot/grub"))
        .map_err(|e| format!("Failed to create grub directory: {}", e))?;
    
    let grub_cfg = iso_root.join("boot/grub/grub.cfg");
    let grub_content = r#"set timeout=5
set default=0

insmod all_video
insmod iso9660
insmod linux

terminal_output console

menuentry "Oreon Live" {
    echo "Loading kernel..."
    linux /boot/vmlinuz console=tty1 consoleblank=0 vga=normal torture.disable_onoff_at_boot=1 rcutorture.onoff_interval=0
    echo "Loading initrd..."
    initrd /boot/initrd.img
    echo "Booting..."
}
"#;
    
    fs::write(&grub_cfg, grub_content)
        .map_err(|e| format!("Failed to write grub.cfg: {}", e))?;
    
    Ok(())
}

fn setup_kernel_and_initrd(rootfs: &Path, iso_root: &Path) -> Result<(), String> {
    fs::create_dir_all(iso_root.join("boot"))
        .map_err(|e| format!("Failed to create boot directory: {}", e))?;
    
    // Check for kernel in common locations
    let boot_dir = rootfs.join("boot");
    
    // First, check what's actually in the rootfs
    let mut rootfs_contents = Vec::new();
    if let Ok(entries) = rootfs.read_dir() {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            rootfs_contents.push((name, path.is_dir()));
        }
    }
    
    let mut kernel_path: Option<PathBuf> = None;
    let mut boot_files = Vec::new();
    
    // Search function to find kernel files recursively
    let mut search_kernel = |dir: &Path| -> Result<Option<PathBuf>, String> {
        let mut found = None;
        let mut search_stack = vec![dir.to_path_buf()];
        let mut visited = std::collections::HashSet::new();
        
        while let Some(current_dir) = search_stack.pop() {
            if visited.contains(&current_dir) {
                continue;
            }
            visited.insert(current_dir.clone());
            
            let entries = match std::fs::read_dir(&current_dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            
        for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                
                let path = entry.path();
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy().to_string();
                
                if path.is_dir() {
                    // Continue searching in subdirectories
                    let path_str = path.to_string_lossy();
                    if !path_str.contains("/proc") && !path_str.contains("/sys") &&
                       !path_str.contains("/dev") && !path_str.contains("/tmp") &&
                       !path_str.contains("/run") && !path_str.contains("/var/cache") {
                        search_stack.push(path);
                    }
                } else if path.is_file() {
                    // Check if it's a kernel file - check multiple patterns
                    let lower_name = name.to_lowercase();
                    if lower_name.starts_with("vmlinuz") || lower_name.starts_with("vmlinux") || 
                       lower_name == "bzimage" || lower_name == "kernel" || 
                       lower_name == "image" || lower_name.ends_with("-kernel") ||
                       (lower_name.contains("kernel") && !lower_name.contains("modules")) {
                        // Check if it's actually an executable/ELF file (kernels are typically ELF)
                        // Skip symlinks for now and use the actual file
                        if let Ok(metadata) = std::fs::metadata(&path) {
                            // Check if it's large enough to be a kernel (at least 1MB)
                            if metadata.len() > 1_000_000 {
                                found = Some(path);
                                break;
                            }
                        }
                    }
                }
            }
            if found.is_some() {
                break;
            }
        }
        Ok(found)
    };
    
    // First, search in /boot and its subdirectories
    if boot_dir.exists() {
        println!("Searching for kernel in boot directory: {}", boot_dir.display());
        if let Ok(entries) = std::fs::read_dir(&boot_dir) {
            for entry in entries {
                if let Ok(entry) = entry {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();
                boot_files.push(name.clone());
                }
            }
        }
        
        if let Ok(found) = search_kernel(&boot_dir) {
            kernel_path = found;
        }
    }
    
    // Also check /lib/modules - if there are kernel modules, there should be a kernel
    let lib_modules = rootfs.join("lib/modules");
    if lib_modules.exists() {
        println!("Found /lib/modules - checking for kernel version...");
        if let Ok(entries) = std::fs::read_dir(&lib_modules) {
            for entry in entries.flatten() {
                let kernel_version = entry.file_name().to_string_lossy().to_string();
                println!("Found kernel module directory: {}", kernel_version);
                
                // Try common kernel locations with this version
                let possible_kernels = vec![
                    rootfs.join("boot").join(format!("vmlinuz-{}", kernel_version)),
                    rootfs.join("boot").join(format!("vmlinux-{}", kernel_version)),
                    rootfs.join("boot").join(format!("bzImage-{}", kernel_version)),
                    rootfs.join(format!("boot/vmlinuz-{}", kernel_version)),
                ];
                
                for kernel_loc in possible_kernels {
                    if kernel_loc.exists() && kernel_loc.is_file() {
                        println!("Found kernel at: {}", kernel_loc.display());
                        kernel_path = Some(kernel_loc);
                        break;
                    }
                }
                
                if kernel_path.is_some() {
                    break;
                }
            }
        }
    }
    
    // If still not found, search entire rootfs (excluding special dirs)
    if kernel_path.is_none() {
        println!("Kernel not found in /boot, searching entire rootfs...");
        let skip_dirs = ["/proc", "/sys", "/dev", "/tmp", "/run", "/var/cache", "/boot/grub"];
        let mut search_dirs = vec![rootfs.to_path_buf()];
        let mut visited = std::collections::HashSet::new();
        
        while let Some(dir) = search_dirs.pop() {
            if visited.contains(&dir) {
                continue;
            }
            visited.insert(dir.clone());
            
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            
                for entry in entries.flatten() {
                    let path = entry.path();
                        let path_str = path.to_string_lossy();
                
                // Skip special directories
                if skip_dirs.iter().any(|skip| path_str.contains(skip)) {
                    continue;
                }
                
                if path.is_dir() {
                            search_dirs.push(path);
                    } else if path.is_file() {
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
                    if (name.starts_with("vmlinuz") || name.starts_with("vmlinux") || 
                        name == "bzimage" || name.contains("kernel")) &&
                        !name.contains("modules") && !name.contains(".ko") {
                        // Check file size - kernels are typically large
                        if let Ok(metadata) = std::fs::metadata(&path) {
                            if metadata.len() > 1_000_000 {
                                println!("Found potential kernel at: {}", path.display());
                            kernel_path = Some(path);
                            break;
                            }
                        }
                    }
                }
            }
            if kernel_path.is_some() {
                break;
            }
        }
    }
    
    let kernel_path = kernel_path.ok_or_else(|| {
        // Provide helpful error message
        let mut error_msg = format!(
            "No kernel file found anywhere in rootfs.\n\n\
            Boot directory contents: {:?}\n\
            Rootfs top-level: {:?}\n\n",
            boot_files, rootfs_contents
        );
        
        // Check if linux-kernel package might be missing
        error_msg.push_str("Possible issues:\n");
        error_msg.push_str("1. The 'linux-kernel' package may not be installed or found\n");
        error_msg.push_str("2. The kernel package may have installed to an unexpected location\n");
        error_msg.push_str("3. The kernel package may be missing from your repositories\n\n");
        error_msg.push_str("Please ensure 'linux-kernel' (or similar) is in your package list.\n");
        
        error_msg
    })?;
    
    println!("Found kernel at: {}", kernel_path.display());
    let dest = iso_root.join("boot/vmlinuz");
    fs::copy(&kernel_path, &dest)
        .map_err(|e| format!("Failed to copy kernel from {} to {}: {}", 
            kernel_path.display(), dest.display(), e))?;
    println!("Copied kernel to ISO boot directory");
    
    // Modern kernels include initramfs at /usr/lib/modules/$kver/initramfs.img
    // Check this location first, then fall back to /boot
    let mut initrd_found = false;
    let mut initrd_search_paths = Vec::new();
    
    // Extract kernel version from filename
    let kernel_version = if let Some(kernel_file_name) = kernel_path.file_name().and_then(|n| n.to_str()) {
        kernel_file_name.strip_prefix("vmlinuz-")
            .or_else(|| kernel_file_name.strip_prefix("vmlinux-"))
            .or_else(|| kernel_file_name.strip_prefix("bzImage-"))
            .map(|s| s.to_string())
    } else {
        None
    };
    
    // Modern location (preferred): /usr/lib/modules/$kver/initramfs.img
    if let Some(ref kver) = kernel_version {
        initrd_search_paths.push(rootfs.join("usr/lib/modules").join(kver).join("initramfs.img"));
        initrd_search_paths.push(rootfs.join("lib/modules").join(kver).join("initramfs.img"));
    }
    
    // Traditional /boot locations
    initrd_search_paths.extend(vec![
        rootfs.join("boot/initrd.img"),
        rootfs.join("boot/initramfs.img"),
        rootfs.join("boot/initrd"),
        rootfs.join("boot/initrd.gz"),
    ]);
    
    // Version-specific /boot locations
    if let Some(ref kver) = kernel_version {
        initrd_search_paths.extend(vec![
            rootfs.join("boot").join(format!("initrd.img-{}", kver)),
            rootfs.join("boot").join(format!("initramfs.img-{}", kver)),
            rootfs.join("boot").join(format!("initrd-{}", kver)),
            rootfs.join("boot").join(format!("initramfs-{}", kver)),
        ]);
    }
    
    for initrd_path in &initrd_search_paths {
        if initrd_path.exists() && initrd_path.is_file() {
            let dest = iso_root.join("boot/initrd.img");
            fs::copy(initrd_path, &dest)
                .map_err(|e| format!("Failed to copy initrd: {}", e))?;
            println!("Found and copied initrd: {}", initrd_path.display());
            initrd_found = true;
            break;
        }
    }
    
    if !initrd_found {
        eprintln!("Warning: No initrd found in rootfs");
        
        // Try to generate initrd using tools in the rootfs
        println!("Attempting to generate initrd using rootfs tools...");
        if let Err(e) = generate_initrd_with_chroot(rootfs, &iso_root.join("boot/initrd.img")) {
            eprintln!("Failed to generate initrd with chroot: {}", e);
            eprintln!("Falling back to downloading pre-built initramfs...");
            
            if let Err(e2) = download_alpine_initramfs(&iso_root.join("boot/initrd.img")) {
                return Err(format!(
                    "Failed to create initrd:\n  - Chroot generation: {}\n  - Download fallback: {}",
                    e, e2
                ));
            }
        }
    }
    
    Ok(())
}

/// Helper to copy a directory recursively
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst)
        .map_err(|e| format!("Failed to create directory {}: {}", dst.display(), e))?;
    
    let entries = fs::read_dir(src)
        .map_err(|e| format!("Failed to read directory {}: {}", src.display(), e))?;
    
    for entry in entries.flatten() {
        let src_path = entry.path();
        let file_name = entry.file_name();
        let dst_path = dst.join(&file_name);
        
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path).ok();
        }
    }
    
    Ok(())
}

/// Helper to copy shared libraries that a binary depends on
fn copy_shared_libs(binary: &Path, dest_root: &Path, rootfs: &Path) -> Result<(), String> {
    use std::process::Command as RunCommand;
    
    // Use ldd to find shared library dependencies
    let output = RunCommand::new("ldd")
        .arg(binary)
        .output();
    
    if let Ok(output) = output {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            // Parse lines like: "libc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x00007f...)"
            if let Some(arrow_pos) = line.find("=>") {
                let after_arrow = &line[arrow_pos + 2..].trim();
                if let Some(space_pos) = after_arrow.find(' ') {
                    let lib_path = after_arrow[..space_pos].trim();
                    if !lib_path.is_empty() && lib_path.starts_with('/') {
                        // Try to find this lib in the rootfs
                        let lib_in_rootfs = rootfs.join(lib_path.trim_start_matches('/'));
                        if lib_in_rootfs.exists() {
                            let dest_lib = dest_root.join(lib_path.trim_start_matches('/'));
                            if let Some(parent) = dest_lib.parent() {
                                fs::create_dir_all(parent).ok();
                            }
                            fs::copy(&lib_in_rootfs, &dest_lib).ok();
                        }
                    }
                }
            }
            // Also handle direct paths like: "/lib64/ld-linux-x86-64.so.2 (0x00007f...)"
            else if line.trim().starts_with('/') {
                if let Some(space_pos) = line.find(' ') {
                    let lib_path = line[..space_pos].trim();
                    let lib_in_rootfs = rootfs.join(lib_path.trim_start_matches('/'));
                    if lib_in_rootfs.exists() {
                        let dest_lib = dest_root.join(lib_path.trim_start_matches('/'));
                        if let Some(parent) = dest_lib.parent() {
                            fs::create_dir_all(parent).ok();
                        }
                        fs::copy(&lib_in_rootfs, &dest_lib).ok();
                    }
                }
            }
        }
    }
    
    Ok(())
}

/// Create a self-contained initrd from the rootfs (busybox, kmod, kernel modules)
fn generate_initrd_with_chroot(rootfs: &Path, output: &Path) -> Result<(), String> {
    use std::process::Command as RunCommand;
    use std::os::unix::fs::PermissionsExt;
    
    println!("Creating self-contained initramfs from rootfs...");
    
    // Find kernel version
    let lib_modules = rootfs.join("lib/modules");
    let kernel_version = if lib_modules.exists() {
        if let Ok(entries) = fs::read_dir(&lib_modules) {
            entries.flatten()
                .find(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().to_string())
        } else {
            None
        }
    } else {
        None
    };
    
    let kver = kernel_version.ok_or("No kernel version found in /lib/modules")?;
    
    // Create temp directory for initrd
    let temp_dir = tempfile::tempdir()
        .map_err(|e| format!("Failed to create temp dir: {}", e))?;
    let init_dir = temp_dir.path();
    
    // Create directory structure
    for dir in &["bin", "sbin", "dev", "proc", "sys", "mnt", "newroot", "lib", "lib64", "usr/bin", "usr/sbin", "usr/lib"] {
        fs::create_dir_all(init_dir.join(dir))
            .map_err(|e| format!("Failed to create {}: {}", dir, e))?;
    }
    
    // Copy busybox from rootfs
    let busybox_paths = vec![
        rootfs.join("usr/bin/busybox"),
        rootfs.join("bin/busybox"),
        rootfs.join("usr/sbin/busybox"),
    ];
    
    let busybox_src = busybox_paths.iter()
        .find(|p| p.exists())
        .ok_or("busybox not found in rootfs")?;
    
    let busybox_dest = init_dir.join("bin/busybox");
    fs::copy(busybox_src, &busybox_dest)
        .map_err(|e| format!("Failed to copy busybox: {}", e))?;
    
    // Make busybox executable
    let mut perms = fs::metadata(&busybox_dest)
        .map_err(|e| format!("Failed to get busybox metadata: {}", e))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&busybox_dest, perms)
        .map_err(|e| format!("Failed to set busybox permissions: {}", e))?;
    
    // Copy essential kernel modules
    let modules_src = rootfs.join(format!("lib/modules/{}", kver));
    let modules_dest = init_dir.join(format!("lib/modules/{}", kver));
    
    if modules_src.exists() {
        // Copy the entire kernel modules directory
        copy_dir_recursive(&modules_src, &modules_dest)?;
    }
    
    // Copy modprobe from kmod package
    let modprobe_paths = vec![
        rootfs.join("usr/bin/modprobe"),
        rootfs.join("sbin/modprobe"),
        rootfs.join("usr/sbin/modprobe"),
    ];
    
    if let Some(modprobe_src) = modprobe_paths.iter().find(|p| p.exists()) {
        let modprobe_dest = init_dir.join("sbin/modprobe");
        fs::copy(modprobe_src, &modprobe_dest).ok();
        if let Ok(meta) = fs::metadata(&modprobe_dest) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            fs::set_permissions(&modprobe_dest, perms).ok();
        }
        
        // Copy modprobe's shared libraries
        copy_shared_libs(modprobe_src, init_dir, rootfs)?;
    }
    
    // Copy mount/umount/losetup from util-linux
    for binary in &["mount", "umount", "switch_root", "losetup"] {
        let search_paths = vec![
            rootfs.join(format!("usr/bin/{}", binary)),
            rootfs.join(format!("bin/{}", binary)),
            rootfs.join(format!("sbin/{}", binary)),
            rootfs.join(format!("usr/sbin/{}", binary)),
        ];
        
        if let Some(src) = search_paths.iter().find(|p| p.exists()) {
            let dest = init_dir.join(format!("bin/{}", binary));
            fs::copy(src, &dest).ok();
            if let Ok(meta) = fs::metadata(&dest) {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                fs::set_permissions(&dest, perms).ok();
            }
            copy_shared_libs(src, init_dir, rootfs)?;
        }
    }
    
    // Copy glibc and other essential libraries
    for lib_pattern in &["libc.so*", "libm.so*", "libdl.so*", "libpthread.so*", "libresolv.so*", "ld-linux*.so*", "ld-*.so*"] {
        for lib_dir in &["lib", "lib64", "usr/lib", "usr/lib64"] {
            let lib_path = rootfs.join(lib_dir);
            if lib_path.exists() {
                if let Ok(entries) = fs::read_dir(&lib_path) {
                    for entry in entries.flatten() {
                        let fname = entry.file_name();
                        let fname_str = fname.to_string_lossy();
                        let pattern = lib_pattern.trim_end_matches('*');
                        if fname_str.starts_with(pattern) {
                            let dest_dir = init_dir.join(lib_dir);
                            fs::create_dir_all(&dest_dir).ok();
                            let dest = dest_dir.join(&fname);
                            fs::copy(entry.path(), &dest).ok();
                        }
                    }
                }
            }
        }
    }
    
    // CRITICAL FIX: Ensure ld-linux is in /lib64 for busybox
    // Some binaries are compiled with hardcoded /lib64/ld-linux path
    println!("Ensuring dynamic linker is accessible in /lib64...");
    let lib64_dir = init_dir.join("lib64");
    fs::create_dir_all(&lib64_dir).ok();
    
    // Find ld-linux in rootfs and copy to /lib64 in initrd
    for src_lib_dir in &["usr/lib", "lib", "lib64", "usr/lib64"] {
        let ld_src = rootfs.join(src_lib_dir).join("ld-linux-x86-64.so.2");
        if ld_src.exists() {
            let ld_dest = lib64_dir.join("ld-linux-x86-64.so.2");
            if !ld_dest.exists() {
                if fs::copy(&ld_src, &ld_dest).is_ok() {
                    println!("  Copied ld-linux-x86-64.so.2 to /lib64/");
                }
            }
            break;
        }
    }
    
    // Also ensure critical libraries are in /lib64
    for lib_name in &["libc.so.6", "libm.so.6", "libresolv.so.2"] {
        for src_lib_dir in &["usr/lib", "lib", "lib64", "usr/lib64"] {
            let lib_src = rootfs.join(src_lib_dir).join(lib_name);
            if lib_src.exists() {
                let lib_dest = lib64_dir.join(lib_name);
                if !lib_dest.exists() {
                    fs::copy(&lib_src, &lib_dest).ok();
                }
                break;
            }
        }
    }
    
    // Create init script
    let init_script = format!(r#"#!/bin/busybox sh
# Self-contained initramfs for Oreon Live CD

echo "=== Oreon initramfs starting ==="

# Install busybox applets
/bin/busybox --install -s /bin || echo "WARNING: busybox install failed"

# Mount essential filesystems
echo "Mounting proc, sys, dev..."
mount -t proc proc /proc || echo "WARNING: proc mount failed"
mount -t sysfs sysfs /sys || echo "WARNING: sys mount failed"
mount -t devtmpfs dev /dev || echo "WARNING: dev mount failed"

echo "Loading kernel modules..."
/sbin/modprobe loop 2>/dev/null || true
/sbin/modprobe squashfs 2>/dev/null || true
/sbin/modprobe isofs 2>/dev/null || true
/sbin/modprobe overlay 2>/dev/null || true
/sbin/modprobe sr_mod 2>/dev/null || true
/sbin/modprobe cdrom 2>/dev/null || true
/sbin/modprobe ata_piix 2>/dev/null || true
/sbin/modprobe ata_generic 2>/dev/null || true

echo "Creating loop devices..."
for i in 0 1 2 3 4 5 6 7; do
    mknod /dev/loop$i b 7 $i 2>/dev/null || true
done

echo "Waiting for CD-ROM..."
sleep 2

# Find and mount the ISO
echo "Mounting CD-ROM..."
MOUNTED=0
for dev in /dev/sr0 /dev/sr1 /dev/cdrom /dev/scd0 /dev/hdc; do
    if [ -b "$dev" ]; then
        if mount -t iso9660 -o ro "$dev" /mnt 2>&1; then
            echo "Mounted CD from $dev"
            MOUNTED=1
            break
        fi
    fi
done

if [ "$MOUNTED" = "0" ]; then
    echo "ERROR: Could not find or mount CD-ROM!"
    echo "Available devices:"
    ls -l /dev/
    echo "Dropping to emergency shell..."
    setsid sh -c 'exec sh </dev/tty1 >/dev/tty1 2>&1'
fi

# Mount squashfs
if [ -f /mnt/live/rootfs.squashfs ]; then
    echo "Found squashfs at /mnt/live/rootfs.squashfs"
    echo "Mounting root filesystem..."
    if mount -t squashfs -o ro /mnt/live/rootfs.squashfs /newroot 2>&1; then
        # Setup overlay for writable root (squashfs is read-only)
        echo "Setting up overlay filesystem..."
        mkdir -p /overlay/upper /overlay/work /overlay/merged
        
        if mount -t overlay overlay -o lowerdir=/newroot,upperdir=/overlay/upper,workdir=/overlay/work /overlay/merged 2>&1; then
            # Prepare new root directories
            mkdir -p /overlay/merged/proc /overlay/merged/sys /overlay/merged/dev /overlay/merged/dev/pts
            mkdir -p /overlay/merged/run /overlay/merged/tmp /overlay/merged/var/log
            
            # Mount filesystems in new root
            mount -t proc proc /overlay/merged/proc
            mount -t sysfs sysfs /overlay/merged/sys
            mount -t devtmpfs devtmpfs /overlay/merged/dev
            mkdir -p /overlay/merged/dev/pts 2>/dev/null || true
            mount -t devpts devpts /overlay/merged/dev/pts -o mode=0620,gid=5 2>/dev/null || true
            mount -t tmpfs tmpfs -o mode=0755 /overlay/merged/run
            mount -t tmpfs tmpfs /overlay/merged/tmp
            
            # Ensure critical device nodes exist
            test -c /overlay/merged/dev/null || mknod -m 666 /overlay/merged/dev/null c 1 3
            test -c /overlay/merged/dev/zero || mknod -m 666 /overlay/merged/dev/zero c 1 5
            test -c /overlay/merged/dev/console || mknod -m 600 /overlay/merged/dev/console c 5 1
            test -c /overlay/merged/dev/tty || mknod -m 666 /overlay/merged/dev/tty c 5 0
            test -c /overlay/merged/dev/tty0 || mknod -m 666 /overlay/merged/dev/tty0 c 4 0
            test -c /overlay/merged/dev/tty1 || mknod -m 666 /overlay/merged/dev/tty1 c 4 1
            test -c /overlay/merged/dev/tty2 || mknod -m 666 /overlay/merged/dev/tty2 c 4 2
            
            # Switch to new root
            if [ -x /overlay/merged/sbin/init ]; then
                echo "Switching to real root..."
                exec switch_root /overlay/merged /sbin/init
            elif [ -x /overlay/merged/usr/lib/systemd/systemd ]; then
                echo "Switching to real root..."
                exec switch_root /overlay/merged /usr/lib/systemd/systemd
            else
                echo "ERROR: No init found!"
                setsid sh -c 'exec sh </dev/tty1 >/dev/tty1 2>&1'
            fi
        else
            echo "ERROR: Failed to create overlay filesystem!"
            echo "Trying direct mount (read-only)..."
            
            # Fallback to read-only root
            if [ -x /newroot/sbin/init ]; then
                echo "WARNING: Root will be read-only!"
                mount -t tmpfs tmpfs /newroot/run || true
                exec switch_root /newroot /sbin/init
            elif [ -x /newroot/usr/lib/systemd/systemd ]; then
                echo "WARNING: Root will be read-only!"
                mount -t tmpfs tmpfs /newroot/run || true
                exec switch_root /newroot /usr/lib/systemd/systemd
            fi
        fi
    else
        echo "ERROR: Failed to mount squashfs!"
        echo "Trying with explicit loop setup..."
        losetup /dev/loop0 /mnt/live/rootfs.squashfs 2>&1
        if mount -t squashfs /dev/loop0 /newroot 2>&1; then
            echo "Successfully mounted with explicit loop"
            ls -l /newroot
            if [ -x /newroot/sbin/init ]; then
                echo "Switching to /sbin/init..."
                exec switch_root /newroot /sbin/init
            elif [ -x /newroot/usr/lib/systemd/systemd ]; then
                echo "Switching to systemd..."
                exec switch_root /newroot /usr/lib/systemd/systemd
            fi
        fi
    fi
else
    echo "ERROR: rootfs.squashfs not found at /mnt/live/"
    echo "Contents of /mnt:"
    ls -lR /mnt
fi

echo "Boot failed! Dropping to emergency shell..."
setsid sh -c 'exec sh </dev/tty1 >/dev/tty1 2>&1'
"#);
    
    let init_path = init_dir.join("init");
    fs::write(&init_path, init_script)
        .map_err(|e| format!("Failed to write init script: {}", e))?;
    
    let mut perms = fs::metadata(&init_path)
        .map_err(|e| format!("Failed to get init metadata: {}", e))?
        .permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&init_path, perms)
        .map_err(|e| format!("Failed to set init permissions: {}", e))?;
    
    // Create the initrd archive
    let result = RunCommand::new("sh")
        .arg("-c")
        .arg(format!("cd {} && find . | cpio -o -H newc | gzip > {}", 
            init_dir.display(), 
            output.display()))
        .status();
    
    match result {
        Ok(status) if status.success() => {
            println!("Successfully created self-contained initramfs");
            Ok(())
        }
        Ok(status) => Err(format!("Failed to create initramfs archive: exit code {}", status)),
        Err(e) => Err(format!("Failed to run cpio: {}", e)),
    }
}

/// Create a simple busybox-based initramfs as an absolute last resort
/// This requires busybox to be available on the HOST system
fn download_alpine_initramfs(output: &Path) -> Result<(), String> {
    use std::process::Command as RunCommand;
    use std::os::unix::fs::PermissionsExt;
    
    println!("Creating simple busybox-based initramfs (last resort fallback)...");
    println!("NOTE: For production use, add 'dracut' to your package list,");
    println!("      or have your kernel package include a pre-built initramfs.");
    
    // Create a minimal initramfs with just a shell script init
    let temp_dir = tempfile::tempdir()
        .map_err(|e| format!("Failed to create temp dir: {}", e))?;
    
    let init_dir = temp_dir.path();
    
    // Create basic directory structure
    for dir in &["bin", "dev", "proc", "sys", "mnt", "newroot"] {
        fs::create_dir_all(init_dir.join(dir))
            .map_err(|e| format!("Failed to create {}: {}", dir, e))?;
    }
    
    // Create a static busybox-based init script that doesn't rely on bash
    let init_script = r#"#!/bin/busybox sh
# Minimal init script for live CD boot

/bin/busybox --install -s /bin

mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs dev /dev

# Try to boot normally with systemd/init from squashfs if it exists
# Otherwise just drop to a shell

echo "Booting Oreon Live System..."

# Try to find and mount the ISO
for dev in /dev/sr0 /dev/cdrom /dev/sda /dev/sdb /dev/hda; do
    if [ -b "$dev" ]; then
        mount -o ro "$dev" /mnt 2>/dev/null && break
    fi
done

# If squashfs exists, use it as root
if [ -f /mnt/live/filesystem.squashfs ]; then
    mkdir -p /newroot
    mount -t squashfs /mnt/live/filesystem.squashfs /newroot 2>/dev/null
    
    if [ -d /newroot/sbin ]; then
        exec switch_root /newroot /sbin/init
    fi
fi

# Fallback: emergency shell
echo "Failed to find root filesystem"
exec /bin/sh
"#;
    
    let init_path = init_dir.join("init");
    fs::write(&init_path, init_script)
        .map_err(|e| format!("Failed to write init: {}", e))?;
    fs::set_permissions(&init_path, fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("Failed to set init permissions: {}", e))?;
    
    // Check if busybox is available on host
    let busybox_check = RunCommand::new("which").arg("busybox").output();
    if busybox_check.is_err() || !busybox_check.unwrap().status.success() {
        return Err("busybox not found on host system. Please install busybox or add dracut to your package list.".to_string());
    }
    
    // Copy busybox from host
    let result = RunCommand::new("cp")
        .args(&["/bin/busybox", init_dir.join("bin/busybox").to_str().unwrap()])
        .status();
        
    if result.is_err() || !result.unwrap().success() {
        // Try alternate location
        let _ = RunCommand::new("cp")
            .args(&["/usr/bin/busybox", init_dir.join("bin/busybox").to_str().unwrap()])
            .status();
    }
    
    // Create the initramfs archive
    let result = RunCommand::new("sh")
        .arg("-c")
        .arg(format!("cd {} && find . | cpio -H newc -o | gzip -9 > {}", 
            init_dir.display(), output.display()))
        .status();
    
    match result {
        Ok(status) if status.success() => {
            println!("Created minimal busybox initramfs");
            Ok(())
        }
        _ => Err("Failed to create initramfs archive".to_string()),
    }
}

fn create_minimal_initrd_old_broken(rootfs: &Path, initrd_path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    
    // This function is kept for reference but should not be used
    // Creating a working initrd manually is extremely complex
    
    let temp_dir = tempfile::tempdir()
        .map_err(|e| format!("Failed to create temp dir: {}", e))?;
    
    // Create basic initrd structure
    for dir in &["bin", "sbin", "lib", "lib64", "usr/bin", "usr/sbin", "usr/lib", "usr/lib64",
                 "etc", "proc", "sys", "dev", "mnt", "newroot", "run"] {
        fs::create_dir_all(temp_dir.path().join(dir))
            .map_err(|e| format!("Failed to create {} dir: {}", dir, e))?;
    }
    
    // Copy essential binaries from rootfs
    println!("Copying essential binaries from rootfs...");
    let essential_bins = vec![
        "bin/bash",
        "bin/sh",
        "usr/bin/bash",
        "usr/bin/sh",
        "bin/mount",
        "bin/umount", 
        "sbin/mount",
        "sbin/umount",
        "usr/bin/mount",
        "usr/bin/umount",
        "bin/switch_root",
        "sbin/switch_root",
        "usr/sbin/switch_root",
        "sbin/modprobe",
        "usr/sbin/modprobe",
        "sbin/insmod",
        "usr/sbin/insmod",
        "bin/mkdir",
        "bin/sleep",
        "bin/echo",
    ];
    
    let mut copied_bins = Vec::new();
    
    for bin_path in &essential_bins {
        let src = rootfs.join(bin_path);
        if src.exists() && src.is_file() {
            let dst = temp_dir.path().join(bin_path);
            if let Some(parent) = dst.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if fs::copy(&src, &dst).is_ok() {
                let _ = fs::set_permissions(&dst, fs::Permissions::from_mode(0o755));
                copied_bins.push(bin_path.to_string());
                println!("  Copied: {}", bin_path);
            }
        }
    }
    
    // Ensure we have at least a shell
    if !copied_bins.iter().any(|p| p.contains("/sh") || p.contains("/bash")) {
        return Err("No shell binary found in rootfs (bash or sh required)".to_string());
    }
    
    // Copy ALL shared libraries - be aggressive, we need everything
    println!("Copying ALL shared libraries...");
    for lib_dir in &["lib", "lib64", "usr/lib", "usr/lib64"] {
        let src_lib_dir = rootfs.join(lib_dir);
        if !src_lib_dir.exists() {
            continue;
        }
        
        println!("  Copying from {}", lib_dir);
        if let Ok(entries) = fs::read_dir(&src_lib_dir) {
            for entry in entries.flatten() {
                let src = entry.path();
                let file_name = entry.file_name();
                let name_str = file_name.to_string_lossy();
                
                // Copy all .so files and ld-linux
                if name_str.contains(".so") || name_str.starts_with("ld-linux") || name_str.starts_with("ld-") {
                    if let Ok(rel_path) = src.strip_prefix(rootfs) {
                        let dst = temp_dir.path().join(rel_path);
                        if let Some(parent) = dst.parent() {
                            let _ = fs::create_dir_all(parent);
                        }
                        
                        if src.is_symlink() {
                            // Preserve symlinks
                            if let Ok(link_target) = fs::read_link(&src) {
                                let _ = std::os::unix::fs::symlink(link_target, &dst);
                            }
                        } else if src.is_file() {
                            let _ = fs::copy(&src, &dst);
                        }
                    }
                }
            }
        }
    }
    
    // CRITICAL: Ensure ld-linux is in /lib64 for busybox
    // Some binaries are compiled with hardcoded /lib64/ld-linux path
    println!("Ensuring dynamic linker is in /lib64...");
    let usr_lib_ld = temp_dir.path().join("usr/lib/ld-linux-x86-64.so.2");
    let lib64_dir = temp_dir.path().join("lib64");
    let lib64_ld = lib64_dir.join("ld-linux-x86-64.so.2");
    
    if usr_lib_ld.exists() && !lib64_ld.exists() {
        let _ = fs::create_dir_all(&lib64_dir);
        // Copy (not symlink) to ensure it works even if /usr isn't mounted
        if let Err(e) = fs::copy(&usr_lib_ld, &lib64_ld) {
            println!("  Warning: Failed to copy ld-linux to /lib64: {}", e);
        } else {
            println!("  Copied ld-linux-x86-64.so.2 to /lib64/");
        }
    }
    
    // Also ensure all libraries busybox needs are in /lib64
    println!("Copying critical libraries to /lib64...");
    for lib_name in &["libc.so.6", "libm.so.6", "libresolv.so.2", "libpthread.so.0", "libdl.so.2"] {
        let usr_lib_path = temp_dir.path().join(format!("usr/lib/{}", lib_name));
        let lib64_path = temp_dir.path().join(format!("lib64/{}", lib_name));
        
        if usr_lib_path.exists() && !lib64_path.exists() {
            let _ = fs::copy(&usr_lib_path, &lib64_path);
            println!("  Copied {} to /lib64/", lib_name);
        }
    }
    
    // Create sh symlink - try both bin/ and usr/bin/
    let sh_path = temp_dir.path().join("bin/sh");
    if !sh_path.exists() {
        // Check if bash is in bin/ or usr/bin/
        let bash_in_bin = temp_dir.path().join("bin/bash");
        let bash_in_usr_bin = temp_dir.path().join("usr/bin/bash");
        
        if bash_in_bin.exists() {
            let _ = std::os::unix::fs::symlink("bash", &sh_path);
            println!("  Created /bin/sh -> bash symlink");
        } else if bash_in_usr_bin.exists() {
            // Create symlink from /bin/sh to /usr/bin/bash
            let _ = std::os::unix::fs::symlink("../usr/bin/bash", &sh_path);
            println!("  Created /bin/sh -> ../usr/bin/bash symlink");
        }
    }
    
    // Also ensure usr/bin/sh exists if bash is there
    let usr_sh_path = temp_dir.path().join("usr/bin/sh");
    if !usr_sh_path.exists() {
        let bash_path = temp_dir.path().join("usr/bin/bash");
        if bash_path.exists() {
            let _ = std::os::unix::fs::symlink("bash", &usr_sh_path);
            println!("  Created /usr/bin/sh -> bash symlink");
        }
    }
    
    // Copy essential kernel modules for mounting filesystems
    println!("Copying kernel modules...");
    let lib_modules = rootfs.join("lib/modules");
    let kernel_version = if lib_modules.exists() {
        if let Ok(entries) = fs::read_dir(&lib_modules) {
            entries.flatten().next().map(|e| e.file_name().to_string_lossy().to_string())
        } else {
            None
        }
    } else {
        None
    };
    
    if let Some(kver) = kernel_version {
        println!("Copying modules for kernel {}", kver);
        let modules_src = rootfs.join("lib/modules").join(&kver);
        let modules_dst = temp_dir.path().join("lib/modules").join(&kver);
        
        // Essential modules needed for live boot
        let essential_modules = vec![
            "kernel/fs/squashfs",
            "kernel/fs/isofs",
            "kernel/fs/overlayfs",
            "kernel/drivers/block/loop.ko",
            "kernel/drivers/cdrom",
            "kernel/drivers/ata",
            "kernel/drivers/scsi",
        ];
        
        for module_path in &essential_modules {
            let src_path = modules_src.join(module_path);
            if src_path.exists() {
                if src_path.is_dir() {
                    // Copy entire directory
                    if let Ok(entries) = fs::read_dir(&src_path) {
                        for entry in entries.flatten() {
                            let entry_path = entry.path();
                            if entry_path.extension().and_then(|s| s.to_str()) == Some("ko") {
                                let rel = entry_path.strip_prefix(&modules_src).ok();
                                if let Some(r) = rel {
                                    let dst = modules_dst.join(r);
                                    if let Some(parent) = dst.parent() {
                                        let _ = fs::create_dir_all(parent);
                                    }
                                    let _ = fs::copy(&entry_path, &dst);
                                }
                            }
                        }
                    }
                } else {
                    let rel = src_path.strip_prefix(&modules_src).ok();
                    if let Some(r) = rel {
                        let dst = modules_dst.join(r);
                        if let Some(parent) = dst.parent() {
                            let _ = fs::create_dir_all(parent);
                        }
                        let _ = fs::copy(&src_path, &dst);
                    }
                }
            }
        }
        
        // Copy modules.* files for module loading
        for file in &["modules.dep", "modules.dep.bin", "modules.alias", "modules.alias.bin"] {
            let src = modules_src.join(file);
            let dst = modules_dst.join(file);
            if src.exists() {
                let _ = fs::copy(&src, &dst);
            }
        }
    }
    
    println!("Initrd preparation complete");
    
    // Create a live boot init script  
    let init_script = r#"#!/bin/sh
set -x

echo "===== OREON LIVE BOOT INIT ====="
echo "Init script starting..."

# Mount essential filesystems
echo "Mounting proc..."
mount -t proc proc /proc || echo "Failed to mount proc"
echo "Mounting sysfs..."
mount -t sysfs sysfs /sys || echo "Failed to mount sysfs"
echo "Mounting devtmpfs..."
mount -t devtmpfs devtmpfs /dev || echo "Failed to mount devtmpfs"
mkdir -p /dev/pts
echo "Mounting devpts..."
mount -t devpts devpts /dev/pts || echo "Failed to mount devpts"

echo "===== Filesystems mounted ====="
echo "Oreon Live Boot - Initializing..."

# Load essential kernel modules
echo "Loading kernel modules..."
modprobe loop 2>/dev/null || echo "loop module already loaded or not needed"
modprobe squashfs 2>/dev/null || echo "squashfs module already loaded or not needed"
modprobe isofs 2>/dev/null || echo "isofs module already loaded or not needed"
modprobe overlay 2>/dev/null || echo "overlay module already loaded or not needed"
modprobe sr_mod 2>/dev/null || echo "sr_mod module already loaded or not needed"
modprobe cdrom 2>/dev/null || echo "cdrom module already loaded or not needed"

# Wait for CD/DVD device
echo "Waiting for CD/DVD device..."
sleep 3

# Try to mount the CD/ISO
mkdir -p /mnt/cdrom
for device in /dev/sr0 /dev/cdrom /dev/scd0; do
    if [ -b "$device" ]; then
        echo "Trying to mount $device..."
        if mount -t iso9660 -o ro "$device" /mnt/cdrom 2>/dev/null; then
            echo "Mounted ISO from $device"
            break
        fi
    fi
done

# Check if squashfs exists
if [ -f /mnt/cdrom/live/filesystem.squashfs ]; then
    echo "Found squashfs filesystem"
    
    # Mount the squashfs
    mkdir -p /mnt/squash
    if mount -t squashfs -o loop,ro /mnt/cdrom/live/filesystem.squashfs /mnt/squash; then
        echo "Mounted squashfs"
        
        # Create overlay
        mkdir -p /mnt/overlay /mnt/work
        mount -t tmpfs tmpfs /mnt/overlay
        mkdir -p /mnt/overlay/upper /mnt/overlay/work
        
        # Mount overlay (union of squashfs and tmpfs)
        if mount -t overlay overlay -o lowerdir=/mnt/squash,upperdir=/mnt/overlay/upper,workdir=/mnt/overlay/work /newroot 2>/dev/null; then
            echo "Mounted overlay filesystem"
        else
            echo "Overlay mount failed, using squashfs directly"
            mount --bind /mnt/squash /newroot
        fi
        
        # Create mount points in new root
        mkdir -p /newroot/proc /newroot/sys /newroot/dev /newroot/run /newroot/tmp
        
        # Switch to new root
        echo "Switching to new root..."
        exec switch_root /newroot /sbin/init
    else
        echo "Failed to mount squashfs"
    fi
else
    echo "ERROR: No squashfs filesystem found at /mnt/cdrom/live/filesystem.squashfs"
fi

echo "Boot failed, dropping to shell"
exec /bin/sh
"#;
    
    fs::write(temp_dir.path().join("init"), init_script)
        .map_err(|e| format!("Failed to write init script: {}", e))?;
    
    fs::set_permissions(temp_dir.path().join("init"), fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("Failed to set init permissions: {}", e))?;
    
    // Create initrd using find and cpio
    let output = RunCommand::new("bash")
        .arg("-c")
        .arg(format!("cd '{}' && find . | cpio -o -H newc | gzip > '{}'",
            temp_dir.path().display(),
            initrd_path.display()))
        .output()
        .map_err(|e| format!("Failed to create initrd: {}", e))?;
    
    if !output.status.success() {
        return Err(format!("Failed to create initrd: {}", 
            String::from_utf8_lossy(&output.stderr)));
    }
    
    println!("Created initrd with live boot support");
    
    Ok(())
}

fn create_squashfs(rootfs: &Path, output: &Path) -> Result<(), String> {
    // Check for mksquashfs
    let mksquashfs = if RunCommand::new("which")
        .arg("mksquashfs")
        .output()
        .is_ok_and(|o| o.status.success()) {
        "mksquashfs"
    } else {
        return Err("mksquashfs not found. Please install squashfs-tools.".to_string());
    };
    
    let output_cmd = RunCommand::new(mksquashfs)
        .arg(rootfs)
        .arg(output)
        .arg("-comp")
        .arg("xz")
        .arg("-e")
        .arg("boot")
        .arg("-e")
        .arg("dev")
        .arg("-e")
        .arg("proc")
        .arg("-e")
        .arg("sys")
        .arg("-e")
        .arg("tmp")
        .arg("-e")
        .arg("run")
        .arg("-e")
        .arg("mnt")
        .output()
        .map_err(|e| format!("Failed to execute mksquashfs: {}", e))?;
    
    if !output_cmd.status.success() {
        let stderr = String::from_utf8_lossy(&output_cmd.stderr);
        return Err(format!("Failed to create squashfs: {}", stderr));
    }
    
    Ok(())
}

async fn fetch_packages_from_repos(
    package_names: Vec<String>,
    repositories: &[OriginKind],
) -> Result<Vec<metadata::InstallPackage>, String> {
    use metadata::ProcessedMetaData;
    use std::io::Write;
    
    println!("[FETCH] Searching for {} packages in {} repositories", package_names.len(), repositories.len());
    std::io::stdout().flush().unwrap();
    
    let mut packages = Vec::new();
    
    for name in package_names {
        println!("[FETCH] Looking for package: {}", name);
        std::io::stdout().flush().unwrap();
        if let Some(metadata) = ProcessedMetaData::get_metadata(&name, None, repositories, true).await {
            println!("[FETCH] Found package {} version {}", metadata.name, metadata.version);
            std::io::stdout().flush().unwrap();
            // Resolve dependencies using the template repositories
            let mut run_deps = Vec::new();
            for dep in &metadata.runtime_dependencies {
                let dep_name = match dep {
                    metadata::depend_kind::DependKind::Latest(n) => n.clone(),
                    metadata::depend_kind::DependKind::Specific(dv) => dv.name.clone(),
                    metadata::depend_kind::DependKind::Volatile(n) => n.clone(),
                };
                if let Some(dep_metadata) = ProcessedMetaData::get_metadata(&dep_name, None, repositories, true).await {
                    run_deps.push(dep_metadata);
                }
            }
            
            let mut build_deps = Vec::new();
            for dep in &metadata.build_dependencies {
                let dep_name = match dep {
                    metadata::depend_kind::DependKind::Latest(n) => n.clone(),
                    metadata::depend_kind::DependKind::Specific(dv) => dv.name.clone(),
                    metadata::depend_kind::DependKind::Volatile(n) => n.clone(),
                };
                if let Some(dep_metadata) = ProcessedMetaData::get_metadata(&dep_name, None, repositories, true).await {
                    build_deps.push(dep_metadata);
                }
            }
            
            packages.push(metadata::InstallPackage {
                metadata,
                run_deps,
                build_deps,
            });
        } else {
            println!("[FETCH] WARNING: Package {} not found in repositories!", name);
            std::io::stdout().flush().unwrap();
        }
    }
    
    println!("[FETCH] Successfully fetched {} packages", packages.len());
    std::io::stdout().flush().unwrap();
    
    Ok(packages)
}

fn setup_live_init(iso_root: &Path, _template: Option<&IsoTemplate>) -> Result<(), String> {
    // Create a simple init script for the live environment
    let init_path = iso_root.join("init");
    let init_content = r#"#!/bin/sh
mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev

# Start shell
exec /bin/sh
"#;
    
    fs::write(&init_path, init_content)
        .map_err(|e| format!("Failed to write init: {}", e))?;
    
    // Make init executable (we'll set this in the ISO)
    
    Ok(())
}

fn apply_template_config(iso_root: &Path, config: &TemplateConfig) -> Result<(), String> {
    println!("Applying template configuration...");
    
    // Set hostname
    if let Some(hostname) = &config.hostname {
        let hostname_path = iso_root.join("etc/hostname");
        fs::write(&hostname_path, hostname)
            .map_err(|e| format!("Failed to write hostname: {}", e))?;
        
        // Also update /etc/hosts
        let hosts_path = iso_root.join("etc/hosts");
        let hosts_content = format!("127.0.0.1\tlocalhost\n127.0.1.1\t{}\n", hostname);
        fs::write(&hosts_path, hosts_content)
            .map_err(|e| format!("Failed to write hosts file: {}", e))?;
    }
    
    // Create passwordless live user (default to "live" if not specified)
    let username = config.username.as_deref().unwrap_or("live");
    let passwd_path = iso_root.join("etc/passwd");
    let shadow_path = iso_root.join("etc/shadow");
    
    // Add user to passwd with no password (x means password in shadow file)
    let passwd_line = format!("{}:x:1000:1000:Live User:/home/{}:/bin/sh\n", username, username);
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&passwd_path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, passwd_line.as_bytes()))
        .map_err(|e| format!("Failed to write passwd: {}", e))?;
    
    // Create passwordless entry in shadow (empty password hash means no password)
    // Using empty password hash for passwordless login
    let shadow_line = format!("{}::0:0:99999:7:::\n", username);
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&shadow_path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, shadow_line.as_bytes()))
        .map_err(|e| format!("Failed to write shadow: {}", e))?;
    
    // Create home directory
    let home_dir = iso_root.join("home").join(username);
    fs::create_dir_all(&home_dir)
        .map_err(|e| format!("Failed to create home directory: {}", e))?;
    
    Ok(())
}

fn create_iso_image(iso_root: &Path, output_path: &Path) -> Result<(), String> {
    // Use grub-mkrescue - this is the standard tool used by most Linux distros
    // It automatically handles creating boot images and ISO in one step
    let check_tool = |tool: &str| -> bool {
        RunCommand::new("/usr/bin/which")
            .arg(tool)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    
    let grub_tool = if check_tool("grub-mkrescue") {
        "grub-mkrescue"
    } else if check_tool("grub2-mkrescue") {
        "grub2-mkrescue"
    } else {
        return Err("grub-mkrescue not found. Please install grub2-tools.".to_string());
    };
    
    println!("Using {} to create bootable ISO...", grub_tool);
    
    // grub-mkrescue automatically creates boot images and ISO
    // It expects the ISO root directory as the last argument
    let mut cmd = RunCommand::new(grub_tool);
    cmd.arg("-o")
        .arg(output_path)
        .arg("-v")
        .arg(format!("--directory={}", find_grub_lib_dir()?))
        .arg(iso_root);
    
    let output = cmd.output()
        .map_err(|e| format!("Failed to execute {}: {}", grub_tool, e))?;
    
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Err(format!("ISO creation failed:\nstdout: {}\nstderr: {}", stdout, stderr));
    }
    
    Ok(())
}

fn find_grub_lib_dir() -> Result<String, String> {
    // Try common GRUB library directories
    let possible_dirs = vec![
        "/usr/lib/grub/i386-pc",
        "/usr/lib/grub2/i386-pc",
        "/usr/share/grub/i386-pc",
        "/usr/share/grub2/i386-pc",
    ];
    
    for dir in possible_dirs {
        if Path::new(dir).exists() {
            return Ok(dir.to_string());
        }
    }
    
    Err("Could not find GRUB library directory. Please install grub2-pc or grub-pc.".to_string())
}

