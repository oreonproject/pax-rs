use commands::Command;
use metadata;
use settings::acquire_lock;
use statebox::StateBox;
use tokio::runtime::Runtime;
use utils::{PostAction, choice};
use std::io;

pub fn build_remove(hierarchy: &[String]) -> Command {
    Command::new(
        "remove",
        vec![String::from("r")],
        "Removes a package, whilst maintaining any user-made configurations",
        vec![utils::specific_flag(), utils::yes_flag()],
        None,
        remove,
        hierarchy,
    )
}

pub fn build_purge(hierarchy: &[String]) -> Command {
    Command::new(
        "purge",
        vec![String::from("p")],
        "Removes a package, WITHOUT maintaining any user-made configurations",
        vec![utils::specific_flag(), utils::yes_flag()],
        None,
        purge,
        hierarchy,
    )
}

fn remove(states: &StateBox, args: Option<&[String]>) -> PostAction {
    run(states, args, false)
}

fn purge(states: &StateBox, args: Option<&[String]>) -> PostAction {
    run(states, args, true)
}

fn run(states: &StateBox, args: Option<&[String]>, purge: bool) -> PostAction {
    match acquire_lock() {
        Ok(Some(action)) => return action,
        Err(fault) => return PostAction::Fuck(fault),
        _ => (),
    }
    let mut args = match args {
        None => return PostAction::NothingToDo,
        Some(args) => args.iter(),
    };
    let mut data = Vec::new();
    if states.get("specific").is_some_and(|x| *x) {
        while let Some(name) = args.next()
            && let Some(ver) = args.next()
        {
            data.push((name, Some(ver)));
        }
    } else {
        args.for_each(|x| data.push((x, None)));
    }
    let Ok(runtime) = Runtime::new() else {
        return PostAction::Fuck(String::from("Error creating runtime!"));
    };
    
    if data.is_empty() {
                return PostAction::NothingToDo;
            }
    
    // Get package names to remove
    let package_names: Vec<String> = data.iter().map(|(name, _)| (*name).clone()).collect();
    
    // Collect dependencies of packages to be removed BEFORE removal (for purge only)
    use std::collections::HashSet;
    let mut removed_deps = HashSet::new();
    if purge {
        for package_name in &package_names {
            if let Ok(metadata) = metadata::InstalledMetaData::open(package_name) {
                for dep in &metadata.dependencies {
                    if dep.name != *package_name {
                        removed_deps.insert(dep.name.clone());
                    }
                }
            }
        }
    }
    
            let msg = if purge { "PURGED: " } else { "REMOVED:" };
            println!(
                "\nThe following package(s) will be {msg}  \x1B[91m{}\x1B[0m",
        package_names.join(" ")
            );
    
    // Show dependencies that might become orphans
    if purge && !removed_deps.is_empty() {
        let dep_vec: Vec<String> = removed_deps.iter().cloned().collect();
                println!(
            "\nDependencies that may no longer be needed: \x1B[93m{}\x1B[0m",
            dep_vec.join(", ")
                );
            }
            
            // Always prompt for confirmation unless --yes flag is used
                if states.get("yes").is_none_or(|x: &bool| !*x) {
                let prompt = if purge { "Proceed with purging?" } else { "Proceed with removal?" };
                match choice(prompt, true) {
                        Err(message) => return PostAction::Fuck(message),
                        Ok(false) => return PostAction::Fuck(String::from("Aborted.")),
                        Ok(true) => (),
                    };
            }
    
    // Actually remove the packages
    for package_name in &package_names {
        if let Err(e) = remove_package(package_name, purge) {
            return PostAction::Fuck(format!("Failed to remove package {}: {}", package_name, e));
        }
    }
    
    println!("\x1B[92mSuccessfully removed package(s): {}\x1B[0m", package_names.join(", "));
    
    // Find orphaned dependencies AFTER removing packages (only for purge)
    let orphans = if purge {
        find_orphaned_dependencies(&package_names, &removed_deps)
    } else {
        Vec::new()
    };
    
    // Clean up orphaned dependencies (only for purge)
    if !orphans.is_empty() {
        println!("\n\x1B[93mThe following dependencies are no longer needed:\x1B[0m \x1B[93m{}\x1B[0m", orphans.join(", "));
        println!("\x1B[93mRemove them? [y/N]:\x1B[0m ");
        
        let mut input = String::new();
        if io::stdin().read_line(&mut input).is_ok() && input.trim().to_lowercase() == "y" {
            for orphan in &orphans {
                let _ = remove_package(orphan, purge);
            }
            println!("\x1B[92mRemoved orphaned dependencies: {}\x1B[0m", orphans.join(", "));
            }
    }
    
            PostAction::Return
        }

fn find_orphaned_dependencies(removed_packages: &[String], _removed_deps: &std::collections::HashSet<String>) -> Vec<String> {
    
    // Get all currently installed packages
    let all_packages = match metadata::list_installed_packages(false, false, None) {
        Ok(packages) => packages,
        Err(_) => return Vec::new(),
    };
    
    // Collect dependencies that were installed by the removed packages
    let mut potential_orphans = std::collections::HashSet::new();
    for package in &all_packages {
        if let Some(installed_by) = &package.installed_by {
            if removed_packages.contains(installed_by) {
                potential_orphans.insert(package.name.clone());
            }
        }
    }
    
    // Check which of these are actually orphans (not needed by other packages)
    let mut orphans = Vec::new();
    for orphan_candidate in &potential_orphans {
        let mut is_needed = false;
        
        // Check if any remaining package depends on this
        for package in &all_packages {
            // Skip removed packages
            if removed_packages.contains(&package.name) {
                continue;
            }
            
            // Skip the orphan candidate itself
            if package.name == *orphan_candidate {
                continue;
            }
            
            // Check if this package depends on the orphan candidate
            if package.dependencies.iter().any(|d| d.name == *orphan_candidate) {
                is_needed = true;
                break;
            }
        }
        
        // If not needed by any other package, it's a true orphan
        if !is_needed {
            orphans.push(orphan_candidate.clone());
        }
    }
    
    orphans
}

fn remove_package(package_name: &str, purge: bool) -> Result<(), String> {
    use std::fs;
    
    let installed_dir = utils::get_metadata_dir()?;
    let package_file = installed_dir.join(format!("{}.json", package_name));
    
    // File must exist for removal
    if !package_file.exists() {
        return Err(format!("Package {} is not installed", package_name));
    }
    
    // Remove the package's file manifest if purge is requested
    if purge {
        let manifest_file = installed_dir.join(format!("{}.manifest.json", package_name));
        if manifest_file.exists() {
            let _ = fs::remove_file(&manifest_file);
        }
        
        // TODO: Clean up backup files
    }
    
    // Remove the package metadata file
    fs::remove_file(&package_file)
        .map_err(|e| format!("Failed to remove package metadata: {}", e))?;
    
    Ok(())
}
