use crate::database::Database;
use crate::download::DownloadManager;
use crate::store::PackageStore;
use crate::symlinks::SymlinkManager;
use crate::{Command, PostAction, StateBox};
use nix::unistd;
use std::io::Write;

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "clean",
        vec![String::from("gc")],
        "Clean up cache and orphaned packages",
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

    let all = args.map(|a| a.contains(&"--all".to_string())).unwrap_or(false);

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

    let downloader = match DownloadManager::new() {
        Ok(d) => d,
        Err(e) => {
            println!("Failed to initialize download manager: {}", e);
            return PostAction::Return;
        }
    };

    let symlink_mgr = SymlinkManager::new(db.clone(), "/opt/pax/links");

    println!("PAX Cleanup\n");

    // 1. Clean orphaned symlinks
    println!("Cleaning orphaned symlinks...");
    match symlink_mgr.cleanup_orphaned() {
        Ok(cleaned) => {
            if cleaned.is_empty() {
                println!("  No orphaned symlinks found");
            } else {
                println!("  Removed {} orphaned symlinks", cleaned.len());
                for link in &cleaned {
                    println!("    - {}", link);
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to clean symlinks: {}", e);
        }
    }

    // 2. Garbage collect orphaned packages in store
    println!("\nGarbage collecting orphaned packages...");
    
    let installed = match db.list_packages() {
        Ok(pkgs) => pkgs,
        Err(e) => {
            eprintln!("Failed to list packages: {}", e);
            return PostAction::Return;
        }
    };

    let installed_hashes: Vec<String> = installed.iter().map(|p| p.hash.clone()).collect();

    match store.garbage_collect(&installed_hashes) {
        Ok(removed) => {
            if removed.is_empty() {
                println!("  No orphaned packages found");
            } else {
                println!("  Removed {} orphaned packages", removed.len());
                for hash in &removed {
                    println!("    - {}...", &hash[..16]);
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to garbage collect: {}", e);
        }
    }

    // 3. Show cache usage
    println!("\nCache statistics:");
    match downloader.get_cache_size() {
        Ok(size) => {
            let size_mb = size as f64 / 1024.0 / 1024.0;
            println!("  Cache size: {:.2} MB", size_mb);
        }
        Err(e) => {
            eprintln!("Failed to get cache size: {}", e);
        }
    }

    // 4. Clear cache if requested
    if all {
        print!("\nClear download cache? [y/N]: ");
        let _ = std::io::stdout().flush();
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_ok() {
            if ["yes", "y"].contains(&input.trim().to_lowercase().as_str()) {
                println!("Clearing cache...");
                match downloader.clear_cache() {
                    Ok(()) => println!("  Cache cleared"),
                    Err(e) => eprintln!("Failed to clear cache: {}", e),
                }
            }
        }
    }

    println!("\n\x1B[32mCleanup complete!\x1B[0m");
    PostAction::Return
}

