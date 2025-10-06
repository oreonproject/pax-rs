use indicatif::{ProgressBar, ProgressStyle};
use reqwest::blocking::Client;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

const CACHE_DIR: &str = "/var/cache/pax";
const DOWNLOAD_TIMEOUT_SECS: u64 = 300;

// Download manager for package files
pub struct DownloadManager {
    client: Client,
    cache_dir: PathBuf,
}

impl DownloadManager {
    // Create new download manager
    pub fn new() -> Result<Self, String> {
        Self::with_cache_dir(CACHE_DIR)
    }

    // Create with custom cache directory
    pub fn with_cache_dir(cache_dir: &str) -> Result<Self, String> {
        let cache_dir = PathBuf::from(cache_dir);
        
        fs::create_dir_all(&cache_dir)
            .map_err(|e| format!("Failed to create cache directory: {}", e))?;

        let client = Client::builder()
            .timeout(Duration::from_secs(DOWNLOAD_TIMEOUT_SECS))
            .build()
            .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

        Ok(DownloadManager {
            client,
            cache_dir,
        })
    }

    // Download a package file
    pub fn download_package(
        &self,
        url: &str,
        package_name: &str,
        version: &str,
    ) -> Result<PathBuf, String> {
        let filename = format!("{}-{}.pkg", package_name, version);
        let dest_path = self.cache_dir.join(&filename);

        // Check if already cached
        if dest_path.exists() {
            println!("Using cached package: {}", filename);
            return Ok(dest_path);
        }

        println!("Downloading {} from {}...", package_name, url);

        // Download with progress bar
        let mut response = self.client.get(url)
            .send()
            .map_err(|e| format!("Failed to download: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Download failed with status: {}", response.status()));
        }

        let total_size = response.content_length().unwrap_or(0);
        
        let pb = if total_size > 0 {
            let pb = ProgressBar::new(total_size);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template("[{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
                    .unwrap()
                    .progress_chars("#>-")
            );
            Some(pb)
        } else {
            None
        };

        // Download to temp file first
        let temp_path = dest_path.with_extension("tmp");
        let mut file = File::create(&temp_path)
            .map_err(|e| format!("Failed to create file: {}", e))?;

        let mut downloaded = 0u64;
        let mut buffer = [0u8; 8192];

        loop {
            match std::io::Read::read(&mut response, &mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    file.write_all(&buffer[..n])
                        .map_err(|e| format!("Failed to write file: {}", e))?;
                    downloaded += n as u64;
                    if let Some(pb) = &pb {
                        pb.set_position(downloaded);
                    }
                }
                Err(e) => return Err(format!("Download error: {}", e)),
            }
        }

        if let Some(pb) = pb {
            pb.finish_with_message("Download complete");
        }

        // Move temp file to final location
        fs::rename(&temp_path, &dest_path)
            .map_err(|e| format!("Failed to move downloaded file: {}", e))?;

        Ok(dest_path)
    }

    // Download signature file
    pub fn download_signature(
        &self,
        url: &str,
        package_name: &str,
        version: &str,
    ) -> Result<PathBuf, String> {
        let filename = format!("{}-{}.sig", package_name, version);
        let dest_path = self.cache_dir.join(&filename);

        // Check if already cached
        if dest_path.exists() {
            return Ok(dest_path);
        }

        println!("Downloading signature...");

        let mut response = self.client.get(url)
            .send()
            .map_err(|e| format!("Failed to download signature: {}", e))?;

        if !response.status().is_success() {
            return Err(format!("Signature download failed: {}", response.status()));
        }

        let mut file = File::create(&dest_path)
            .map_err(|e| format!("Failed to create signature file: {}", e))?;

        io::copy(&mut response, &mut file)
            .map_err(|e| format!("Failed to write signature: {}", e))?;

        Ok(dest_path)
    }

    // Clear cache
    pub fn clear_cache(&self) -> Result<(), String> {
        if self.cache_dir.exists() {
            fs::remove_dir_all(&self.cache_dir)
                .map_err(|e| format!("Failed to clear cache: {}", e))?;
            fs::create_dir_all(&self.cache_dir)
                .map_err(|e| format!("Failed to recreate cache directory: {}", e))?;
        }

        Ok(())
    }

    // Get cache size
    pub fn get_cache_size(&self) -> Result<u64, String> {
        let mut total_size = 0u64;

        if !self.cache_dir.exists() {
            return Ok(0);
        }

        for entry in fs::read_dir(&self.cache_dir)
            .map_err(|e| format!("Failed to read cache directory: {}", e))? {
            let entry = entry
                .map_err(|e| format!("Failed to read entry: {}", e))?;
            let metadata = entry.metadata()
                .map_err(|e| format!("Failed to get metadata: {}", e))?;
            
            if metadata.is_file() {
                total_size += metadata.len();
            }
        }

        Ok(total_size)
    }

    // Remove old cached files
    pub fn clean_old_cache(&self, keep_latest: usize) -> Result<Vec<String>, String> {
        use std::collections::HashMap;
        use std::time::SystemTime;
        
        let mut removed = Vec::new();
        
        if keep_latest == 0 {
            return Ok(removed);
        }
        
        // Group files by package name (extract from filename)
        let mut packages: HashMap<String, Vec<(String, SystemTime)>> = HashMap::new();
        
        let entries = fs::read_dir(&self.cache_dir)
            .map_err(|e| format!("Failed to read cache directory: {}", e))?;
        
        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
            let path = entry.path();
            
            if !path.is_file() {
                continue;
            }
            
            let filename = match path.file_name().and_then(|n| n.to_str()) {
                Some(f) => f.to_string(),
                None => continue,
            };
            
            // Extract package name (everything before first version-like pattern)
            let pkg_name = if let Some(pos) = filename.find(|c: char| c.is_numeric()) {
                filename[..pos].trim_end_matches('-').to_string()
            } else {
                filename.clone()
            };
            
            // Get file modification time
            let metadata = fs::metadata(&path)
                .map_err(|e| format!("Failed to get file metadata: {}", e))?;
            let modified = metadata.modified()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            
            packages.entry(pkg_name)
                .or_insert_with(Vec::new)
                .push((path.to_string_lossy().to_string(), modified));
        }
        
        // For each package, keep only the latest N versions
        for (_pkg_name, mut files) in packages {
            if files.len() <= keep_latest {
                continue;
            }
            
            // Sort by modification time (newest first)
            files.sort_by(|a, b| b.1.cmp(&a.1));
            
            // Remove old versions
            for (path, _) in files.iter().skip(keep_latest) {
                match fs::remove_file(path) {
                    Ok(_) => {
                        removed.push(path.clone());
                        println!("Removed old cached file: {}", path);
                    }
                    Err(e) => {
                        eprintln!("Warning: Failed to remove {}: {}", path, e);
                    }
                }
            }
        }
        
        Ok(removed)
    }
}

impl Default for DownloadManager {
    fn default() -> Self {
        Self::new().expect("Failed to create download manager")
    }
}

