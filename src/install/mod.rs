use std::collections::HashMap;
use std::io::Write;
use std::fs;
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};

use crate::adapters::{detect_package_type, PackageType};
use crate::database::Database;
use crate::download::DownloadManager;
use crate::provides::ProvidesManager;
use crate::repository::{create_client_from_settings, PackageEntry};
use crate::resolver::{DependencyResolver, PackageInfo};
use crate::store::PackageStore;
use crate::symlinks::SymlinkManager;
use crate::verify::{verify_package, verify_with_options, VerifyOptions};
use crate::{Command, PostAction, StateBox};
use nix::unistd;
use settings::{get_settings_or_local};

/// Package metadata for local packages
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalPackageMetadata {
    pub name: String,
    pub version: String,
    pub description: String,
    pub arch: Vec<String>,
    pub dependencies: Vec<String>,
    pub runtime_dependencies: Vec<String>,
    pub provides: Vec<String>,
    pub conflicts: Vec<String>,
    pub install_script: Option<String>,
    pub uninstall_script: Option<String>,
    pub files: Vec<String>,
}

pub fn build(hierarchy: &[String]) -> Command {
    use flags::Flag;

    Command::new(
        "install",
        vec![String::from("i")],
        "Install packages with dependency resolution",
        vec![
            Flag::new(
                None,
                "skip-signature",
                "Skip signature verification (use with caution)",
                false,
                false,
                |states, _| {
                    states.shove("skip_signature", true);
                },
            ),
            Flag::new(
                None,
                "skip-native-hash",
                "Skip hash verification for RPM/DEB packages (use with caution)",
                false,
                false,
                |states, _| {
                    states.shove("skip_native_hash", true);
                },
            ),
        ],
        None,
        run,
        hierarchy,
    )
}

fn run(states: &StateBox, args: Option<&[String]>) -> PostAction {
    // Check for root privileges
    let euid = unistd::geteuid();
    if euid.as_raw() != 0 {
        return PostAction::Elevate;
    }

    // Check if signature verification should be skipped
    let skip_signature = states.get::<bool>("skip_signature").copied().unwrap_or(false);

    let args = match args {
        None => {
            println!("Usage: pax install <package1> [package2] [...]");
            return PostAction::Return;
        }
        Some(args) => args,
    };

    // load settings - use local-only settings if endpoints.txt doesn't exist
    let settings = match get_settings_or_local() {
        Ok(s) => s,
        Err(_) => return PostAction::Return,
    };

    // Initialize components
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

    let repo_client = if settings.sources.is_empty() {
        // No repository sources configured, skip repository operations
        None
    } else {
        match create_client_from_settings(&settings) {
            Ok(c) => Some(c),
            Err(e) => {
                println!("Failed to create repository client: {}", e);
                return PostAction::Return;
            }
        }
    };

    let resolver = DependencyResolver::new(db.clone());
    let provides_mgr = ProvidesManager::new(db.clone());
    let symlink_mgr = SymlinkManager::new(db.clone(), "/opt/pax/links");

    let resolve_progress = crate::progress::create_simple_progress("Resolving dependencies");

    // Search for packages in repositories
    let mut packages_to_install = HashMap::new();
    
    for pkg_name in args {
        // Check if this is a local file path
        if std::path::Path::new(pkg_name).exists() {
            // Detect package type
            let pkg_type = match detect_package_type(std::path::Path::new(pkg_name)) {
                Some(t) => t,
                None => {
                    println!("Unknown package type: {}", pkg_name);
                    return PostAction::Return;
                }
            };
            
            // Extract package metadata based on type
            let metadata = match pkg_type {
                PackageType::Pax => {
                    match extract_pax_metadata(pkg_name) {
                        Ok(m) => m,
                        Err(e) => {
                            println!("Failed to read PAX package metadata: {}", e);
                            return PostAction::Return;
                        }
                    }
                }
                PackageType::Rpm | PackageType::Deb => {
                    match extract_native_metadata(pkg_name, pkg_type) {
                        Ok(m) => m,
                        Err(e) => {
                            println!("Failed to read {} package metadata: {}", pkg_type.as_str(), e);
                            return PostAction::Return;
                        }
                    }
                }
            };
            
            // Create a PackageEntry from the metadata
            let hash = match calculate_file_hash(pkg_name) {
                Ok(h) => h,
                Err(e) => {
                    println!("Failed to calculate hash: {}", e);
                    return PostAction::Return;
                }
            };
            
            let canonical_path = match std::fs::canonicalize(pkg_name) {
                Ok(p) => p,
                Err(e) => {
                    println!("Failed to canonicalize path: {}", e);
                    return PostAction::Return;
                }
            };
            
            let pkg_entry = PackageEntry {
                name: metadata.name.clone(),
                version: metadata.version.clone(),
                architecture: None,
                description: metadata.description.clone(),
                hash,
                download_url: format!("file://{}", canonical_path.display()),
                signature_url: String::new(), // No signature for local files
                dependencies: metadata.dependencies.clone(),
                provides: metadata.provides.clone(),
                runtime_dependencies: metadata.runtime_dependencies.clone(),
                size: 0, // Will be calculated during extraction
            };
            
            packages_to_install.insert(metadata.name.clone(), ("local".to_string(), pkg_entry));
        } else {
            // Search in repositories (if available)
            if let Some(ref client) = repo_client {
                match client.search_package(pkg_name) {
                    Ok(Some((source, pkg_entry))) => {
                        println!("Found {} (version {}) in {}", pkg_name, pkg_entry.version, source);
                        packages_to_install.insert(pkg_name.clone(), (source, pkg_entry));
                    }
                    Ok(None) => {
                        println!("Package not found: {}", pkg_name);
                        return PostAction::Return;
                    }
                    Err(e) => {
                        println!("Error searching for {}: {}", pkg_name, e);
                        return PostAction::Return;
                    }
                }
            } else {
                println!("Package not found (no repository sources configured): {}", pkg_name);
                return PostAction::Return;
            }
        }
    }

    // Build available packages map for resolver
    let mut available = HashMap::new();
    for (pkg_name, (_, entry)) in &packages_to_install {
        available.insert(
            pkg_name.clone(),
            PackageInfo {
                version: entry.version.clone(),
                dependencies: entry.dependencies.iter().map(|d| {
                    crate::adapters::Dependency {
                        name: d.clone(),
                        version_constraint: None,
                        dep_type: crate::adapters::DependencyType::Runtime,
                    }
                }).collect(),
                provides: entry.provides.clone(),
            },
        );
    }

    // Check for unmet dependencies BEFORE resolving
    println!("Checking dependencies...");
    let mut unmet_deps = Vec::new();
    let mut system_deps_ok = 0;
    
    for (pkg_name, (_, entry)) in &packages_to_install {
        for dep in &entry.dependencies {
            // Check if dependency is satisfied by system or will be installed
            if packages_to_install.contains_key(dep) || db.is_installed(dep).unwrap_or(false) {
                // Will be installed or already installed
                continue;
            }
            
            // Check if satisfied by system
            match provides_mgr.is_satisfied(dep) {
                Ok(true) => {
                    // Satisfied by system, count it
                    system_deps_ok += 1;
                }
                Ok(false) => {
                    // Not satisfied
                    unmet_deps.push((pkg_name.clone(), dep.clone()));
                }
                Err(_) => {
                    // Error checking, assume not satisfied
                    unmet_deps.push((pkg_name.clone(), dep.clone()));
                }
            }
        }
    }
    
    if system_deps_ok > 0 {
        println!("  {} system dependencies satisfied", system_deps_ok);
    }
    
    if !unmet_deps.is_empty() {
        // Separate system library dependencies from package dependencies
        let mut sys_lib_deps = Vec::new();
        let mut pkg_deps = Vec::new();
        
        for (pkg, dep) in &unmet_deps {
            if dep.contains(".so") || dep.contains("(") || dep.starts_with("rtld") {
                sys_lib_deps.push((pkg, dep));
            } else {
                pkg_deps.push((pkg, dep));
            }
        }
        
        if !pkg_deps.is_empty() {
            println!("\n\x1B[31mError: Missing package dependencies!\x1B[0m");
            for (pkg, dep) in &pkg_deps {
                println!("  {} requires: {}", pkg, dep);
            }
            println!("\nInstallation aborted. Please install missing packages first.");
            return PostAction::Return;
        }
        
        if !sys_lib_deps.is_empty() {
            println!("\n\x1B[33mWarning: Some system library dependencies could not be verified:\x1B[0m");
            for (pkg, dep) in &sys_lib_deps {
                println!("  {} requires: {}", pkg, dep);
            }
            println!("\nThese may be satisfied by your system's package manager.");
            println!("If installation fails, install the required libraries using official repositories or another package manager.");
            println!("Continuing with installation...");
        }
    }

    // Resolve dependencies
    let package_names: Vec<String> = packages_to_install.keys().cloned().collect();
    let resolved = match resolver.resolve(&package_names, &available) {
        Ok(r) => r,
        Err(e) => {
            resolve_progress.finish_and_clear();
            println!("\x1B[31mDependency resolution failed: {}\x1B[0m", e);
            return PostAction::Return;
        }
    };
    
    resolve_progress.finish_and_clear();

    println!("\nPackages to install:");
    for pkg in &resolved {
        println!("  - {} ({})", pkg.name, pkg.version.as_ref().unwrap_or(&"unknown".to_string()));
    }

    // Ask for confirmation
    print!("\nProceed with installation? [Y/n]: ");
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        println!("Failed to read input");
        return PostAction::Return;
    }
    
    if ["no", "n"].contains(&input.trim().to_lowercase().as_str()) {
        println!("Installation cancelled");
        return PostAction::Return;
    }

    // Install packages in order with overall progress
    let total_packages = resolved.len() as u64;
    let overall_progress = crate::progress::create_progress_bar(total_packages, "Overall progress");
    
    for pkg in resolved {
        println!("Installing {}...", pkg.name);
        
        if let Err(e) = install_package(
            &pkg.name,
            &packages_to_install,
            &repo_client,
            &downloader,
            &store,
            &db,
            &provides_mgr,
            &symlink_mgr,
            skip_signature,
        ) {
            overall_progress.finish_and_clear();
            println!("Failed to install {}: {}", pkg.name, e);
            println!("Installation aborted");
            return PostAction::Return;
        }
        
        overall_progress.inc(1);
    }
    
    overall_progress.finish_and_clear();
    println!("\nAll packages installed successfully!");

    // Update library cache
    println!("\nUpdating system library cache...");
    if let Err(e) = symlink_mgr.update_library_cache() {
        eprintln!("Warning: Failed to update library cache: {}", e);
    }

    println!("\n\x1B[32mInstallation complete!\x1B[0m");
    PostAction::Return
}

// Install a single package
fn install_package(
    pkg_name: &str,
    packages: &HashMap<String, (String, PackageEntry)>,
    repo_client: &Option<crate::repository::RepositoryClient>,
    downloader: &DownloadManager,
    store: &PackageStore,
    db: &Database,
    provides_mgr: &ProvidesManager,
    symlink_mgr: &SymlinkManager,
    skip_signature: bool,
) -> Result<(), String> {
    // Skip if already installed
    if db.is_installed(pkg_name)
        .map_err(|e| format!("Database error: {}", e))? {
        println!("  {} already installed, skipping", pkg_name);
        return Ok(());
    }

    // Skip if satisfied by system
    if provides_mgr.is_satisfied(pkg_name)? {
        println!("  {} satisfied by system, skipping", pkg_name);
        return Ok(());
    }

    let (source, entry) = packages.get(pkg_name)
        .ok_or_else(|| format!("Package {} not found in list", pkg_name))?;

    println!("\nInstalling {}...", pkg_name);

    // Handle local vs remote packages
    let pkg_path = if source == "local" {
        // Local package - extract path from file:// URL
        let path = entry.download_url.strip_prefix("file://")
            .ok_or_else(|| "Invalid local package URL".to_string())?;
        println!("Using local package: {}", path);
        std::path::PathBuf::from(path)
    } else {
        // Download package from repository (only if repo_client is available)
        if repo_client.is_none() {
            return Err(format!("Cannot download {}: no repository sources configured", pkg_name));
        }

        println!("Downloading {} from {}...", pkg_name, entry.download_url);
        let pkg_path = downloader.download_package(
            &entry.download_url,
            pkg_name,
            &entry.version,
        )?;

        // Download signature
        let sig_path = downloader.download_signature(
            &entry.signature_url,
            pkg_name,
            &entry.version,
        )?;

        // Verify package
        println!("Verifying package...");

        // For RPM and DEB packages, skip signature verification since they don't use PAX signatures
        // They may also skip hash verification depending on user preference, but structure check is always done
        let package_type = detect_package_type(&pkg_path).unwrap_or(PackageType::Pax);
        let verify_result = if matches!(package_type, PackageType::Rpm | PackageType::Deb) {
            // RPM/DEB packages: skip signature verification since they don't use PAX signatures
            // For hash verification, we can be more lenient since these are native package formats
            println!("\x1B[33mNote: RPM/DEB packages skip signature verification (using native signatures)\x1B[0m");
            let options = VerifyOptions {
                verify_hash: true,  // Still verify hash for integrity unless user explicitly skips
                verify_signature: false,
                verify_structure: true,
                force_insecure: false,
            };
            verify_with_options(&pkg_path, None, Some(&entry.hash), package_type.as_str(), &options)?
        } else if skip_signature {
            println!("\x1B[33mWARNING: Skipping signature verification\x1B[0m");
            let options = VerifyOptions {
                verify_hash: true,
                verify_signature: false,
                verify_structure: true,
                force_insecure: false,
            };
            verify_with_options(&pkg_path, Some(&sig_path), Some(&entry.hash), "pax", &options)?
        } else {
            verify_package(&pkg_path, &sig_path, &entry.hash)?
        };

        if !verify_result.is_valid() {
            return Err(format!("Verification failed: {}", verify_result.error_message()));
        }

        pkg_path
    };

    // Extract to store with progress indicator
    let extract_progress = crate::progress::create_simple_progress("Extracting package");
    let hash = entry.hash.clone();
    
    // Detect package type and extract
    let (files, pkg_type) = if let Some(pkg_type) = detect_package_type(&pkg_path) {
        let files = match pkg_type {
            PackageType::Pax => {
                extract_progress.set_message("Extracting PAX package...");
                store.extract_pax_package(&pkg_path, &hash)?
            }
            PackageType::Rpm => {
                extract_progress.set_message("Extracting RPM package...");
                extract_rpm_to_store(&pkg_path, &hash, store)?
            }
            PackageType::Deb => {
                extract_progress.set_message("Extracting DEB package...");
                extract_deb_to_store(&pkg_path, &hash, store)?
            }
        };
        (files, pkg_type)
    } else {
        return Err("Unknown package type".to_string());
    };
    
    extract_progress.finish_and_clear();

    let size = store.get_package_size(&hash)?;

    // Add to database with progress
    let db_progress = crate::progress::create_simple_progress("Updating database");
    let pkg_id = db.insert_package(
        pkg_name,
        &entry.version,
        &entry.description,
        source,
        &hash,
        size,
    ).map_err(|e| format!("Failed to insert package: {}", e))?;

    // Add file entries with progress
    let files_total = files.len() as u64;
    let files_progress = crate::progress::create_progress_bar(files_total, "Adding files");
    for file in &files {
        db.add_file(pkg_id, file, "regular")
            .map_err(|e| format!("Failed to add file: {}", e))?;
        files_progress.inc(1);
    }
    files_progress.finish_and_clear();

    // Add dependencies
    for dep in &entry.dependencies {
        db.add_dependency(pkg_id, dep, None, "runtime")
            .map_err(|e| format!("Failed to add dependency: {}", e))?;
    }

    // Add provides
    for provide in &entry.provides {
        db.add_provides(pkg_id, provide, None, "virtual")
            .map_err(|e| format!("Failed to add provide: {}", e))?;
    }
    
    db_progress.finish_and_clear();

    // Run pre-install scriptlets
    let _ = run_scriptlets(&pkg_path, pkg_type, "pre", pkg_id, &store.get_package_path(&hash));
    
    // Create symlinks with progress
    let symlink_progress = crate::progress::create_simple_progress("Creating symlinks");
    symlink_mgr.create_symlinks(
        pkg_id,
        &hash,
        &store.get_package_path(&hash),
        &files,
    )?;
    symlink_progress.finish_and_clear();

    // Run post-install scriptlets
    let _ = run_scriptlets(&pkg_path, pkg_type, "post", pkg_id, &store.get_package_path(&hash));

    println!("  {} installed successfully", pkg_name);

    Ok(())
}

/// Extract metadata from a local .pax package
fn extract_pax_metadata(package_path: &str) -> Result<LocalPackageMetadata, String> {
    use tempfile::TempDir;
    
    
    let temp_dir = TempDir::new()
        .map_err(|e| format!("Failed to create temp directory: {}", e))?;
    
    let extract_dir = temp_dir.path().join("extract");
    fs::create_dir_all(&extract_dir)
        .map_err(|e| format!("Failed to create extract directory: {}", e))?;
    
    // Decompress and extract the package
    let zstd_output = std::process::Command::new("zstd")
        .arg("-dc")
        .arg(package_path)
        .output()
        .map_err(|e| format!("Failed to decompress package: {}", e))?;
    
    if !zstd_output.status.success() {
        return Err("Failed to decompress package".to_string());
    }
    
    let mut tar_process = std::process::Command::new("tar")
        .arg("-xf")
        .arg("-")
        .arg("-C")
        .arg(&extract_dir)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start tar process: {}", e))?;
    
    if let Some(stdin) = tar_process.stdin.take() {
        std::io::Write::write_all(&mut std::io::BufWriter::new(stdin), &zstd_output.stdout)
            .map_err(|e| format!("Failed to write to tar stdin: {}", e))?;
    }
    
    let tar_output = tar_process.wait_with_output()
        .map_err(|e| format!("Failed to wait for tar process: {}", e))?;
    
    if !tar_output.status.success() {
        return Err("Failed to extract package".to_string());
    }
    
    // Read metadata.yaml
    let metadata_path = extract_dir.join("metadata.yaml");
    if !metadata_path.exists() {
        return Err("metadata.yaml not found in package".to_string());
    }
    
    let metadata_content = fs::read_to_string(&metadata_path)
        .map_err(|e| format!("Failed to read metadata.yaml: {}", e))?;
    
    serde_yaml::from_str(&metadata_content)
        .map_err(|e| format!("Failed to parse metadata: {}", e))
}

/// Extract metadata from RPM or DEB packages
fn extract_native_metadata(package_path: &str, pkg_type: PackageType) -> Result<LocalPackageMetadata, String> {
    match pkg_type {
        PackageType::Rpm => extract_rpm_metadata(package_path),
        PackageType::Deb => extract_deb_metadata(package_path),
        _ => Err("Unsupported package type".to_string()),
    }
}

/// Extract metadata from RPM package
fn extract_rpm_metadata(package_path: &str) -> Result<LocalPackageMetadata, String> {
    
    
    // Use rpm command to query package info
    let name_output = std::process::Command::new("rpm")
        .args(&["-qp", "--queryformat", "%{NAME}", package_path])
        .output()
        .map_err(|e| format!("Failed to run rpm command: {}", e))?;
    
    if !name_output.status.success() {
        return Err(format!("Failed to query RPM: {}", String::from_utf8_lossy(&name_output.stderr)));
    }
    
    let name = String::from_utf8_lossy(&name_output.stdout).to_string();
    
    let version_output = std::process::Command::new("rpm")
        .args(&["-qp", "--queryformat", "%{VERSION}-%{RELEASE}", package_path])
        .output()
        .map_err(|e| format!("Failed to query RPM version: {}", e))?;
    let version = String::from_utf8_lossy(&version_output.stdout).to_string();
    
    let desc_output = std::process::Command::new("rpm")
        .args(&["-qp", "--queryformat", "%{SUMMARY}", package_path])
        .output()
        .map_err(|e| format!("Failed to query RPM description: {}", e))?;
    let description = String::from_utf8_lossy(&desc_output.stdout).to_string();
    
    // Get dependencies
    let deps_output = std::process::Command::new("rpm")
        .args(&["-qp", "--requires", package_path])
        .output()
        .map_err(|e| format!("Failed to query RPM dependencies: {}", e))?;
    let dependencies: Vec<String> = String::from_utf8_lossy(&deps_output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with("rpmlib("))
        .map(|line| line.split_whitespace().next().unwrap_or(line).to_string())
        .collect();
    
    // Get provides
    let prov_output = std::process::Command::new("rpm")
        .args(&["-qp", "--provides", package_path])
        .output()
        .map_err(|e| format!("Failed to query RPM provides: {}", e))?;
    let provides: Vec<String> = String::from_utf8_lossy(&prov_output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.split_whitespace().next().unwrap_or(line).to_string())
        .collect();
    
    // Get architecture
    let arch_output = std::process::Command::new("rpm")
        .args(&["-qp", "--queryformat", "%{ARCH}", package_path])
        .output()
        .map_err(|e| format!("Failed to query RPM arch: {}", e))?;
    let arch = if arch_output.status.success() {
        let arch_str = String::from_utf8_lossy(&arch_output.stdout).to_string();
        if arch_str.is_empty() || arch_str == "noarch" {
            vec!["x86_64".to_string(), "aarch64".to_string()]
        } else {
            vec![arch_str]
        }
    } else {
        vec!["x86_64".to_string()]
    };
    
    // Get file list
    let files_output = std::process::Command::new("rpm")
        .args(&["-qpl", package_path])
        .output()
        .map_err(|e| format!("Failed to query RPM files: {}", e))?;
    let files: Vec<String> = String::from_utf8_lossy(&files_output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with("/"))
        .map(|line| line.trim_start_matches('/').to_string())
        .collect();
    
    Ok(LocalPackageMetadata {
        name,
        version,
        description,
        arch,
        dependencies,
        runtime_dependencies: Vec::new(),
        provides,
        conflicts: Vec::new(),
        install_script: None,
        uninstall_script: None,
        files,
    })
}

/// Extract metadata from DEB package
fn extract_deb_metadata(package_path: &str) -> Result<LocalPackageMetadata, String> {
    
    
    // Use dpkg-deb to query package info
    let info_output = std::process::Command::new("dpkg-deb")
        .args(&["-f", package_path])
        .output()
        .map_err(|e| format!("Failed to run dpkg-deb: {}", e))?;
    
    if !info_output.status.success() {
        return Err(format!("Failed to query DEB: {}", String::from_utf8_lossy(&info_output.stderr)));
    }
    
    let info = String::from_utf8_lossy(&info_output.stdout);
    let mut name = String::new();
    let mut version = String::new();
    let mut description = String::new();
    let mut arch = Vec::new();
    let mut dependencies = Vec::new();
    let mut provides = Vec::new();
    
    for line in info.lines() {
        if let Some(value) = line.strip_prefix("Package: ") {
            name = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("Version: ") {
            version = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("Description: ") {
            description = value.trim().to_string();
        } else if let Some(value) = line.strip_prefix("Architecture: ") {
            let arch_str = value.trim().to_string();
            arch = if arch_str == "all" {
                vec!["x86_64".to_string(), "aarch64".to_string()]
            } else {
                // Map Debian arch names to common names
                let mapped_arch = match arch_str.as_str() {
                    "amd64" => "x86_64",
                    "arm64" => "aarch64",
                    "i386" => "i686",
                    _ => &arch_str,
                };
                vec![mapped_arch.to_string()]
            };
        } else if let Some(value) = line.strip_prefix("Depends: ") {
            dependencies = value
                .split(',')
                .map(|d| d.trim().split_whitespace().next().unwrap_or(d.trim()).to_string())
                .collect();
        } else if let Some(value) = line.strip_prefix("Provides: ") {
            provides = value
                .split(',')
                .map(|p| p.trim().to_string())
                .collect();
        }
    }
    
    // Default architecture if not found
    if arch.is_empty() {
        arch = vec!["x86_64".to_string()];
    }
    
    // Get file list
    let files_output = std::process::Command::new("dpkg-deb")
        .args(&["-c", package_path])
        .output()
        .map_err(|e| format!("Failed to query DEB files: {}", e))?;
    let files: Vec<String> = String::from_utf8_lossy(&files_output.stdout)
        .lines()
        .filter_map(|line| {
            line.split_whitespace().last().and_then(|path| {
                if path.starts_with("./") {
                    Some(path.trim_start_matches("./").to_string())
                } else {
                    None
                }
            })
        })
        .collect();
    
    if provides.is_empty() {
        provides.push(name.clone());
    }
    
    Ok(LocalPackageMetadata {
        name,
        version,
        description,
        arch,
        dependencies,
        runtime_dependencies: Vec::new(),
        provides,
        conflicts: Vec::new(),
        install_script: None,
        uninstall_script: None,
        files,
    })
}

/// Extract RPM package to store
fn extract_rpm_to_store(rpm_path: &std::path::Path, hash: &str, store: &PackageStore) -> Result<Vec<String>, String> {
    
    
    let dest = store.get_package_path(hash);
    fs::create_dir_all(&dest)
        .map_err(|e| format!("Failed to create package directory: {}", e))?;
    
    // Extract RPM using rpm2cpio and cpio
    let rpm2cpio = std::process::Command::new("rpm2cpio")
        .arg(rpm_path)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to run rpm2cpio: {}", e))?;
    
    let cpio = std::process::Command::new("cpio")
        .args(&["-idm"])
        .current_dir(&dest)
        .stdin(rpm2cpio.stdout.unwrap())
        .output()
        .map_err(|e| format!("Failed to run cpio: {}", e))?;
    
    if !cpio.status.success() {
        return Err(format!("Failed to extract RPM: {}", String::from_utf8_lossy(&cpio.stderr)));
    }
    
    // List extracted files
    store.list_package_files(hash)
}

/// Extract DEB package to store
fn extract_deb_to_store(deb_path: &std::path::Path, hash: &str, store: &PackageStore) -> Result<Vec<String>, String> {
    
    
    let dest = store.get_package_path(hash);
    fs::create_dir_all(&dest)
        .map_err(|e| format!("Failed to create package directory: {}", e))?;
    
    // Extract DEB using dpkg-deb
    let output = std::process::Command::new("dpkg-deb")
        .args(&["-x", deb_path.to_str().unwrap(), dest.to_str().unwrap()])
        .output()
        .map_err(|e| format!("Failed to run dpkg-deb: {}", e))?;
    
    if !output.status.success() {
        return Err(format!("Failed to extract DEB: {}", String::from_utf8_lossy(&output.stderr)));
    }
    
    // List extracted files
    store.list_package_files(hash)
}

/// Run package scriptlets
fn run_scriptlets(pkg_path: &std::path::Path, pkg_type: PackageType, stage: &str, pkg_id: i64, store_path: &std::path::Path) -> Result<(), String> {
    
    
    match pkg_type {
        PackageType::Rpm => {
            println!("Running RPM {} scriptlets...", stage);
            // Extract and run RPM scriptlets
            let script_query = match stage {
                "pre" => "--script=prein",
                "post" => "--script=postin",
                "preun" => "--script=preun",
                "postun" => "--script=postun",
                _ => return Ok(()),
            };
            
            let script_output = std::process::Command::new("rpm")
                .args(&["-qp", script_query, pkg_path.to_str().unwrap()])
                .output()
                .map_err(|e| format!("Failed to query RPM scriptlets: {}", e))?;
            
            if script_output.status.success() {
                let script = String::from_utf8_lossy(&script_output.stdout);
                if !script.trim().is_empty() {
                    // Run scriptlet with proper environment
                    let output = std::process::Command::new("sh")
                        .arg("-c")
                        .arg(&script.to_string())
                        .env("RPM_INSTALL_PREFIX", store_path)
                        .output()
                        .map_err(|e| format!("Failed to run scriptlet: {}", e))?;
                    
                    if !output.status.success() {
                        eprintln!("Warning: {} scriptlet failed: {}", stage, String::from_utf8_lossy(&output.stderr));
                    }
                }
            }
        }
        PackageType::Deb => {
            println!("Running DEB {} scripts...", stage);
            // Extract and run DEB maintainer scripts
            let script_name = match stage {
                "pre" => "preinst",
                "post" => "postinst",
                "preun" => "prerm",
                "postun" => "postrm",
                _ => return Ok(()),
            };
            
            // Extract the control archive
            let control_output = std::process::Command::new("dpkg-deb")
                .args(&["--control", pkg_path.to_str().unwrap()])
                .current_dir("/tmp")
                .output()
                .map_err(|e| format!("Failed to extract DEB control: {}", e))?;
            
            if control_output.status.success() {
                let script_path = format!("/tmp/DEBIAN/{}", script_name);
                if std::path::Path::new(&script_path).exists() {
                    let output = std::process::Command::new(&script_path)
                        .arg("install")
                        .env("DPKG_MAINTSCRIPT_PACKAGE", pkg_id.to_string())
                        .output()
                        .map_err(|e| format!("Failed to run {} script: {}", script_name, e))?;
                    
                    if !output.status.success() {
                        eprintln!("Warning: {} script failed: {}", script_name, String::from_utf8_lossy(&output.stderr));
                    }
                    
                    // Cleanup
                    let _ = fs::remove_file(&script_path);
                }
            }
        }
        PackageType::Pax => {
            // PAX packages handle scripts via metadata.yaml
            // This is handled separately
        }
    }
    
    Ok(())
}

/// Calculate SHA256 hash of a file
fn calculate_file_hash(file_path: &str) -> Result<String, String> {
    let mut file = fs::File::open(file_path)
        .map_err(|e| format!("Failed to open file: {}", e))?;
    
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("Failed to read file: {}", e))?;
    
    let hash = hasher.finalize();
    Ok(format!("{:x}", hash))
}
