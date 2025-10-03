use crate::{Command, PostAction, StateBox};
use crate::database::Database;
use crate::store::PackageStore;
use crate::symlinks::SymlinkManager;
use crate::crypto::calculate_sha256;
use serde::{Deserialize, Serialize};
use std::fs;
use std::process::Command as ProcessCommand;
use nix::unistd;

// Build recipe format (.paxmeta)
#[derive(Debug, Clone, Serialize, Deserialize)]
struct BuildRecipe {
    name: String,
    version: String,
    description: String,
    source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    #[serde(default = "default_arch")]
    arch: Vec<String>,
    #[serde(default)]
    dependencies: Vec<String>,
    #[serde(default)]
    runtime_dependencies: Vec<String>,
    #[serde(default)]
    provides: Vec<String>,
    #[serde(default)]
    conflicts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    build: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    install: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    uninstall: Option<String>,
}

fn default_arch() -> Vec<String> {
    vec!["x86_64".to_string(), "aarch64".to_string()]
}

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "compile",
        Vec::new(),
        "Build and install package from source",
        Vec::new(),
        None,
        run,
        hierarchy,
    )
}

fn run(_: &StateBox, args: Option<&[String]>) -> PostAction {
    // Check for root privileges
    let euid = unistd::geteuid();
    if euid.as_raw() != 0 {
        return PostAction::Elevate;
    }

    let args = match args {
        None => {
            println!("Usage: pax compile <url|path-to-paxmeta>");
            println!("\nExamples:");
            println!("  pax compile https://github.com/user/project");
            println!("  pax compile ./custom-package.paxmeta");
            println!("  pax compile https://example.com/package.paxmeta");
            return PostAction::Return;
        }
        Some(args) => args,
    };

    if args.is_empty() {
        println!("Error: No source specified");
        return PostAction::Return;
    }

    let source = &args[0];
    
    println!("PAX Compile - Build from source");
    println!("Source: {}", source);
    println!();

    // Determine what we're compiling
    let recipe = if source.ends_with(".paxmeta") {
        // Direct .paxmeta file
        load_recipe_from_file(source)
    } else if source.starts_with("http://") || source.starts_with("https://") {
        if source.ends_with(".paxmeta") {
            // Download .paxmeta from URL
            download_and_load_recipe(source)
        } else {
            // Assume GitHub repo, try to find .paxmeta or auto-detect
            compile_from_github(source)
        }
    } else {
        println!("Error: Invalid source. Must be:");
        println!("  - Path to .paxmeta file");
        println!("  - URL to .paxmeta file");
        println!("  - GitHub repository URL");
        return PostAction::Return;
    };

    let recipe = match recipe {
        Ok(r) => r,
        Err(e) => {
            println!("Error loading build recipe: {}", e);
            return PostAction::Return;
        }
    };

    // Build and install
    match build_and_install(&recipe) {
        Ok(()) => {
            println!("\n{} compiled and installed successfully", recipe.name);
            PostAction::Return
        }
        Err(e) => {
            println!("\nBuild failed: {}", e);
            PostAction::Return
        }
    }
}

fn load_recipe_from_file(path: &str) -> Result<BuildRecipe, String> {
    println!("Loading build recipe from {}...", path);
    
    let contents = fs::read_to_string(path)
        .map_err(|e| format!("Failed to read file: {}", e))?;
    
    let recipe: BuildRecipe = serde_json::from_str(&contents)
        .map_err(|e| format!("Failed to parse .paxmeta: {}", e))?;
    
    Ok(recipe)
}

fn download_and_load_recipe(url: &str) -> Result<BuildRecipe, String> {
    println!("Downloading build recipe from {}...", url);
    
    let response = reqwest::blocking::get(url)
        .map_err(|e| format!("Failed to download: {}", e))?;
    
    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()));
    }
    
    let contents = response.text()
        .map_err(|e| format!("Failed to read response: {}", e))?;
    
    let recipe: BuildRecipe = serde_json::from_str(&contents)
        .map_err(|e| format!("Failed to parse .paxmeta: {}", e))?;
    
    Ok(recipe)
}

fn compile_from_github(url: &str) -> Result<BuildRecipe, String> {
    println!("Detecting project from {}...", url);
    
    // Try to find .paxmeta in repo
    let raw_url = convert_to_raw_github_url(url);
    
    if let Ok(recipe) = download_and_load_recipe(&format!("{}/.paxmeta", raw_url)) {
        return Ok(recipe);
    }
    
    println!("No .paxmeta found, auto-detecting build system...");
    
    // Auto-detect build system and generate recipe
    auto_detect_and_generate_recipe(url)
}

fn convert_to_raw_github_url(github_url: &str) -> String {
    github_url
        .replace("github.com", "raw.githubusercontent.com")
        .replace("/tree/", "/")
        + "/main"
}

fn auto_detect_and_generate_recipe(url: &str) -> Result<BuildRecipe, String> {
    // Extract repo name from URL
    let repo_name = url
        .trim_end_matches('/')
        .split('/')
        .last()
        .unwrap_or("unknown")
        .to_string();
    
    println!("Auto-generating recipe for {}...", repo_name);
    println!("Warning: Auto-detection is experimental");
    
    // Create a basic recipe
    // In a real implementation, this would clone and inspect the repo
    let recipe = BuildRecipe {
        name: repo_name.clone(),
        version: "git-main".to_string(),
        description: format!("Built from {}", url),
        source: format!("{}/archive/refs/heads/main.tar.gz", url),
        hash: None,
        arch: default_arch(),
        dependencies: vec![],
        runtime_dependencies: vec![],
        provides: vec![repo_name.clone()],
        conflicts: vec![],
        build: Some("make && make install DESTDIR=$PAX_BUILD_ROOT".to_string()),
        install: None,
        uninstall: None,
    };
    
    println!("Generated recipe:");
    println!("  Name: {}", recipe.name);
    println!("  Version: {}", recipe.version);
    println!("  Source: {}", recipe.source);
    
    Ok(recipe)
}

fn build_and_install(recipe: &BuildRecipe) -> Result<(), String> {
    println!("\n=== Building {} {} ===\n", recipe.name, recipe.version);
    
    // Ensure PAX directories exist
    fs::create_dir_all("/opt/pax/db")
        .map_err(|e| format!("Failed to create database directory: {}", e))?;
    fs::create_dir_all("/opt/pax/store")
        .map_err(|e| format!("Failed to create store directory: {}", e))?;
    fs::create_dir_all("/opt/pax/links")
        .map_err(|e| format!("Failed to create links directory: {}", e))?;
    
    // Initialize components
    let db = Database::open("/opt/pax/db/pax.db")
        .map_err(|e| format!("Failed to open database: {}", e))?;
    
    let store = PackageStore::new()
        .map_err(|e| format!("Failed to initialize store: {}", e))?;
    
    let symlink_mgr = SymlinkManager::new(db.clone(), "/opt/pax/links");
    
    // Create temporary build directory
    let build_dir = format!("/tmp/pax-build-{}-{}", recipe.name, std::process::id());
    fs::create_dir_all(&build_dir)
        .map_err(|e| format!("Failed to create build dir: {}", e))?;
    
    // Download source
    println!("1. Downloading source from {}...", recipe.source);
    let source_tarball = download_source(&recipe.source, &build_dir)?;
    
    // Calculate hash if not provided
    let hash = if let Some(provided_hash) = &recipe.hash {
        println!("2. Verifying hash...");
        let calculated = calculate_sha256(&source_tarball)
            .map_err(|e| format!("Failed to calculate hash: {}", e))?;
        
        let provided_hash_clean = provided_hash.replace("sha256:", "");
        if calculated != provided_hash_clean {
            return Err(format!("Hash mismatch! Expected {}, got {}", provided_hash_clean, calculated));
        }
        println!("   Hash verified");
        calculated
    } else {
        println!("2. Calculating hash...");
        let calculated = calculate_sha256(&source_tarball)
            .map_err(|e| format!("Failed to calculate hash: {}", e))?;
        println!("   SHA256: {}", calculated);
        calculated
    };
    
    // Extract source
    println!("3. Extracting source...");
    extract_tarball(&source_tarball, &build_dir)?;
    
    // Find extracted directory (usually has package name)
    let extract_dir = find_extracted_dir(&build_dir)?;
    
    // Build
    println!("4. Building...");
    let build_root = format!("{}/install", build_dir);
    fs::create_dir_all(&build_root)
        .map_err(|e| format!("Failed to create install dir: {}", e))?;
    
    let build_cmd = recipe.build.clone().unwrap_or_else(|| {
        "./configure --prefix=/usr && make -j$(nproc) && make install DESTDIR=$PAX_BUILD_ROOT".to_string()
    });
    
    run_build_command(&build_cmd, &extract_dir, &build_root, recipe)?;
    
    // Install to store
    println!("5. Installing to package store...");
    let final_hash = hash[..16].to_string();
    store.copy_directory(&build_root, &final_hash)?;
    
    // Add to database
    println!("6. Updating database...");
    let size = store.get_package_size(&final_hash)?;
    let pkg_id = db.insert_package(
        &recipe.name,
        &recipe.version,
        &recipe.description,
        "compiled",
        &final_hash,
        size,
    ).map_err(|e| format!("Failed to insert package: {}", e))?;
    
    // Add dependencies
    for dep in &recipe.dependencies {
        db.add_dependency(pkg_id, dep, None, "runtime")
            .map_err(|e| format!("Failed to add dependency: {}", e))?;
    }
    
    // Add provides
    let provides = if recipe.provides.is_empty() {
        vec![recipe.name.clone()]
    } else {
        recipe.provides.clone()
    };
    
    for provide in &provides {
        db.add_provides(pkg_id, provide, None, "virtual")
            .map_err(|e| format!("Failed to add provide: {}", e))?;
    }
    
    // List installed files
    let files = store.list_package_files(&final_hash)?;
    for file in &files {
        db.add_file(pkg_id, file, "regular")
            .map_err(|e| format!("Failed to add file: {}", e))?;
    }
    
    // Create symlinks
    println!("7. Creating symlinks...");
    symlink_mgr.create_symlinks(
        pkg_id,
        &final_hash,
        &store.get_package_path(&final_hash),
        &files,
    )?;
    
    // Run post-install script if provided
    if let Some(install_script) = &recipe.install {
        println!("8. Running post-install script...");
        run_script(install_script)?;
    }
    
    // Cleanup
    println!("9. Cleaning up...");
    let _ = fs::remove_dir_all(&build_dir);
    
    Ok(())
}

fn download_source(url: &str, dest_dir: &str) -> Result<String, String> {
    let filename = url.split('/').last().unwrap_or("source.tar.gz");
    let dest_path = format!("{}/{}", dest_dir, filename);
    
    let mut response = reqwest::blocking::get(url)
        .map_err(|e| format!("Download failed: {}", e))?;
    
    if !response.status().is_success() {
        return Err(format!("HTTP error: {}", response.status()));
    }
    
    let mut file = fs::File::create(&dest_path)
        .map_err(|e| format!("Failed to create file: {}", e))?;
    
    std::io::copy(&mut response, &mut file)
        .map_err(|e| format!("Failed to write file: {}", e))?;
    
    println!("   Downloaded to {}", dest_path);
    Ok(dest_path)
}

fn extract_tarball(tarball: &str, dest_dir: &str) -> Result<(), String> {
    let output = ProcessCommand::new("tar")
        .arg("-xf")
        .arg(tarball)
        .arg("-C")
        .arg(dest_dir)
        .output()
        .map_err(|e| format!("Failed to run tar: {}", e))?;
    
    if !output.status.success() {
        return Err(format!("Extraction failed: {}", String::from_utf8_lossy(&output.stderr)));
    }
    
    Ok(())
}

fn find_extracted_dir(build_dir: &str) -> Result<String, String> {
    let entries = fs::read_dir(build_dir)
        .map_err(|e| format!("Failed to read build dir: {}", e))?;
    
    for entry in entries {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let path = entry.path();
        
        if path.is_dir() && !path.ends_with("install") {
            return Ok(path.to_string_lossy().to_string());
        }
    }
    
    Ok(build_dir.to_string())
}

fn run_build_command(cmd: &str, work_dir: &str, build_root: &str, recipe: &BuildRecipe) -> Result<(), String> {
    let host_arch = std::env::consts::ARCH;
    
    let output = ProcessCommand::new("bash")
        .arg("-c")
        .arg(cmd)
        .current_dir(work_dir)
        .env("PAX_BUILD_ROOT", build_root)
        .env("PAX_ARCH", host_arch)
        .env("PAX_PACKAGE_NAME", &recipe.name)
        .env("PAX_PACKAGE_VERSION", &recipe.version)
        .output()
        .map_err(|e| format!("Failed to run build command: {}", e))?;
    
    if !output.status.success() {
        println!("\n--- Build Output ---");
        println!("{}", String::from_utf8_lossy(&output.stdout));
        println!("\n--- Build Errors ---");
        println!("{}", String::from_utf8_lossy(&output.stderr));
        return Err("Build command failed".to_string());
    }
    
    println!("   Build successful");
    Ok(())
}

fn run_script(script: &str) -> Result<(), String> {
    let output = ProcessCommand::new("bash")
        .arg("-c")
        .arg(script)
        .output()
        .map_err(|e| format!("Failed to run script: {}", e))?;
    
    if !output.status.success() {
        println!("Warning: Post-install script failed");
        println!("{}", String::from_utf8_lossy(&output.stderr));
    }
    
    Ok(())
}

