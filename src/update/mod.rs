use crate::database::Database;
use crate::download::DownloadManager;
use crate::repository::create_client_from_settings;
use crate::store::PackageStore;
use crate::symlinks::SymlinkManager;
use crate::verify::verify_package;
use crate::{Command, PostAction, StateBox};
use nix::unistd;
use settings::{get_settings_or_local};
use std::collections::HashMap;
use std::io::Write;

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "update",
        vec![String::from("upgrade")],
        "Update installed packages to latest versions",
        Vec::new(),
        None,
        run,
        hierarchy,
    )
}

fn run(_: &StateBox, _args: Option<&[String]>) -> PostAction {
    // check for root
    let euid = unistd::geteuid();
    if euid.as_raw() != 0 {
        return PostAction::Elevate;
    }

    // load settings - use local-only settings if endpoints.txt doesn't exist
    let settings = match get_settings_or_local() {
        Ok(s) => s,
        Err(_) => return PostAction::Return,
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

    let downloader = match DownloadManager::new() {
        Ok(d) => d,
        Err(e) => {
            println!("Failed to initialize download manager: {}", e);
            return PostAction::Return;
        }
    };

    // initialize repository client (if sources are configured)
    if settings.sources.is_empty() {
        println!("No repository sources configured. Cannot check for updates.");
        return PostAction::Return;
    }

    let repo_client = match create_client_from_settings(&settings) {
        Ok(c) => c,
        Err(e) => {
            println!("Failed to create repository client: {}", e);
            return PostAction::Return;
        }
    };

    let symlink_mgr = SymlinkManager::new(db.clone(), "/opt/pax/links");

    println!("Checking for updates...");

    // get installed packages
    let installed = match db.list_packages() {
        Ok(pkgs) => pkgs,
        Err(e) => {
            println!("Failed to list installed packages: {}", e);
            return PostAction::Return;
        }
    };

    let mut installed_map = HashMap::new();
    for pkg in &installed {
        installed_map.insert(pkg.name.clone(), pkg.version.clone());
    }

    // check for updates
    let updates = repo_client.check_updates(&installed_map);

    if updates.is_empty() {
        println!("\nAll packages are up to date!");
        return PostAction::Return;
    }

    println!("\nAvailable updates:");
    for (name, old_ver, new_ver) in &updates {
        println!("  {} {} -> {}", name, old_ver, new_ver);
    }

    // ask for confirmation
    print!("\nProceed with update? [Y/n]: ");
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        println!("Failed to read input");
        return PostAction::Return;
    }
    
    if ["no", "n"].contains(&input.trim().to_lowercase().as_str()) {
        println!("Update cancelled");
        return PostAction::Return;
    }

    // update each package
    for (pkg_name, _, new_version) in updates {
        if let Err(e) = update_package(
            &pkg_name,
            &new_version,
            &repo_client,
            &downloader,
            &store,
            &db,
            &symlink_mgr,
        ) {
            println!("Failed to update {}: {}", pkg_name, e);
            println!("Update aborted");
            return PostAction::Return;
        }
    }

    // update library cache
    println!("\nUpdating system library cache...");
    if let Err(e) = symlink_mgr.update_library_cache() {
        eprintln!("Warning: Failed to update library cache: {}", e);
    }

    println!("\n\x1B[32mUpdate complete!\x1B[0m");
    PostAction::Return
}

fn update_package(
    pkg_name: &str,
    new_version: &str,
    repo_client: &crate::repository::RepositoryClient,
    downloader: &DownloadManager,
    store: &PackageStore,
    db: &Database,
    symlink_mgr: &SymlinkManager,
) -> Result<(), String> {
    println!("\nUpdating {}...", pkg_name);

    // get old package info
    let old_pkg_info = db.get_package_info(pkg_name)
        .map_err(|e| format!("Failed to get package info: {}", e))?
        .ok_or_else(|| format!("Package not found: {}", pkg_name))?;

    let _pkg_id = db.get_package_id(pkg_name)
        .map_err(|e| format!("Failed to get package ID: {}", e))?
        .ok_or_else(|| format!("Package not found: {}", pkg_name))?;

    // search for new version
    let (source, entry) = repo_client.search_package(pkg_name)?
        .ok_or_else(|| format!("Package not found in repositories: {}", pkg_name))?;

    if entry.version != new_version {
        return Err(format!("Version mismatch: expected {}, found {}", new_version, entry.version));
    }

    // download new version
    let pkg_path = downloader.download_package(
        &entry.download_url,
        pkg_name,
        &entry.version,
    )?;

    let sig_path = downloader.download_signature(
        &entry.signature_url,
        pkg_name,
        &entry.version,
    )?;

    // verify
    println!("Verifying package...");
    let verify_result = verify_package(&pkg_path, &sig_path, &entry.hash)?;
    
    if !verify_result.is_valid() {
        return Err(format!("Verification failed: {}", verify_result.error_message()));
    }

    // extract new version
    println!("Extracting package...");
    let hash = entry.hash.clone();
    let files = store.extract_pax_package(&pkg_path, &hash)?;
    let size = store.get_package_size(&hash)?;

    // update database
    println!("Updating database...");
    
    // remove old package entry (cascade will remove files, deps, provides, symlinks)
    db.remove_package(pkg_name)
        .map_err(|e| format!("Failed to remove old package entry: {}", e))?;

    // add new package entry
    let new_pkg_id = db.insert_package(
        pkg_name,
        &entry.version,
        &entry.description,
        &source,
        &hash,
        size,
    ).map_err(|e| format!("Failed to insert package: {}", e))?;

    // add file entries
    for file in &files {
        db.add_file(new_pkg_id, file, "regular")
            .map_err(|e| format!("Failed to add file: {}", e))?;
    }

    // add dependencies
    for dep in &entry.dependencies {
        db.add_dependency(new_pkg_id, dep, None, "runtime")
            .map_err(|e| format!("Failed to add dependency: {}", e))?;
    }

    // add provides
    for provide in &entry.provides {
        db.add_provides(new_pkg_id, provide, None, "virtual")
            .map_err(|e| format!("Failed to add provide: {}", e))?;
    }

    // update symlinks
    println!("Updating symlinks...");
    symlink_mgr.update_symlinks(
        new_pkg_id,
        &hash,
        &store.get_package_path(&hash),
        &files,
    )?;

    // remove old version from store
    println!("Cleaning up old version...");
    store.remove_package(&old_pkg_info.hash)?;

    println!("  {} updated successfully", pkg_name);

    Ok(())
}

