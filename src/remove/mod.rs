use crate::database::Database;
use crate::resolver::DependencyResolver;
use crate::store::PackageStore;
use crate::symlinks::SymlinkManager;
use crate::{Command, PostAction, StateBox};
use nix::unistd;
use std::io::Write;

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "remove",
        vec![String::from("rm"), String::from("uninstall")],
        "Remove installed packages",
        Vec::new(),
        None,
        run,
        hierarchy,
    )
}

fn run(_: &StateBox, args: Option<&[String]>) -> PostAction {
    // check for root
    let euid = unistd::geteuid();
    if euid.as_raw() != 0 {
        return PostAction::Elevate;
    }

    let args = match args {
        None => {
            println!("Usage: pax remove <package1> [package2] [...]");
            return PostAction::Return;
        }
        Some(args) => args,
    };

    // initialize components
    let db = match Database::open("/opt/pax/db/pax.db") {
        Ok(db) => db,
        Err(e) => {
            println!("Failed to open database: {}", e);
            return PostAction::Return;
        }
    };

    let store = match PackageStore::new() {
        Ok(s) => s,
        Err(e) => {
            println!("Failed to initialize package store: {}", e);
            return PostAction::Return;
        }
    };

    let resolver = DependencyResolver::new(db.clone());
    let symlink_mgr = SymlinkManager::new(db.clone(), "/opt/pax/links");

    // check which packages are installed
    let mut to_remove = Vec::new();
    for pkg_name in args {
        match db.is_installed(pkg_name) {
            Ok(true) => to_remove.push(pkg_name.to_string()),
            Ok(false) => {
                println!("Package not installed: {}", pkg_name);
                return PostAction::Return;
            }
            Err(e) => {
                println!("Database error: {}", e);
                return PostAction::Return;
            }
        }
    }

    // check for reverse dependencies
    let mut has_rdeps = false;
    for pkg_name in &to_remove {
        match resolver.calculate_removal_impact(pkg_name) {
            Ok(rdeps) => {
                if !rdeps.is_empty() {
                    println!("\nWarning: {} is required by:", pkg_name);
                    for rdep in rdeps {
                        println!("  - {}", rdep);
                    }
                    has_rdeps = true;
                }
            }
            Err(e) => {
                println!("Failed to check dependencies: {}", e);
                return PostAction::Return;
            }
        }
    }

    if has_rdeps {
        println!("\nRemoving these packages may break dependent packages.");
    }

    println!("\nPackages to remove:");
    for pkg in &to_remove {
        println!("  - {}", pkg);
    }

    // ask confirmation
    print!("\nProceed with removal? [y/N]: ");
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        println!("Failed to read input");
        return PostAction::Return;
    }
    
    if !["yes", "y"].contains(&input.trim().to_lowercase().as_str()) {
        println!("Removal cancelled");
        return PostAction::Return;
    }

    // remove each package
    for pkg_name in to_remove {
        if let Err(e) = remove_package(&pkg_name, &db, &store, &symlink_mgr) {
            println!("Failed to remove {}: {}", pkg_name, e);
            println!("Removal aborted");
            return PostAction::Return;
        }
    }

    // update library cache
    println!("\nUpdating system library cache...");
    if let Err(e) = symlink_mgr.update_library_cache() {
        eprintln!("Warning: Failed to update library cache: {}", e);
    }

    println!("\n\x1B[32mRemoval complete!\x1B[0m");
    PostAction::Return
}

fn remove_package(
    pkg_name: &str,
    db: &Database,
    store: &PackageStore,
    symlink_mgr: &SymlinkManager,
) -> Result<(), String> {
    println!("\nRemoving {}...", pkg_name);

    // get package info
    let pkg_info = db.get_package_info(pkg_name)
        .map_err(|e| format!("Failed to get package info: {}", e))?
        .ok_or_else(|| format!("Package not found: {}", pkg_name))?;

    let pkg_id = db.get_package_id(pkg_name)
        .map_err(|e| format!("Failed to get package ID: {}", e))?
        .ok_or_else(|| format!("Package not found: {}", pkg_name))?;

    // remove symlinks
    println!("Removing symlinks...");
    symlink_mgr.remove_symlinks(pkg_id)?;

    // remove from store
    println!("Removing files...");
    store.remove_package(&pkg_info.hash)?;

    // remove from database
    println!("Updating database...");
    db.remove_package(pkg_name)
        .map_err(|e| format!("Failed to remove from database: {}", e))?;

    println!("  {} removed successfully", pkg_name);

    Ok(())
}

