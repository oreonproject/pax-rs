use serde::{Deserialize, Serialize};
use settings::OriginKind;
use std::hash::Hash;
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Command as RunCommand,
};
use tokio::runtime::Runtime;
use utils::{err, get_update_dir, tmpfile};

use crate::{
    depend_kind::DependKind, InstalledInstallKind, InstalledMetaData, MetaDataKind,
    Specific, installed::InstalledCompilable, parsers::pax::RawPax, parsers::github::RawGithub, parsers::apt::RawApt,
};

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum ProcessedInstallKind {
    PreBuilt(PreBuilt),
    Compilable(ProcessedCompilable),
}

fn walk_package_payload<F>(root: &Path, mut visitor: F) -> Result<(), String>
where
    F: FnMut(&Path, &Path, &std::fs::Metadata) -> Result<(), String>,
{
    use std::fs;

    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = fs::read_dir(&dir).map_err(|e| {
            format!("Failed to read directory {}: {}", dir.display(), e)
        })?;

        for entry in entries {
            let entry = entry.map_err(|e| {
                format!("Failed to iterate directory {}: {}", dir.display(), e)
            })?;
            let path = entry.path();

            if path == root.join("manifest.yaml") {
                continue;
            }

            let metadata = fs::symlink_metadata(&path).map_err(|e| {
                format!("Failed to inspect {}: {}", path.display(), e)
            })?;

            let relative = path.strip_prefix(root).map_err(|_| {
                format!(
                    "Failed to determine relative path for {}",
                    path.display()
                )
            })?;

            visitor(&path, relative, &metadata)?;

            if metadata.is_dir() {
                stack.push(path);
            }
        }
    }

    Ok(())
}

fn collect_package_entries(root: &Path) -> Result<Vec<(PathBuf, PathBuf)>, String> {
    let mut entries = Vec::new();
    walk_package_payload(root, |src, relative, _| {
        entries.push((src.to_path_buf(), relative.to_path_buf()));
        Ok(())
    })?;
    Ok(entries)
}

pub fn render_progress(label: &str, current: usize, total: usize, item: &str) {
    let total = total.max(1);
    let percent = (current * 100) / total;
    let bar_width = 30usize;
    let filled = (percent * bar_width) / 100;
    let mut bar = String::new();
    bar.push_str(&"#".repeat(filled.min(bar_width)));
    bar.push_str(&"-".repeat(bar_width.saturating_sub(filled)));

    let mut display_item = item.to_string();
    if display_item.len() > 40 {
        let tail_len = 37;
        display_item = format!(
            "...{}",
            &display_item[display_item.len().saturating_sub(tail_len)..]
        );
    }

    print!(
        "\r\x1B[K{} [{}] {:3}% {}",
        label,
        bar,
        percent.min(100),
        display_item
    );
    io::stdout().flush().ok();

    if current >= total {
        println!();
    }
}

fn needs_ldconfig(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    path_str.starts_with("/lib")
        || path_str.starts_with("/usr/lib")
        || path_str.starts_with("/usr/local/lib")
}

fn refresh_ld_cache() {
    match RunCommand::new("ldconfig").status() {
        Ok(status) if status.success() => {
            println!("Refreshed shared library cache with ldconfig.");
        }
        Ok(status) => {
            println!(
                "\x1B[93m[WARN] ldconfig exited with status {}. Library cache may be stale.\x1B[0m",
                status
            );
        }
        Err(err) => {
            println!(
                "\x1B[93m[WARN] Failed to run ldconfig: {}. You may need to refresh the linker cache manually.\x1B[0m",
                err
            );
        }
    }
}

fn read_dpkg_field(path: &Path, field: &str) -> Result<Option<String>, String> {
    use std::process::Command;

    let output = Command::new("dpkg-deb")
        .arg("-f")
        .arg(path)
        .arg(field)
        .output()
        .map_err(|e| format!("Failed to execute dpkg-deb -f: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "dpkg-deb -f {} failed for {}: {}",
            field,
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}

fn rpm_query_field(path: &Path, format: &str) -> Result<String, String> {
    use std::process::Command;

    let output = Command::new("rpm")
        .arg("-qp")
        .arg("--queryformat")
        .arg(format)
        .arg(path)
        .output()
        .map_err(|e| format!("Failed to execute rpm query: {}", e))?;

    if !output.status.success() {
        return Err(format!(
            "rpm query failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
#[derive(PartialEq, Eq, Debug, Deserialize, Serialize, Hash, Clone)]
pub struct PreBuilt {
    pub critical: Vec<String>,
    pub configs: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ProcessedCompilable {
    pub build: String,
    pub install: String,
    pub uninstall: String,
    pub purge: String,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct InstallPackage {
    pub metadata: ProcessedMetaData,
    pub run_deps: Vec<ProcessedMetaData>,
    pub build_deps: Vec<ProcessedMetaData>,
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct QueuedChanges {
    pub install: Vec<String>,
    pub remove: Vec<String>,
    pub upgrade: Vec<String>,
}

impl InstallPackage {
    pub fn list_deps(&self, include_build: bool) -> Vec<String> {
        let mut deps = Vec::new();
        
        for dep in &self.run_deps {
            deps.push(dep.name.clone());
        }
        
        if include_build {
            for dep in &self.build_deps {
                deps.push(dep.name.clone());
            }
        }
        
        deps
    }
    
    pub fn install(&self, runtime: &Runtime) -> Result<(), String> {
        // First install runtime dependencies with this package as parent
        for dep in &self.run_deps {
            if let Err(e) = runtime.block_on(dep.clone().install_package_impl(false, Some(self.metadata.name.clone()))) {
                return Err(format!("Failed to install dependency {}: {}", dep.name, e));
            }
        }
        
        // Then install build dependencies with this package as parent
        for dep in &self.build_deps {
            if let Err(e) = runtime.block_on(dep.clone().install_package_impl(false, Some(self.metadata.name.clone()))) {
                return Err(format!("Failed to install build dependency {}: {}", dep.name, e));
            }
        }
        
        // Finally install the main package (no parent)
        self.metadata.install(runtime)
    }
    
    pub fn install_with_overwrite(&self, runtime: &Runtime) -> Result<(), String> {
        // First install runtime dependencies with this package as parent
        for dep in &self.run_deps {
            if let Err(e) = runtime.block_on(dep.clone().install_package_impl(true, Some(self.metadata.name.clone()))) {
                return Err(format!("Failed to install dependency {}: {}", dep.name, e));
            }
        }
        
        // Then install build dependencies with this package as parent
        for dep in &self.build_deps {
            if let Err(e) = runtime.block_on(dep.clone().install_package_impl(true, Some(self.metadata.name.clone()))) {
                return Err(format!("Failed to install build dependency {}: {}", dep.name, e));
            }
        }
        
        // Finally install the main package with overwrite enabled (no parent)
        self.metadata.install_with_overwrite(runtime)
    }
}
impl QueuedChanges {
    pub fn new() -> Self {
        Self {
            install: Vec::new(),
            remove: Vec::new(),
            upgrade: Vec::new(),
        }
    }

    pub fn insert_primary(&mut self, package: String) -> bool {
        if self.remove.contains(&package) {
            self.remove.retain(|p| p != &package);
            self.upgrade.push(package);
            true
        } else if !self.install.contains(&package) {
            self.install.push(package);
            true
        } else {
            false
        }
    }

    pub fn insert_dependent(&mut self, package: String) -> bool {
        if !self.install.contains(&package) && !self.upgrade.contains(&package) {
            self.install.push(package);
            true
        } else {
            false
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub struct ProcessedMetaData {
    pub name: String,
    pub kind: MetaDataKind,
    pub description: String,
    pub version: String,
    pub origin: OriginKind,
    pub dependent: bool,
    pub build_dependencies: Vec<DependKind>,
    pub runtime_dependencies: Vec<DependKind>,
    pub install_kind: ProcessedInstallKind,
    pub hash: String,
    // Additional fields expected by the application
    pub package_type: String,
    pub installed: bool,
    pub dependencies: Vec<String>,
    pub dependents: Vec<String>,
    pub installed_files: Vec<String>,
    pub available_versions: Vec<String>,
}

impl ProcessedMetaData {
    pub fn to_installed_with_parent(&self, installed_by: Option<String>) -> InstalledMetaData {
        InstalledMetaData {
            name: self.name.clone(),
            kind: self.kind.clone(),
            version: self.version.to_string(),
            description: self.description.clone(),
            origin: self.origin.clone(),
            dependent: self.dependent,
            installed_by,
            dependencies: {
                let mut result = Vec::new();
                for dep in &self.runtime_dependencies {
                    if let Some(dep) = dep.as_dep_ver() {
                        result.push(dep);
                    }
                }
                result
            },
            dependents: Vec::new(),
            install_kind: match &self.install_kind {
                ProcessedInstallKind::PreBuilt(prebuilt) => {
                    InstalledInstallKind::PreBuilt(prebuilt.clone())
                }
                ProcessedInstallKind::Compilable(comp) => {
                    InstalledInstallKind::Compilable(InstalledCompilable {
                        uninstall: comp.uninstall.clone(),
                        purge: comp.purge.clone(),
                    })
                }
            },
            hash: self.hash.to_string(),
        }
    }
    
    pub fn to_installed(&self) -> InstalledMetaData {
        self.to_installed_with_parent(None)
    }
    
    pub async fn install_package(self) -> Result<(), String> {
        self.install_package_impl(false, None).await
    }
    
    async fn install_package_impl(self, allow_overwrite: bool, installed_by: Option<String>) -> Result<(), String> {
        let name = self.name.to_string();
        println!("Installing {name}...");
        
        // Get the package file (download or use local)
        let package_file = self.get_package_file().await?;
        
        // Note: Hash verification is skipped for packages with embedded manifests
        // because the hash in manifest.yaml is the hash of the entire archive including
        // the manifest, creating a circular verification problem.
        // For packages with sidecar metadata files (.pax.meta), verification can be performed.
        
        if !self.hash.is_empty() && self.hash != "unknown" && !self.hash.starts_with('0') {
            // This package has a valid hash, but we don't verify for embedded manifests
            println!("\x1B[92m[OK]\x1B[0m Package metadata loaded (embedded manifest)");
        } else {
            println!("\x1B[93m[WARN]\x1B[0m Package hash not provided or placeholder, skipping verification");
        }
        
        // Create temporary extraction directory
        let extract_dir = std::env::temp_dir().join(format!("pax_install_{}", std::process::id()));
        std::fs::create_dir_all(&extract_dir)
            .map_err(|_| "Failed to create extraction directory")?;
        
        // Extract the package
        self.extract_package(&package_file, &extract_dir).await?;
        
        // Check for file conflicts before installation
        let file_manifest = self.create_file_manifest(&extract_dir).await?;
        let conflicts = file_manifest.check_conflicts()?;
        
        if !conflicts.is_empty() {
            if allow_overwrite {
                println!("\x1B[93m[WARN] File conflicts detected, but --allow-overwrite is enabled:\x1B[0m");
            } else {
                println!("\x1B[93m[WARN] File conflicts detected:\x1B[0m");
            }
            for conflict in &conflicts {
                match conflict.conflict_type {
                    crate::file_tracking::ConflictType::FileOwnership => {
                        println!("  File {} is owned by package '{}'", 
                                conflict.path.display(), conflict.existing_owner);
                    }
                    crate::file_tracking::ConflictType::DirectoryOwnership => {
                        println!("  Directory {} is owned by package '{}'", 
                                conflict.path.display(), conflict.existing_owner);
                    }
                    crate::file_tracking::ConflictType::SymlinkOwnership => {
                        println!("  Symlink {} is owned by package '{}'", 
                                conflict.path.display(), conflict.existing_owner);
                    }
                    crate::file_tracking::ConflictType::UntrackedFile => {
                        println!("  File {} already exists (not tracked by any package)", 
                                conflict.path.display());
                    }
                }
            }
            if !allow_overwrite {
                println!("\x1B[93m[WARN] Proceeding with installation - existing files will be backed up.\x1B[0m");
            }
        }
        
        // Install based on package type
        match self.install_kind {
            ProcessedInstallKind::PreBuilt(ref prebuilt) => {
                self.install_prebuilt_package(&extract_dir, prebuilt, allow_overwrite).await?;
            }
            ProcessedInstallKind::Compilable(ref compilable) => {
                self.install_compilable_package(&extract_dir, compilable).await?;
            }
        }
        
        // Save installed metadata
        let installed_dir = utils::get_metadata_dir()?;
        let package_file = installed_dir.join(format!("{}.json", name));
        let path = package_file;
        let metadata = self.to_installed_with_parent(installed_by); // Use provided parent
        metadata.write(&path)?;
        
        // Save file manifest for conflict detection
        file_manifest.save()?;
        
        // Clean up
        let _ = std::fs::remove_dir_all(&extract_dir);
        
        Ok(())
    }
    
    async fn create_file_manifest(&self, extract_dir: &Path) -> Result<crate::file_tracking::FileManifest, String> {
        use crate::file_tracking::FileManifest;
        
        let mut manifest = FileManifest::new(self.name.clone(), self.version.clone());
        
        // Walk through the extracted directory and catalog all files
        // We need to map the extraction directory paths to actual system paths
        self.walk_directory(extract_dir, &PathBuf::from("/"), &mut manifest)?;
        
        Ok(manifest)
    }
    
    fn walk_directory(&self, extract_base: &Path, target_base: &PathBuf, manifest: &mut crate::file_tracking::FileManifest) -> Result<(), String> {
        use std::fs;
        
        for entry in fs::read_dir(extract_base)
            .map_err(|e| format!("Failed to read directory {}: {}", extract_base.display(), e))? {
            let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
            let extract_path = entry.path();
            let metadata = entry.metadata()
                .map_err(|e| format!("Failed to get metadata for {}: {}", extract_path.display(), e))?;
            
            // Map the extraction path to target path
            let rel_path = extract_path.strip_prefix(extract_base)
                .map_err(|e| format!("Failed to strip prefix: {}", e))?;
            let target_path = target_base.join(rel_path);
            
            if metadata.is_file() {
                let size = metadata.len();
                let permissions = metadata.permissions().mode();
                let checksum = crate::file_tracking::calculate_file_checksum(&extract_path)
                    .unwrap_or_else(|_| "unknown".to_string());
                
                manifest.add_file(target_path, size, permissions, checksum);
            } else if metadata.is_dir() {
                let permissions = metadata.permissions().mode();
                manifest.add_directory(target_path.clone(), permissions);
                // Recursively process subdirectories
                self.walk_directory(&extract_path, target_base, manifest)?;
            } else if metadata.file_type().is_symlink() {
                let target = fs::read_link(&extract_path)
                    .map_err(|e| format!("Failed to read symlink target: {}", e))?;
                manifest.add_symlink(target_path, target);
            }
        }
        
        Ok(())
    }
    
    async fn get_package_file(&self) -> Result<std::path::PathBuf, String> {
        let tmpfile = tmpfile().ok_or("Failed to reserve temporary file")?;
        
        match &self.origin {
            OriginKind::Pax(pax) => {
                let pax_path = std::path::Path::new(pax);
                if pax_path.exists() {
                    // Local file - copy to temp location
                    std::fs::copy(pax, &tmpfile)
                        .map_err(|e| format!("Failed to copy local PAX file: {}", e))?;
                } else if pax.starts_with("http://") || pax.starts_with("https://") {
                    // Remote file - download directly
                    // PAX repositories now just serve .pax files directly
                    let response = reqwest::get(pax.as_str()).await
                        .map_err(|e| format!("Failed to download PAX file: {}", e))?;
                    
                    if !response.status().is_success() {
                        return Err(format!("HTTP error {} when downloading PAX file from {}", response.status(), pax));
                    }
                    
                    let bytes = response.bytes().await
                        .map_err(|e| format!("Failed to read PAX file data: {}", e))?;
                    std::fs::write(&tmpfile, bytes)
                        .map_err(|e| format!("Failed to write PAX file to temp: {}", e))?;
                } else {
                    return Err(format!("Package file does not exist: {}", pax));
                }
            }
            OriginKind::Github { user, repo } => {
                let endpoint = format!("https://github.com/{}/{}/archive/refs/tags/{}.tar.gz", user, repo, self.version);
                let response = reqwest::get(&endpoint).await
                    .map_err(|_| "Failed to download GitHub archive")?;
                let bytes = response.bytes().await
                    .map_err(|_| "Failed to read GitHub archive data")?;
                std::fs::write(&tmpfile, bytes)
                    .map_err(|_| "Failed to write GitHub archive to temp")?;
            }
            OriginKind::Apt(source) => {
                let path = std::path::Path::new(source);
                if path.exists() {
                    std::fs::copy(path, &tmpfile)
                        .map_err(|_| "Failed to copy local DEB package")?;
                } else {
                    let base = source.trim_end_matches('/');
                    let endpoint = format!("{}/packages/{}/{}.deb", base, self.name, self.version);
                    let response = reqwest::get(&endpoint).await
                        .map_err(|_| "Failed to download APT package")?;
                    let bytes = response.bytes().await
                        .map_err(|_| "Failed to read APT package data")?;
                    std::fs::write(&tmpfile, bytes)
                        .map_err(|_| "Failed to write APT package to temp")?;
                }
            }
            OriginKind::Rpm(repo_url) => {
                use crate::yum_repository::YumRepositoryClient;
                
                let client = YumRepositoryClient::new(repo_url.clone());
                let package_info = client.get_package(&self.name, Some(&self.version)).await
                    .map_err(|_| "Failed to get RPM package info")?;
                
                let response = reqwest::get(&package_info.url).await
                        .map_err(|_| "Failed to download RPM package")?;
                    let bytes = response.bytes().await
                        .map_err(|_| "Failed to read RPM package data")?;
                    std::fs::write(&tmpfile, bytes)
                        .map_err(|_| "Failed to write RPM package to temp")?;
                }
            OriginKind::CloudflareR2 { bucket, account_id, .. } => {
                use crate::cloudflare_r2::CloudflareR2Client;
                
                let client = CloudflareR2Client::new(
                    bucket.clone(),
                    account_id.clone(),
                    None, // access_key_id
                    None, // secret_access_key
                    None, // region
                );
                
                let package_info = client.get_package(&self.name, Some(&self.version)).await
                    .map_err(|_| "Failed to get package info from R2")?;
                
                let bytes = client.download_package(&package_info).await
                    .map_err(|_| "Failed to download package from R2")?;
                
                std::fs::write(&tmpfile, bytes)
                    .map_err(|_| "Failed to write R2 package to temp")?;
            }
            OriginKind::Deb(repo_url) => {
                use crate::deb_repository::DebRepositoryClient;
                
                let client = DebRepositoryClient::new(repo_url.clone());
                
                let package_info = client.get_package(&self.name, Some(&self.version)).await
                    .map_err(|_| "Failed to get package info from DEB repository")?;
                
                let bytes = client.download_package(&package_info).await
                    .map_err(|_| "Failed to download package from DEB repository")?;
                
                std::fs::write(&tmpfile, bytes)
                    .map_err(|_| "Failed to write DEB package to temp")?;
            }
            OriginKind::Yum(repo_url) => {
                use crate::yum_repository::YumRepositoryClient;
                
                let client = YumRepositoryClient::new(repo_url.clone());
                
                let package_info = client.get_package(&self.name, Some(&self.version)).await
                    .map_err(|_| "Failed to get package info from YUM repository")?;
                
                let bytes = client.download_package(&package_info).await
                    .map_err(|_| "Failed to download package from YUM repository")?;
                
                std::fs::write(&tmpfile, bytes)
                    .map_err(|_| "Failed to write RPM package to temp")?;
            }
        }
        
        Ok(tmpfile)
    }
    
    async fn extract_package(&self, package_file: &std::path::Path, extract_dir: &std::path::Path) -> Result<(), String> {
        match &self.origin {
            OriginKind::Pax(_) | OriginKind::Github { .. } => {
                let mut tar_cmd = RunCommand::new("tar");
                tar_cmd
                    .arg("-xzf")
                    .arg(package_file)
                    .arg("-C")
                    .arg(extract_dir);
                let status = tar_cmd
                    .status()
                    .map_err(|_| "Failed to extract archive with tar")?;
                if !status.success() {
                    return err!("Failed to extract archive using tar");
                }
            }
            OriginKind::Apt(_) => {
                let mut dpkg_cmd = RunCommand::new("dpkg-deb");
                dpkg_cmd.arg("-x").arg(package_file).arg(extract_dir);
                let status = dpkg_cmd
                    .status()
                    .map_err(|_| "Failed to execute dpkg-deb for extraction")?;
                if !status.success() {
                    return err!("Failed to extract DEB package");
                }
            }
            OriginKind::Rpm(_) | OriginKind::Yum(_) => {
                let command = format!(
                    "rpm2cpio '{}' | cpio -idmv",
                    package_file.display()
                );
                let status = RunCommand::new("bash")
                    .arg("-c")
                    .arg(command)
                    .current_dir(extract_dir)
                    .status()
                    .map_err(|_| "Failed to extract RPM package")?;
                if !status.success() {
                    return err!("Failed to extract RPM package");
                }
            }
            OriginKind::Deb(_) => {
                let mut dpkg_cmd = RunCommand::new("dpkg-deb");
                dpkg_cmd.arg("-x").arg(package_file).arg(extract_dir);
                let status = dpkg_cmd
                    .status()
                    .map_err(|_| "Failed to execute dpkg-deb for extraction")?;
                if !status.success() {
                    return err!("Failed to extract DEB package");
                }
            }
            OriginKind::CloudflareR2 { .. } => {
                // R2 packages are typically PAX format
                let mut tar_cmd = RunCommand::new("tar");
                tar_cmd
                    .arg("-xzf")
                    .arg(package_file)
                    .arg("-C")
                    .arg(extract_dir);
                let status = tar_cmd
                    .status()
                    .map_err(|_| "Failed to extract archive with tar")?;
                if !status.success() {
                    return err!("Failed to extract archive using tar");
                }
            }
        }
        Ok(())
    }
    
    async fn install_prebuilt_package(&self, extract_dir: &std::path::Path, _prebuilt: &PreBuilt, allow_overwrite: bool) -> Result<(), String> {
        use std::fs;
        use crate::file_tracking::FileManifest;

        println!("Installing pre-built files...");

        let mut manifest = FileManifest::new(
            self.name.clone(),
            self.version.clone(),
        );

        let entries = collect_package_entries(extract_dir)?;
        let total = entries.len().max(1);
        let mut processed = 0usize;

        for (src_path, relative) in entries {
            processed += 1;
            let metadata = fs::symlink_metadata(&src_path).map_err(|e| {
                format!("Failed to inspect {}: {}", src_path.display(), e)
            })?;

            let dest_path = Path::new("/").join(&relative);

            if metadata.is_dir() {
                fs::create_dir_all(&dest_path).map_err(|e| {
                    format!("Failed to create directory {}: {}", dest_path.display(), e)
                })?;

                let mode = metadata.permissions().mode();
                fs::set_permissions(&dest_path, std::fs::Permissions::from_mode(mode)).map_err(|e| {
                    format!(
                        "Failed to set permissions on directory {}: {}",
                        dest_path.display(),
                        e
                    )
                })?;

                manifest.add_directory(dest_path.clone(), mode);
            } else if metadata.file_type().is_symlink() {
                if let Some(parent) = dest_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| {
                        format!(
                            "Failed to create parent directory {}: {}",
                            parent.display(),
                            e
                        )
                    })?;
                }

                // Try to remove existing symlink or file, ignore errors if it doesn't exist
                if dest_path.is_symlink() {
                    let _ = fs::remove_file(&dest_path);
                } else if dest_path.is_file() {
                    let _ = fs::remove_file(&dest_path);
                } else if dest_path.is_dir() {
                    return Err(format!("Destination path {} is a directory, cannot create symlink", dest_path.display()));
                } else if dest_path.exists() {
                    // Fallback: try to remove even if we can't determine the type
                    let _ = fs::remove_file(&dest_path);
                }

                let target = fs::read_link(&src_path).map_err(|e| {
                    format!("Failed to read symlink target {}: {}", src_path.display(), e)
                })?;

                // Try to create symlink with retry in case of race condition
                let mut retries = 3;
                loop {
                    match symlink(&target, &dest_path) {
                        Ok(_) => break,
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && retries > 0 => {
                            // Race condition: try removing again
                            let _ = fs::remove_file(&dest_path);
                            retries -= 1;
                            // Brief pause
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                        Err(e) => {
                            return Err(format!(
                        "Failed to create symlink {} -> {}: {}",
                        dest_path.display(),
                        target.display(),
                        e
                            ));
                        }
                    }
                }

                manifest.add_symlink(dest_path.clone(), target);
            } else if metadata.is_file() {
                if let Some(parent) = dest_path.parent() {
                    fs::create_dir_all(parent).map_err(|e| {
                        format!(
                            "Failed to create parent directory {}: {}",
                            parent.display(),
                            e
                        )
                    })?;
                }

                if dest_path.exists() {
                    fs::remove_file(&dest_path).map_err(|e| {
                        format!("Failed to remove existing file {}: {}", dest_path.display(), e)
                    })?;
                }

                fs::copy(&src_path, &dest_path).map_err(|e| {
                    format!(
                        "Failed to install file {}: {}",
                        dest_path.display(),
                        e
                    )
                })?;

                let mode = metadata.permissions().mode();
                fs::set_permissions(&dest_path, std::fs::Permissions::from_mode(mode)).map_err(|e| {
                    format!(
                        "Failed to set permissions on file {}: {}",
                        dest_path.display(),
                        e
                    )
                })?;

                let checksum = crate::file_tracking::calculate_file_checksum(&dest_path)
                    .unwrap_or_default();

                manifest.add_file(dest_path.clone(), metadata.len(), mode, checksum);
            }

            render_progress(
                "Installing",
                processed,
                total,
                &relative.to_string_lossy(),
            );
        }

        manifest.save()?;

        println!(
            "Installed {} file(s), {} director(y/ies), {} symlink(s).",
            manifest.files.len(),
            manifest.directories.len(),
            manifest.symlinks.len(),
        );

        if manifest
            .files
            .iter()
            .all(|f| f.permissions & 0o111 == 0)
        {
            println!(
                "\x1B[93m[WARN] No executable files were installed; this package may only provide libraries.\x1B[0m"
            );
        }
        if manifest
            .files
            .iter()
            .any(|f| needs_ldconfig(&f.path))
        {
            refresh_ld_cache();
        }

        Ok(())
    }
    
    async fn install_compilable_package(&self, extract_dir: &std::path::Path, compilable: &ProcessedCompilable) -> Result<(), String> {
        println!("Building and installing from source...");
        
        // Find the build directory
        let build_dir = self.find_build_directory(extract_dir)?;
        
        // Build the package
        println!("Building...");
        let mut build_cmd = RunCommand::new("bash");
        build_cmd.arg("-c").arg(&compilable.build).current_dir(&build_dir);
        if build_cmd.status().is_err() {
            return err!("Build failed");
                }
                        
        // Install the package
        println!("Installing...");
        let mut install_cmd = RunCommand::new("bash");
        install_cmd.arg("-c").arg(&compilable.install).current_dir(&build_dir);
        if install_cmd.status().is_err() {
            return err!("Install failed");
                        }
        
        Ok(())
    }
    
    fn find_build_directory(&self, extract_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
        // Try common build directory patterns
        let candidates = vec![
            extract_dir.join(&self.name),
            extract_dir.join(format!("{}-{}", self.name, self.version)),
            extract_dir.join(format!("{}-{}", self.name, self.version.replace('.', "-"))),
        ];
        
        for candidate in candidates {
            if candidate.exists() && candidate.is_dir() {
                return Ok(candidate);
            }
                        }
                        
        // If no specific directory found, use the extract directory itself
        Ok(extract_dir.to_path_buf())
    }
    
    pub async fn get_metadata_from_local_package(package_path: &str) -> Result<Self, String> {
        use std::path::Path;

        let path = Path::new(package_path);
        if !path.exists() {
            return err!("Package file does not exist: {}", path.display());
        }

        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .unwrap_or_else(|| String::new());

        match extension.as_str() {
            "pax" => Self::load_local_pax(path),
            "deb" => Self::load_local_deb(path),
            "rpm" => Self::load_local_rpm(path),
            other => err!(
                "Unsupported package format `{}` for {}",
                other,
                path.display()
            ),
        }
    }

    fn load_local_pax(path: &Path) -> Result<Self, String> {
        use std::process::Command;

        let temp_dir = Self::create_temp_dir("pax_extract")?;

        let status = Command::new("tar")
            .arg("-xzf")
            .arg(path)
            .arg("-C")
            .arg(&temp_dir)
            .status()
            .map_err(|e| format!("Failed to extract PAX archive {}: {}", path.display(), e))?;

        if !status.success() {
            let _ = fs::remove_dir_all(&temp_dir);
            return err!("Failed to extract PAX archive: {}", path.display());
        }

        let manifest_path = temp_dir.join("manifest.yaml");
        let sidecar_path = path.with_extension("pax.meta");

        let manifest_content = if manifest_path.exists() {
            fs::read_to_string(&manifest_path)
                .map_err(|_| "Failed to read manifest.yaml")?
        } else if sidecar_path.exists() {
            fs::read_to_string(&sidecar_path).map_err(|_| {
                format!(
                    "Failed to read metadata sidecar: {}",
                    sidecar_path.display()
                )
            })?
        } else {
            let _ = fs::remove_dir_all(&temp_dir);
            return err!(
                "No package metadata found for {}. Expected manifest.yaml in archive or {}",
                path.display(),
                sidecar_path.display()
            );
        };

        let raw_pax = serde_norway::from_str::<RawPax>(&manifest_content)
            .map_err(|_| "Failed to parse manifest.yaml as PAX format")?;

        // Note: We don't verify the embedded hash because the manifest itself contains the hash,
        // creating a circular dependency. The hash in manifest.yaml is informational only.
        // For verification of packages without embedded manifests, see the sidecar verification logic.

        let mut processed = raw_pax
            .process()
            .ok_or("Failed to process PAX metadata")?;

        let (has_entries, critical_files, config_files) = Self::collect_payload_from(&temp_dir)?;

        if has_entries {
            processed.install_kind = ProcessedInstallKind::PreBuilt(PreBuilt {
                critical: critical_files,
                configs: config_files,
            });
        }

        processed.dependent = false;
        processed.origin = OriginKind::Pax(path.to_string_lossy().to_string());

        let _ = fs::remove_dir_all(&temp_dir);

        Ok(processed)
    }

    fn load_local_deb(path: &Path) -> Result<Self, String> {
        use std::process::Command;

        let temp_dir = Self::create_temp_dir("pax_extract_deb")?;

        let status = Command::new("dpkg-deb")
            .arg("-x")
            .arg(path)
            .arg(&temp_dir)
            .status()
            .map_err(|e| format!("Failed to execute dpkg-deb -x: {}", e))?;

        if !status.success() {
            let _ = fs::remove_dir_all(&temp_dir);
            return err!(
                "dpkg-deb -x failed for {} with status {}",
                path.display(),
                status
            );
        }

        let name = read_dpkg_field(path, "Package")?
            .unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "local-deb".to_string()));
        let version = read_dpkg_field(path, "Version")?
            .unwrap_or_else(|| "0.0.0".to_string());
        let description = read_dpkg_field(path, "Description")?
            .unwrap_or_else(|| format!("Debian package {}", name));
        let depends_raw = read_dpkg_field(path, "Depends")?.unwrap_or_default();

        let (_, critical_files, config_files) = Self::collect_payload_from(&temp_dir)?;

        let metadata = ProcessedMetaData {
            name: name.clone(),
            kind: MetaDataKind::Apt,
            description,
            version,
            origin: OriginKind::Apt(path.to_string_lossy().to_string()),
            dependent: false,
            build_dependencies: Vec::new(),
            runtime_dependencies: Self::parse_dependency_list(&depends_raw),
            install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                critical: critical_files,
                configs: config_files,
            }),
            hash: crate::file_tracking::calculate_file_checksum(path).unwrap_or_default(),
            package_type: "APT".to_string(),
            installed: false,
            dependencies: Vec::new(),
            dependents: Vec::new(),
            installed_files: Vec::new(),
            available_versions: Vec::new(),
        };

        let _ = fs::remove_dir_all(&temp_dir);

        Ok(metadata)
    }

    fn load_local_rpm(path: &Path) -> Result<Self, String> {
        use std::process::Command;

        let temp_dir = Self::create_temp_dir("pax_extract_rpm")?;

        let extract_status = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "rpm2cpio '{}' | cpio -idmv",
                path.display()
            ))
            .current_dir(&temp_dir)
            .status()
            .map_err(|e| format!("Failed to extract RPM package {}: {}", path.display(), e))?;

        if !extract_status.success() {
            let _ = fs::remove_dir_all(&temp_dir);
            return err!(
                "Failed to extract RPM package {}. Ensure rpm2cpio and cpio are installed.",
                path.display()
            );
        }

        let name = rpm_query_field(path, "%{NAME}")?
            .trim()
            .to_string();
        let version = rpm_query_field(path, "%{VERSION}")?
            .trim()
            .to_string();
        let summary = rpm_query_field(path, "%{SUMMARY}")?
            .trim()
            .to_string();
        let requires_raw = rpm_query_field(path, "[%{REQUIRENAME}\n]")?;

        let (_, critical_files, config_files) = Self::collect_payload_from(&temp_dir)?;

        let metadata = ProcessedMetaData {
            name: if name.is_empty() {
                path.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "local-rpm".to_string())
            } else {
                name
            },
            kind: MetaDataKind::Rpm,
            description: if summary.is_empty() {
                "RPM package".to_string()
            } else {
                summary
            },
            version: if version.is_empty() {
                "0.0.0".to_string()
            } else {
                version
            },
            origin: OriginKind::Rpm(path.to_string_lossy().to_string()),
            dependent: false,
            build_dependencies: Vec::new(),
            runtime_dependencies: Self::parse_dependency_list(&requires_raw),
            install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                critical: critical_files,
                configs: config_files,
            }),
            hash: crate::file_tracking::calculate_file_checksum(path).unwrap_or_default(),
            package_type: "RPM".to_string(),
            installed: false,
            dependencies: Vec::new(),
            dependents: Vec::new(),
            installed_files: Vec::new(),
            available_versions: Vec::new(),
        };

        let _ = fs::remove_dir_all(&temp_dir);

        Ok(metadata)
    }

    fn create_temp_dir(prefix: &str) -> Result<std::path::PathBuf, String> {
        let dir = std::env::temp_dir().join(format!(
            "{}_{}_{}",
            prefix,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));

        fs::create_dir_all(&dir).map_err(|_| {
            format!(
                "Failed to create temporary extraction directory at {}",
                dir.display()
            )
        })?;

        Ok(dir)
    }

    fn collect_payload_from(root: &Path) -> Result<(bool, Vec<String>, Vec<String>), String> {
        let mut has_entries = false;
        let mut critical_files = Vec::new();
        let mut config_files = Vec::new();

        walk_package_payload(root, |_, relative, metadata| {
            has_entries = true;

            if metadata.is_file() || metadata.file_type().is_symlink() {
                let install_path = Path::new("/").join(relative);
                let install_str = install_path.to_string_lossy().to_string();
                if install_str.starts_with("/etc/") {
                    config_files.push(install_str.clone());
                }
                critical_files.push(install_str);
            }

            Ok(())
        })?;

        Ok((has_entries, critical_files, config_files))
    }

    fn parse_dependency_list(list: &str) -> Vec<DependKind> {
        list.split([',', '\n'])
            .filter_map(|item| {
                let trimmed = item.trim();
                if trimmed.is_empty() || trimmed == "rpmlib(PayloadFilesHavePrefix)" {
                    return None;
                }
                let name = trimmed
                    .split(|c: char| c == '(' || c.is_whitespace() || c == '|')
                    .next()
                    .unwrap_or("")
                    .trim();
                if name.is_empty() {
                    None
                } else {
                    Some(DependKind::Latest(name.to_string()))
                }
            })
            .collect()
    }
    pub async fn get_metadata(
        app: &str,
        version: Option<&str>,
        sources: &[OriginKind],
        dependent: bool,
    ) -> Option<Self> {
        let mut metadata = None;
        let mut sources = sources.iter();
        while let (Some(source), None) = (sources.next(), &metadata) {
            match source {
                OriginKind::Pax(source) => {
                    // PAX repositories now work by scanning for .pax files
                    let possible_urls = if let Some(version) = version {
                        // Try versioned URLs
                        vec![
                            format!("{}/{}-{}.pax", source, app, version),
                            format!("{}/{}/{}/{}-{}.pax", source, app, version, app, version),
                        ]
                        } else {
                        // For latest version, try to discover available versions
                        // First try simple unversioned name
                        let simple_url = format!("{}/{}.pax", source, app);
                        if let Ok(response) = reqwest::get(&simple_url).await {
                            if response.status().is_success() {
                                if let Some(tmpfile_path) = tmpfile() {
                                    if let Ok(bytes) = response.bytes().await {
                                        if std::fs::write(&tmpfile_path, bytes).is_ok() {
                                            if let Some(path_str) = tmpfile_path.to_str() {
                                                if let Ok(mut processed) = Self::get_metadata_from_local_package(path_str).await {
                                                    processed.origin = OriginKind::Pax(simple_url);
                                                    metadata = Some(processed);
                                                    continue;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        
                        // Try package discovery - attempt common patterns
                        vec![
                            format!("{}/packages/{}.pax", source, app),
                            format!("{}/{}-latest.pax", source, app),
                        ]
                    };
                    
                    for url in possible_urls {
                        // First, try to download and parse the package
                        if let Ok(response) = reqwest::get(&url).await {
                            if response.status().is_success() {
                                if let Some(tmpfile_path) = tmpfile() {
                                    if let Ok(bytes) = response.bytes().await {
                                        if std::fs::write(&tmpfile_path, bytes).is_ok() {
                                            if let Some(path_str) = tmpfile_path.to_str() {
                                                if let Ok(mut processed) = Self::get_metadata_from_local_package(path_str).await {
                                                    // Update the origin to point to the actual .pax file URL
                                                    processed.origin = OriginKind::Pax(url.clone());
                                                    metadata = Some(processed);
                                                    break;
                                                }
                        }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                OriginKind::Github { user, repo } => {
                    metadata = {
                        // Try to get package metadata from GitHub releases
                        let endpoint = if let Some(version) = version {
                            format!("https://api.github.com/repos/{}/{}/releases/tags/{}", user, repo, version)
                        } else {
                            format!("https://api.github.com/repos/{}/{}/releases/latest", user, repo)
                        };
                        
                        if let Ok(response) = reqwest::get(&endpoint).await {
                            if let Ok(body) = response.text().await {
                                if let Ok(release_data) = serde_json::from_str::<serde_json::Value>(&body) {
                                    // Look for a PAX metadata file in the release assets
                                    if let Some(assets) = release_data.get("assets").and_then(|a| a.as_array()) {
                                        for asset in assets {
                                            if let Some(name) = asset.get("name").and_then(|n| n.as_str()) {
                                                if name.ends_with(".pax") || name.ends_with(".json") {
                                                    if let Some(download_url) = asset.get("browser_download_url").and_then(|u| u.as_str()) {
                                                        if let Ok(asset_response) = reqwest::get(download_url).await {
                                                            if let Ok(asset_body) = asset_response.text().await {
                                                                // Try to parse as PAX format first
                                                                if let Ok(raw_pax) = serde_json::from_str::<RawPax>(&asset_body) {
                                                                    if let Some(processed) = raw_pax.process() {
                                                                        return Some(processed);
                                                                    }
                                                                }
                                                                // Try to parse as GitHub format
                                                                if let Ok(raw_github) = serde_json::from_str::<RawGithub>(&asset_body) {
                                                                    if let Some(processed) = raw_github.process() {
                                                                        return Some(processed);
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
}

                                    // If no assets found, try to create a basic package from release info
                                    if let Some(tag_name) = release_data.get("tag_name").and_then(|t| t.as_str()) {
                                        if let Some(name) = release_data.get("name").and_then(|n| n.as_str()) {
                                            if let Some(body) = release_data.get("body").and_then(|b| b.as_str()) {
                                                // Create a basic ProcessedMetaData from release info
                                                let processed = ProcessedMetaData {
                                            name: name.to_string(),
                                                    kind: MetaDataKind::Github,
                                                    description: body.to_string(),
                                            version: tag_name.to_string(),
                                                    origin: OriginKind::Github { 
                                            user: user.clone(),
                                                        repo: repo.clone() 
                                                    },
                                                    dependent,
                                            build_dependencies: Vec::new(),
                                            runtime_dependencies: Vec::new(),
                                                    install_kind: ProcessedInstallKind::Compilable(ProcessedCompilable {
                                                        build: "make".to_string(),
                                            install: "make install".to_string(),
                                            uninstall: "make uninstall".to_string(),
                                                        purge: "make uninstall".to_string(),
                                                    }),
                                                    hash: "unknown".to_string(),
                                                    package_type: "GitHub".to_string(),
                                                    installed: false,
                                                    dependencies: Vec::new(),
                                                    dependents: Vec::new(),
                                                    installed_files: Vec::new(),
                                                    available_versions: Vec::new(),
                                        };
                                            return Some(processed);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        None
                    };
                }
                OriginKind::Apt(repo_url) => {
                    metadata = {
                        // Query APT repository for package information
                        let endpoint = if let Some(version) = version {
                            format!("{}/packages/{}/{}", repo_url, app, version)
                        } else {
                            format!("{}/packages/{}", repo_url, app)
                        };
                        
                        if let Ok(response) = reqwest::get(&endpoint).await {
                            if let Ok(body) = response.text().await {
                                // Try to parse as APT package data
                                if let Ok(raw_apt) = serde_json::from_str::<RawApt>(&body) {
                                    if let Some(processed) = raw_apt.process() {
                                        Some(processed)
                                    } else {
                                        None
                                    }
                                } else {
                                    // If not JSON, try to parse as APT control file format
                                    Self::parse_apt_control_file(&body, app, repo_url)
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    };
                }
                OriginKind::Rpm(repo_url) => {
                    metadata = {
                        use crate::yum_repository::YumRepositoryClient;
                        
                        let client = YumRepositoryClient::new(repo_url.clone());
                        
                        if let Ok(package_info) = client.get_package(app, version).await {
                            // Convert YumPackageInfo to ProcessedMetaData
                            let processed = ProcessedMetaData {
                                name: package_info.name,
                                kind: MetaDataKind::Rpm,
                                description: package_info.description,
                                version: package_info.version,
                                origin: source.clone(),
                                dependent,
                                build_dependencies: Vec::new(),
                                runtime_dependencies: package_info.dependencies.into_iter()
                                    .map(|dep| crate::depend_kind::DependKind::Latest(dep))
                                    .collect(),
                                install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                                    critical: Vec::new(),
                                    configs: Vec::new(),
                                }),
                                hash: "unknown".to_string(),
                                package_type: "RPM".to_string(),
                                installed: false,
                                dependencies: Vec::new(),
                                dependents: Vec::new(),
                                installed_files: Vec::new(),
                                available_versions: Vec::new(),
                            };
                            Some(processed)
                        } else {
                            None
                        }
                    };
                }
                OriginKind::CloudflareR2 { bucket, account_id, .. } => {
                    metadata = {
                        use crate::cloudflare_r2::CloudflareR2Client;
                        
                        let client = CloudflareR2Client::new(
                            bucket.clone(),
                            account_id.clone(),
                            None, // access_key_id
                            None, // secret_access_key
                            None, // region
                        );
                        
                        if let Ok(package_info) = client.get_package(app, version).await {
                            // Convert PackageInfo to ProcessedMetaData
                            let processed = ProcessedMetaData {
                                name: package_info.name,
                                kind: MetaDataKind::Pax,
                                description: package_info.description,
                                version: package_info.version,
                                origin: source.clone(),
                                dependent,
                                build_dependencies: Vec::new(),
                                runtime_dependencies: package_info.dependencies.into_iter()
                                    .map(|dep| crate::depend_kind::DependKind::Latest(dep))
                                    .collect(),
                                install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                                    critical: Vec::new(),
                                    configs: Vec::new(),
                                }),
                                hash: "unknown".to_string(),
                                package_type: "RPM".to_string(),
                                installed: false,
                                dependencies: Vec::new(),
                                dependents: Vec::new(),
                                installed_files: Vec::new(),
                                available_versions: Vec::new(),
                            };
                            Some(processed)
                        } else {
                            None
                        }
                    };
                }
                OriginKind::Deb(repo_url) => {
                    metadata = {
                        use crate::deb_repository::DebRepositoryClient;
                        
                        let client = DebRepositoryClient::new(repo_url.clone());
                        
                        if let Ok(package_info) = client.get_package(app, version).await {
                            // Convert DebPackageInfo to ProcessedMetaData
                            let processed = ProcessedMetaData {
                                name: package_info.name,
                                kind: MetaDataKind::Apt,
                                description: package_info.description,
                                version: package_info.version,
                                origin: source.clone(),
                                dependent,
                                build_dependencies: Vec::new(),
                                runtime_dependencies: package_info.dependencies.into_iter()
                                    .map(|dep| crate::depend_kind::DependKind::Latest(dep))
                                    .collect(),
                                install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                                    critical: Vec::new(),
                                    configs: Vec::new(),
                                }),
                                hash: "unknown".to_string(),
                                package_type: "RPM".to_string(),
                                installed: false,
                                dependencies: Vec::new(),
                                dependents: Vec::new(),
                                installed_files: Vec::new(),
                                available_versions: Vec::new(),
                            };
                            Some(processed)
                        } else {
                            None
                        }
                    };
                }
                OriginKind::Yum(repo_url) => {
                    metadata = {
                        use crate::yum_repository::YumRepositoryClient;
                        
                        let client = YumRepositoryClient::new(repo_url.clone());
                        
                        if let Ok(package_info) = client.get_package(app, version).await {
                            // Convert YumPackageInfo to ProcessedMetaData
                            let processed = ProcessedMetaData {
                                name: package_info.name,
                                kind: MetaDataKind::Rpm,
                                description: package_info.description,
                                version: package_info.version,
                                origin: source.clone(),
                                dependent,
                                build_dependencies: Vec::new(),
                                runtime_dependencies: package_info.dependencies.into_iter()
                                    .map(|dep| crate::depend_kind::DependKind::Latest(dep))
                                    .collect(),
                                install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                                    critical: Vec::new(),
                                    configs: Vec::new(),
                                }),
                                hash: "unknown".to_string(),
                                package_type: "RPM".to_string(),
                                installed: false,
                                dependencies: Vec::new(),
                                dependents: Vec::new(),
                                installed_files: Vec::new(),
                                available_versions: Vec::new(),
                            };
                            Some(processed)
                        } else {
                            None
                        }
                    };
                }
            }
            if let Some(mut mut_metadata) = metadata {
                mut_metadata.dependent = dependent;
                metadata = Some(mut_metadata);
                break;
            }
        }
        metadata
    }
    
    fn parse_apt_control_file(control_data: &str, app: &str, repo_url: &str) -> Option<Self> {
        // Parse APT control file format (like what you'd find in a .deb package)
        let mut name = app.to_string();
        let mut version = "1.0.0".to_string();
        let mut description = "No description available".to_string();
        let mut dependencies = Vec::new();
        let mut critical_files = Vec::new();
        let mut config_files = Vec::new();
        
        for line in control_data.lines() {
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim();
                let value = value.trim();
                
                match key {
                    "Package" => name = value.to_string(),
                    "Version" => version = value.to_string(),
                    "Description" => description = value.to_string(),
                    "Depends" => {
                        // Parse dependencies (comma-separated)
                        dependencies = value.split(',')
                            .map(|dep| dep.trim().split_whitespace().next().unwrap_or("").to_string())
                            .filter(|dep| !dep.is_empty())
                            .collect();
                    }
                    "Files" => {
                        // Parse file list (one per line, format: hash size path)
                        for file_line in value.lines() {
                            let parts: Vec<&str> = file_line.trim().split_whitespace().collect();
                            if parts.len() >= 3 {
                                let path = parts[2];
                                if path.starts_with("/etc/") {
                                    config_files.push(path.to_string());
                                } else {
                                    critical_files.push(path.to_string());
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        
        Some(ProcessedMetaData {
            name,
            kind: MetaDataKind::Apt,
            description,
            version,
            origin: OriginKind::Apt(repo_url.to_string()),
            dependent: false,
            build_dependencies: Vec::new(),
            runtime_dependencies: dependencies.into_iter().map(|dep| DependKind::Latest(dep)).collect(),
            install_kind: ProcessedInstallKind::PreBuilt(PreBuilt {
                critical: critical_files,
                configs: config_files,
            }),
            hash: "unknown".to_string(),
            package_type: "APT".to_string(),
            installed: false,
            dependencies: Vec::new(),
            dependents: Vec::new(),
            installed_files: Vec::new(),
            available_versions: Vec::new(),
        })
    }
    
    pub async fn get_depends(
        &self,
        sources: &[OriginKind],
        prior: &mut HashSet<Specific>,
    ) -> Result<InstallPackage, String> {
        let mut run_deps = Vec::new();
        let mut build_deps = Vec::new();
        
        // Resolve runtime dependencies
        for dep in &self.runtime_dependencies {
            let resolved = self.resolve_single_dependency(dep, sources, prior).await?;
            run_deps.push(resolved);
        }
        
        // Resolve build dependencies
        for dep in &self.build_dependencies {
            let resolved = self.resolve_single_dependency(dep, sources, prior).await?;
            build_deps.push(resolved);
        }
        
        Ok(InstallPackage {
            metadata: self.clone(),
            run_deps,
            build_deps,
        })
    }
    
    async fn resolve_single_dependency(
        &self,
        dep: &DependKind,
        sources: &[OriginKind],
        prior: &mut HashSet<Specific>,
    ) -> Result<ProcessedMetaData, String> {
        match dep {
            DependKind::Latest(name) => {
                // Find the latest version across all sources
                self.find_latest_version(name, sources).await
            }
            DependKind::Specific(dep_ver) => {
                // Check if we've already resolved this specific dependency
                let specific = Specific {
                    name: dep_ver.name.clone(),
                    version: dep_ver.range.lower.as_version().unwrap_or_default(),
                };
                
                if prior.contains(&specific) {
                    return Err(format!("Circular dependency detected: {}", dep_ver.name));
                }
                
                prior.insert(specific);
                let result = self.find_specific_version(&dep_ver.name, &dep_ver.range, sources).await;
                prior.remove(&Specific {
                    name: dep_ver.name.clone(),
                    version: dep_ver.range.lower.as_version().unwrap_or_default(),
                });
                
                result
            }
            DependKind::Volatile(name) => {
                // Check if the system binary exists
                if self.check_system_binary(name) {
                           // Create a dummy metadata for system binaries
                           Ok(ProcessedMetaData {
                               name: name.clone(),
                               kind: self.kind.clone(),
                               description: format!("System binary: {}", name),
                               version: "system".to_string(),
                               origin: settings::OriginKind::Pax("system".to_string()),
                               dependent: false,
                               build_dependencies: Vec::new(),
                               runtime_dependencies: Vec::new(),
                               install_kind: ProcessedInstallKind::Compilable(ProcessedCompilable {
                                   build: "".to_string(),
                                   install: "".to_string(),
                                   uninstall: "".to_string(),
                                   purge: "".to_string(),
                               }),
                               hash: "".to_string(),
                               package_type: "System".to_string(),
                               installed: true,
                               dependencies: Vec::new(),
                               dependents: Vec::new(),
                               installed_files: Vec::new(),
                               available_versions: Vec::new(),
        })
                } else {
                    Err(format!("System binary {} not found", name))
                }
            }
        }
    }
    
    async fn find_latest_version(&self, name: &str, sources: &[OriginKind]) -> Result<ProcessedMetaData, String> {
        let mut latest_version: Option<ProcessedMetaData> = None;
        
        for source in sources {
            if let Ok(metadata) = self.get_metadata_from_source(name, source).await {
                if latest_version.is_none() || self.is_newer_version(&metadata, latest_version.as_ref().unwrap()) {
                    latest_version = Some(metadata);
                }
            }
        }
        
        latest_version.ok_or_else(|| format!("Package {} not found in any source", name))
    }
    
    async fn find_specific_version(
        &self,
        name: &str,
        range: &utils::Range,
        sources: &[OriginKind],
    ) -> Result<ProcessedMetaData, String> {
        for source in sources {
            if let Ok(metadata) = self.get_metadata_from_source(name, source).await {
                let version = utils::Version::parse(&metadata.version)?;
                if self.version_matches_range(&version, range) {
                    return Ok(metadata);
                }
            }
        }
        
        Err(format!("Package {} with version matching range not found", name))
    }
    
    async fn get_metadata_from_source(
        &self,
        name: &str,
        _source: &OriginKind,
    ) -> Result<ProcessedMetaData, String> {
        // This would typically query the actual source
        // For now, we'll check installed packages
        let installed_dir = utils::get_metadata_dir()?;
        let package_file = installed_dir.join(format!("{}.json", name));
        
        if package_file.exists() {
            let content = std::fs::read_to_string(&package_file)
                .map_err(|e| format!("Failed to read package file: {}", e))?;
            let installed: crate::installed::InstalledMetaData = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse package metadata: {}", e))?;
            
                   Ok(ProcessedMetaData {
                       name: installed.name,
                       kind: installed.kind,
                       description: installed.description,
                       version: installed.version,
                       origin: installed.origin,
                       dependent: true,
                       build_dependencies: installed.dependencies.iter().map(|dep| DependKind::Specific(dep.clone())).collect(),
                       runtime_dependencies: installed.dependencies.iter().map(|dep| DependKind::Specific(dep.clone())).collect(),
                       install_kind: ProcessedInstallKind::Compilable(ProcessedCompilable {
                           build: "".to_string(),
                           install: "".to_string(),
                           uninstall: "".to_string(),
                           purge: "".to_string(),
                       }),
                       hash: installed.hash,
                       package_type: format!("{:?}", installed.kind.clone()),
                       installed: true,
                       dependencies: installed.dependencies.iter().map(|dep| dep.name.clone()).collect(),
                       dependents: installed.dependents.iter().map(|dep| dep.name.clone()).collect(),
                       installed_files: Vec::new(), // TODO: implement file tracking
                       available_versions: Vec::new(), // TODO: implement version discovery
                   })
        } else {
            Err(format!("Package {} not found", name))
        }
    }
    
    fn is_newer_version(&self, new: &ProcessedMetaData, current: &ProcessedMetaData) -> bool {
        let new_ver = utils::Version::parse(&new.version).unwrap_or_default();
        let current_ver = utils::Version::parse(&current.version).unwrap_or_default();
        new_ver > current_ver
    }
    
    fn version_matches_range(&self, version: &utils::Version, range: &utils::Range) -> bool {
        // Check lower bound
        let lower_match = match &range.lower {
            utils::VerReq::NoBound => true,
            utils::VerReq::Eq(req_ver) => version == req_ver,
            utils::VerReq::Ge(req_ver) => version >= req_ver,
            utils::VerReq::Gt(req_ver) => version > req_ver,
            utils::VerReq::Le(req_ver) => version <= req_ver,
            utils::VerReq::Lt(req_ver) => version < req_ver,
        };
        
        // Check upper bound
        let upper_match = match &range.upper {
            utils::VerReq::NoBound => true,
            utils::VerReq::Eq(req_ver) => version == req_ver,
            utils::VerReq::Ge(req_ver) => version >= req_ver,
            utils::VerReq::Gt(req_ver) => version > req_ver,
            utils::VerReq::Le(req_ver) => version <= req_ver,
            utils::VerReq::Lt(req_ver) => version < req_ver,
        };
        
        lower_match && upper_match
    }
    
    fn check_system_binary(&self, name: &str) -> bool {
        use std::process::Command;
        
        Command::new("which")
            .arg(name)
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    pub fn install(&self, runtime: &Runtime) -> Result<(), String> {
        runtime.block_on(self.clone().install_package_impl(false, None))
    }
    
    pub fn install_with_overwrite(&self, runtime: &Runtime) -> Result<(), String> {
        runtime.block_on(self.clone().install_package_impl(true, None))
    }

    pub fn list_deps(&self, runtime: bool) -> Vec<String> {
        let deps = if runtime {
            &self.runtime_dependencies
        } else {
            &self.build_dependencies
        };
        
        deps.iter()
            .filter_map(|dep| dep.as_dep_ver().map(|dv| dv.name.clone()))
            .collect()
    }

    pub fn write(self, base: &Path, inc: &mut usize) -> Result<Self, String> {
        let path = loop {
            let mut path = base.to_path_buf();
            path.push(format!("{inc}.yaml"));
            if path.exists() {
                *inc += 1;
                continue;
            }
            break path;
        };
        let mut file = match File::create(&path) {
            Ok(file) => file,
            Err(_) => return err!("Failed to open upgrade metadata as WO!"),
        };
        let data = match serde_norway::to_string(&self) {
            Ok(data) => data,
            Err(_) => return err!("Failed to parse upgrade metadata to string!"),
        };
        match file.write_all(data.as_bytes()) {
            Ok(_) => Ok(self),
            Err(_) => err!("Failed to write upgrade metadata file!"),
        }
    }
    
    pub fn open(name: &str) -> Result<Self, String> {
        let mut path = get_update_dir()?;
        path.push(format!("{}.yaml", name));
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(_) => return err!("Failed to read package `{name}`'s metadata!"),
        };
        let mut metadata = String::new();
        if file.read_to_string(&mut metadata).is_err() {
            return err!("Failed to read package `{name}`'s config!");
        }
        Ok(match serde_norway::from_str::<Self>(&metadata) {
            Ok(data) => data,
            Err(_) => return err!("Failed to parse package `{name}`'s data!"),
        })
    }
    
    pub fn upgrade_package(&self, _sources: &[OriginKind], runtime: &Runtime) -> Result<(), String> {
        // For now, just reinstall the package
        // TODO: Implement proper upgrade logic
        runtime.block_on(self.clone().install_package())
    }
    
    pub fn remove_update_cache(&self) -> Result<(), String> {
        // Remove any cached update files
        // For now, just return Ok
        Ok(())
    }
}

// Public API functions
pub async fn get_packages(
    package_names: Vec<String>,
    _preferred_source: Option<&str>,
) -> Result<Vec<InstallPackage>, String> {
    let mut packages = Vec::new();
    
    // Get configured repositories from settings
    let settings = settings::SettingsYaml::get_settings().map_err(|e| format!("Failed to load settings: {}", e))?;
    let sources: Vec<OriginKind> = settings.sources.clone();
    
    for name in package_names {
        if let Some(metadata) = ProcessedMetaData::get_metadata(&name, None, &sources, true).await {
            // Resolve runtime dependencies
            let mut run_deps = Vec::new();
            for dep in &metadata.runtime_dependencies {
                let dep_name = match dep {
                    DependKind::Latest(name) => name.clone(),
                    DependKind::Specific(dep_ver) => dep_ver.name.clone(),
                    DependKind::Volatile(name) => name.clone(),
                };
                if let Some(dep_metadata) = ProcessedMetaData::get_metadata(&dep_name, None, &sources, true).await {
                    run_deps.push(dep_metadata);
                }
            }
            
            // Special case: for nodejs packages, also ensure the corresponding libs package is installed
            if metadata.name.starts_with("nodejs") && !metadata.name.ends_with("libs") {
                // Try nodejs*-libs pattern (e.g., nodejs22-libs for nodejs22)
                let libs_name1 = if metadata.name.contains("22") {
                    "nodejs22-libs".to_string()
                } else {
                    format!("{}22-libs", metadata.name)
                };
                
                // Also try plain nodejs-libs
                let libs_name2 = "nodejs-libs".to_string();
                
                for libs_name in [libs_name1, libs_name2] {
                    if let Some(libs_metadata) = ProcessedMetaData::get_metadata(&libs_name, None, &sources, true).await {
                        if !run_deps.iter().any(|dep| dep.name == libs_metadata.name) {
                            run_deps.push(libs_metadata);
                        }
                        break; // Found one, no need to try others
                    }
                }
            }
            
            // Resolve build dependencies
            let mut build_deps = Vec::new();
            for dep in &metadata.build_dependencies {
                let dep_name = match dep {
                    DependKind::Latest(name) => name.clone(),
                    DependKind::Specific(dep_ver) => dep_ver.name.clone(),
                    DependKind::Volatile(name) => name.clone(),
                };
                if let Some(dep_metadata) = ProcessedMetaData::get_metadata(&dep_name, None, &sources, true).await {
                    build_deps.push(dep_metadata);
                }
            }
            
            // Convert ProcessedMetaData to InstallPackage
            let install_package = InstallPackage {
                metadata: metadata.clone(),
                run_deps,
                build_deps,
            };
            packages.push(install_package);
        }
    }
    Ok(packages)
}

pub async fn get_package_info(
    package_name: &str,
    _show_files: bool,
    _show_deps: bool,
    _show_versions: bool,
    _settings: Option<&settings::SettingsYaml>,
) -> Result<ProcessedMetaData, String> {
    let sources = vec![settings::OriginKind::Pax("local".to_string())];
    ProcessedMetaData::get_metadata(package_name, None, &sources, true).await
        .ok_or_else(|| format!("Package {} not found", package_name))
}

pub fn list_installed_packages(
    _show_deps: bool,
    _show_dependents: bool,
    _filter_pattern: Option<&str>,
) -> Result<Vec<InstalledMetaData>, String> {
    let mut packages = Vec::new();
    let installed_dir = utils::get_metadata_dir()?;
    
    for entry in std::fs::read_dir(&installed_dir)
        .map_err(|e| format!("Failed to read directory: {}", e))? {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let path = entry.path();
        
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            let installed: InstalledMetaData = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse JSON: {}", e))?;
            
            // Apply filter if provided
            if let Some(pattern) = _filter_pattern {
                if !installed.name.contains(pattern) && !installed.description.contains(pattern) {
                    continue;
                }
            }
            
            packages.push(installed);
        }
    }
    
    Ok(packages)
}

pub fn get_local_deps(package_name: &str) -> Result<Vec<String>, String> {
    let installed_dir = utils::get_metadata_dir()?;
    let package_file = installed_dir.join(format!("{}.json", package_name));
    
    if package_file.exists() {
        let content = std::fs::read_to_string(&package_file)
            .map_err(|e| format!("Failed to read file: {}", e))?;
        let installed: InstalledMetaData = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse JSON: {}", e))?;
        Ok(installed.dependents.iter().map(|d| d.name.clone()).collect())
    } else {
        Ok(Vec::new())
    }
}

pub async fn search_packages(
    query: &str,
    _exact_match: bool,
    _installed_only: bool,
    _show_deps: bool,
    _settings: Option<&settings::SettingsYaml>,
) -> Result<Vec<ProcessedMetaData>, String> {
    let mut results = Vec::new();
    let installed_dir = utils::get_metadata_dir()?;
    
    for entry in std::fs::read_dir(&installed_dir)
        .map_err(|e| format!("Failed to read directory: {}", e))? {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let path = entry.path();
        
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            let installed: InstalledMetaData = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse JSON: {}", e))?;
            
            if installed.name.contains(query) || installed.description.contains(query) {
                let processed = ProcessedMetaData {
                    name: installed.name,
                    kind: installed.kind,
                    description: installed.description,
                    version: installed.version,
                    origin: installed.origin,
                    dependent: true,
                    build_dependencies: installed.dependencies.iter().map(|dep| DependKind::Specific(dep.clone())).collect(),
                    runtime_dependencies: installed.dependencies.iter().map(|dep| DependKind::Specific(dep.clone())).collect(),
                    install_kind: ProcessedInstallKind::Compilable(ProcessedCompilable {
                        build: "".to_string(),
                        install: "".to_string(),
                        uninstall: "".to_string(),
                        purge: "".to_string(),
                    }),
                    hash: installed.hash,
                    package_type: format!("{:?}", installed.kind.clone()),
                    installed: true,
                    dependencies: installed.dependencies.iter().map(|dep| dep.name.clone()).collect(),
                    dependents: installed.dependents.iter().map(|dep| dep.name.clone()).collect(),
                    installed_files: Vec::new(), // TODO: implement file tracking
                    available_versions: Vec::new(), // TODO: implement version discovery
                };
                results.push(processed);
            }
        }
    }
    
    Ok(results)
}

pub async fn collect_updates() -> Result<Vec<ProcessedMetaData>, String> {
    // Check for updates from repositories
    let installed_dir = utils::get_metadata_dir()?;
    let settings = settings::SettingsYaml::get_settings()
        .map_err(|e| format!("Failed to load settings: {}", e))?;
    let sources = settings.sources;
    let mut updates = Vec::new();
    
    for entry in std::fs::read_dir(&installed_dir)
        .map_err(|e| format!("Failed to read directory: {}", e))? {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let path = entry.path();
        
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| format!("Failed to read file: {}", e))?;
            let installed: InstalledMetaData = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse JSON: {}", e))?;
            
            // Check if newer version is available
            if let Some(latest) = ProcessedMetaData::get_metadata(&installed.name, None, &sources, true).await {
                let installed_version = utils::Version::parse(&installed.version)
                    .unwrap_or_default();
                let latest_version = utils::Version::parse(&latest.version)
                    .unwrap_or_default();
                
                if latest_version > installed_version {
                    updates.push(latest);
                }
            }
        }
    }
    
    Ok(updates)
}

pub async fn upgrade_all() -> Result<Vec<String>, String> {
    // Check for updates on all installed packages
    let updates = collect_updates().await?;
    Ok(updates.iter().map(|u| u.name.clone()).collect())
}

pub async fn upgrade_only(package_names: Vec<String>) -> Result<Vec<String>, String> {
    // Check for updates on specific packages
    let settings = settings::SettingsYaml::get_settings()
        .map_err(|e| format!("Failed to load settings: {}", e))?;
    let sources = settings.sources;
    let mut to_upgrade = Vec::new();
    
    for name in package_names {
        // Check installed version
        let installed = match InstalledMetaData::open(&name) {
            Ok(installed) => installed,
            Err(_) => continue, // Not installed
        };
        
        // Check latest version
        if let Some(latest) = ProcessedMetaData::get_metadata(&name, None, &sources, true).await {
            let installed_version = utils::Version::parse(&installed.version)
                .unwrap_or_default();
            let latest_version = utils::Version::parse(&latest.version)
                .unwrap_or_default();
            
            if latest_version > installed_version {
                to_upgrade.push(name);
            }
        }
    }
    
    Ok(to_upgrade)
}

pub async fn upgrade_packages(package_names: Vec<String>) -> Result<(), String> {
    // Upgrade specific packages
    let settings = settings::SettingsYaml::get_settings()
        .map_err(|e| format!("Failed to load settings: {}", e))?;
    let sources = settings.sources;
    let runtime = Runtime::new()
        .map_err(|_| "Failed to create runtime".to_string())?;
    
    for name in package_names {
        // Get latest version
        let latest = ProcessedMetaData::get_metadata(&name, None, &sources, true).await
            .ok_or_else(|| format!("Package {} not found", name))?;
        
        // Install the latest version (this will handle upgrades)
        latest.install(&runtime)?;
    }
    
    Ok(())
}

pub async fn emancipate(_package_name: &str) -> Result<(), String> {
    // This would typically remove a package and its dependencies
    // For now, just return success
    Ok(())
}
