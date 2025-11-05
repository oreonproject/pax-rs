use commands::Command;
use metadata::{get_packages, ProcessedMetaData, InstalledMetaData};
use settings::SettingsYaml;
use settings::acquire_lock;
use statebox::StateBox;
use tokio::runtime::Runtime;
use utils::PostAction;
use utils::choice;
use std::path::Path;

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "install",
        vec![String::from("i")],
        "Install the application from a specified path",
        vec![utils::specific_flag(), utils::yes_flag(), utils::from_flag(), utils::allow_overwrite_flag()],
        None,
        run,
        hierarchy,
    )
}

fn run(states: &StateBox, args: Option<&[String]>) -> PostAction {
    let args_vec = match args {
        None => return PostAction::NothingToDo,
        Some(args) => args.to_vec(),
    };
    
    // Check for already installed packages before acquiring lock
    let is_local_package = |arg: &str| {
        let path = Path::new(arg);
        if !path.exists() {
            return false;
        }
        match path.extension().and_then(|ext| ext.to_str()).map(|ext| ext.to_ascii_lowercase()) {
            Some(ext) if matches!(ext.as_str(), "pax" | "deb" | "rpm") => true,
            _ => false,
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
    
    // Handle local package files
    for package_file in local_package_files {
        match runtime.block_on(ProcessedMetaData::get_metadata_from_local_package(&package_file)) {
            Ok(metadata) => {
                // Create a mock InstallPackage for local files
                let install_package = metadata::InstallPackage {
                    metadata,
                    run_deps: Vec::new(),
                    build_deps: Vec::new(),
                };
                install_packages.push(install_package);
            }
            Err(fault) => return PostAction::Fuck(format!("Failed to parse local package `{}`: {}", package_file, fault)),
        }
    }
    
    // Handle remote packages
    if !data.is_empty() {
        let preferred_source = states.get("from_repo").and_then(|v: &String| Some(v.as_str()));
        let package_names: Vec<String> = data.iter().map(|(name, _)| (*name).clone()).collect();
        let remote_data = match runtime.block_on(get_packages(package_names, preferred_source)) {
        Ok(data) => data,
        Err(fault) => return PostAction::Fuck(fault),
    };
        
        // Check for already installed packages
        let mut filtered_data = Vec::new();
        for package in remote_data {
            if let Ok(installed) = InstalledMetaData::open(&package.metadata.name) {
                if installed.version == package.metadata.version {
                    println!("Package `{}` version `{}` is already installed.", package.metadata.name, package.metadata.version);
                    continue;
                } else {
                    println!("Package `{}` is installed with version `{}`, but you're trying to install version `{}`.", 
                            package.metadata.name, installed.version, package.metadata.version);
                    println!("Consider using `pax upgrade` or `pax remove` first.");
                    continue;
                }
            }
            filtered_data.push(package);
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
