use commands::Command;
use metadata::{get_packages, ProcessedMetaData, InstalledMetaData};
use settings::SettingsYaml;
use settings::acquire_lock;
use statebox::StateBox;
use tokio::runtime::Runtime;
use utils::PostAction;
use utils::choice;
use std::path::Path;
use futures::future::join_all;

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "install",
        vec![String::from("i")],
        "Install the application from a specified path",
        vec![utils::specific_flag(), utils::yes_flag(), utils::from_flag(), utils::allow_overwrite_flag(), utils::refresh_flag()],
        None,
        run,
        hierarchy,
    )
}

fn run(states: &StateBox, args: Option<&[String]>) -> PostAction {
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::fs::OpenOptions;
    use std::io::Write;
    
    let start_time = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
        let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"timing\",\"hypothesisId\":\"DELAY\",\"location\":\"src/install/mod.rs:24\",\"message\":\"install_command_start\",\"data\":{{\"timestamp\":{}}},\"timestamp\":{}}}", start_time, start_time);
    }
    
    let args_vec = match args {
        None => return PostAction::NothingToDo,
        Some(args) => args.to_vec(),
    };
    
    // Check for already installed packages before acquiring lock
    let is_local_package = |arg: &str| {
        // Fast check: if it contains path separators or obvious file extensions, check filesystem
        if arg.contains('/') || arg.contains('\\') || arg.ends_with(".pax") || arg.ends_with(".deb") || arg.ends_with(".rpm") {
            let path = Path::new(arg);
            if !path.exists() {
                return false;
            }
            match path.extension().and_then(|ext| ext.to_str()).map(|ext| ext.to_ascii_lowercase()) {
                Some(ext) if matches!(ext.as_str(), "pax" | "deb" | "rpm") => true,
                _ => false,
            }
        } else {
            // Doesn't look like a file path, assume it's a package name
            false
        }
    };

    let has_local_package = args_vec.iter().any(|arg| is_local_package(arg));
    
    if has_local_package {
        let Ok(runtime) = Runtime::new() else {
            return PostAction::Fuck(String::from("Error creating runtime!"));
        };
        
        for package_file in args_vec.iter().filter(|arg| is_local_package(arg)) {
            match runtime.block_on(ProcessedMetaData::get_metadata_from_local_package(package_file)) {
                Ok(metadata) => {
                    // Check if package is already installed
                    if let Ok(installed) = InstalledMetaData::open(&metadata.name) {
                        if installed.version == metadata.version {
                            println!("Package `{}` version `{}` is already installed.", metadata.name, metadata.version);
                            return PostAction::Return;
                        } else {
                            println!("Package `{}` is installed with version `{}`, but you're trying to install version `{}`.", 
                                    metadata.name, installed.version, metadata.version);
                            println!("Consider using `pax upgrade` or `pax remove` first.");
                            return PostAction::Return;
                        }
                    }
                }
                Err(fault) => return PostAction::Fuck(format!("Failed to parse local package `{}`: {}", package_file, fault)),
            }
        }
    }
    
    match acquire_lock() {
        Ok(Some(action)) => return action,
        Err(fault) => return PostAction::Fuck(fault),
        _ => (),
    }
    
    if !has_local_package {
    print!("Reading sources...");
    let settings = match SettingsYaml::get_settings() {
        Ok(settings) => settings,
        Err(_) => return PostAction::PullSources,
    };
    println!(" Found {} repositories", settings.sources.len());
    if settings.sources.is_empty() && settings.mirror_list.is_none() {
        return PostAction::PullSources;
        }
    }
    let mut data = Vec::new();
    let mut local_package_files = Vec::new();
    
    if states.get("specific").is_some_and(|x| *x) {
        let mut args_iter = args_vec.iter();
        while let Some(name) = args_iter.next()
            && let Some(ver) = args_iter.next()
        {
            if is_local_package(name) {
                local_package_files.push(name.to_string());
            } else {
            data.push((name, Some(ver)));
            }
        }
    } else {
        for arg in &args_vec {
            if is_local_package(arg) {
                local_package_files.push(arg.to_string());
            } else {
                data.push((arg, None));
    }
        }
    }
    
    let Ok(runtime) = Runtime::new() else {
        return PostAction::Fuck(String::from("Error creating runtime!"));
    };
    
    let mut install_packages = Vec::new();
    
    // Handle local package files in parallel
    if !local_package_files.is_empty() {
        let local_futures: Vec<_> = local_package_files.iter().map(|package_file| {
            let package_file = package_file.clone();
            async move {
                ProcessedMetaData::get_metadata_from_local_package(&package_file).await
            }
        }).collect();
        
        let local_results = runtime.block_on(join_all(local_futures));
        for result in local_results {
            match result {
                Ok(metadata) => {
                    // Create a mock InstallPackage for local files
                    let install_package = metadata::InstallPackage {
                        metadata,
                        run_deps: Vec::new(),
                        build_deps: Vec::new(),
                    };
                    install_packages.push(install_package);
                }
                Err(fault) => return PostAction::Fuck(format!("Failed to parse local package: {}", fault)),
            }
        }
    }
    
    // Handle remote packages
    if !data.is_empty() {
        let preferred_source = states.get("from_repo").and_then(|v: &String| Some(v.as_str()));

        // Separate packages: those with specific versions vs those without
        let mut packages_with_versions: Vec<String> = Vec::new();
        let mut packages_without_versions: Vec<String> = Vec::new();

        for (name, version) in &data {
            if version.is_some() {
                packages_with_versions.push((*name).clone());
            } else {
                packages_without_versions.push((*name).clone());
            }
        }

        // For packages without specific versions, check if they're already installed
        // If so, skip remote fetching for them
        let mut packages_to_fetch = packages_with_versions;
        for name in packages_without_versions {
            if let Ok(installed) = InstalledMetaData::open(&name) {
                println!("Package `{}` is already installed (version {}).", name, installed.version);
                continue;
            }
            packages_to_fetch.push(name);
        }

        // Only fetch remote data for packages that need it
        let mut filtered_data = Vec::new();
        if !packages_to_fetch.is_empty() {
            use std::time::{SystemTime, UNIX_EPOCH};
            use std::fs::OpenOptions;
            use std::io::Write;
            
            let before_get_packages = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
            if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("/home/blester/pax-rs/.cursor/debug.log") {
                let _ = writeln!(file, "{{\"sessionId\":\"debug-session\",\"runId\":\"timing\",\"hypothesisId\":\"DELAY\",\"location\":\"src/install/mod.rs:179\",\"message\":\"before_get_packages\",\"data\":{{\"timestamp\":{}}},\"timestamp\":{}}}", before_get_packages, before_get_packages);
            }
            
            let refresh_cache = states.get("refresh_cache").is_some_and(|x: &bool| *x);
            let remote_data = match runtime.block_on(get_packages(packages_to_fetch, preferred_source, refresh_cache)) {
                Ok(data) => data,
                Err(fault) => return PostAction::Fuck(fault),
            };

            // Check versions for packages that had specific versions requested
            for package in remote_data {
                let requested_version = data.iter().find(|(n, _)| n.eq_ignore_ascii_case(&package.metadata.name)).and_then(|(_, v)| v.as_ref());

                if let Some(requested_ver) = requested_version {
                    if let Ok(installed) = InstalledMetaData::open(&package.metadata.name) {
                        if installed.version == **requested_ver {
                            println!("Package `{}` version `{}` is already installed.", package.metadata.name, requested_ver);
                            continue;
                        } else {
                            println!("Package `{}` is installed with version `{}`, but you're trying to install version `{}`.",
                                    package.metadata.name, installed.version, requested_ver);
                            println!("Consider using `pax upgrade` or `pax remove` first.");
                            continue;
                        }
                    }
                }
                filtered_data.push(package);
            }
        }
        
        install_packages.extend(filtered_data);
    }
    
    let data = install_packages;

    println!();
    if data.is_empty() {
        return PostAction::NothingToDo;
    }
    println!(
        "\nThe following package(s) will be INSTALLED: \x1B[92m{}\x1B[0m",
        data.iter()
            .fold(String::new(), |acc, x| format!("{acc} {}", x.metadata.name))
            .trim()
    );
    let has_dependencies = data.iter().any(|x| !x.run_deps.is_empty() || !x.build_deps.is_empty());
    if has_dependencies {
        println!(
            "The following package(s) will be INSTALLED (dependencies):  \x1B[93m{}\x1B[0m",
            data.iter()
                .flat_map(|x| x.list_deps(true))
                .fold(String::new(), |acc, x| format!("{acc} {x}"))
                .trim()
        );
    }

    if states.get("yes").is_none_or(|x: &bool| !*x) {
        let prompt = if has_dependencies {
            "Continue with installation?"
        } else {
            "Proceed with installation?"
        };
        match choice(prompt, true) {
            Err(message) => return PostAction::Fuck(message),
            Ok(false) => return PostAction::Fuck(String::from("Aborted.")),
            Ok(true) => (),
        };
    }
    let allow_overwrite = states.get("allow_overwrite").is_some_and(|x: &bool| *x);
    
    for data in data {
        if allow_overwrite {
            if let Err(fault) = data.install_with_overwrite(&runtime) {
                return PostAction::Fuck(fault);
            }
        } else {
            if let Err(fault) = data.install(&runtime) {
                return PostAction::Fuck(fault);
            }
        }
    }
    PostAction::Return
}
