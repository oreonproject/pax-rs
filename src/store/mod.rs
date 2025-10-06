use std::fs::{self, File, create_dir_all};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;
use zstd::stream::read::Decoder as ZstdDecoder;
use tar::Archive;

const STORE_BASE: &str = "/opt/pax/store";
const LINKS_BASE: &str = "/opt/pax/links";

// Package store for content-addressed storage
pub struct PackageStore {
    store_path: PathBuf,
    links_path: PathBuf,
}

impl PackageStore {
    // Create new store instance
    pub fn new() -> Result<Self, String> {
        Self::with_paths(STORE_BASE, LINKS_BASE)
    }

    // Create store with custom paths (mainly for testing)
    pub fn with_paths(store_base: &str, links_base: &str) -> Result<Self, String> {
        let store_path = PathBuf::from(store_base);
        let links_path = PathBuf::from(links_base);

        create_dir_all(&store_path)
            .map_err(|e| format!("Failed to create store directory: {}", e))?;
        create_dir_all(&links_path)
            .map_err(|e| format!("Failed to create links directory: {}", e))?;

        // create subdirectories for links
        create_dir_all(links_path.join("bin"))
            .map_err(|e| format!("Failed to create bin directory: {}", e))?;
        create_dir_all(links_path.join("lib"))
            .map_err(|e| format!("Failed to create lib directory: {}", e))?;
        create_dir_all(links_path.join("share"))
            .map_err(|e| format!("Failed to create share directory: {}", e))?;

        Ok(PackageStore {
            store_path,
            links_path,
        })
    }

    // Get path to package in store by hash
    pub fn get_package_path(&self, hash: &str) -> PathBuf {
        self.store_path.join(hash)
    }

    // Check if package exists in store
    pub fn has_package(&self, hash: &str) -> bool {
        self.get_package_path(hash).exists()
    }

    // Extract a .pax package (zstd compressed tarball) to store
    pub fn extract_pax_package<P: AsRef<Path>>(
        &self,
        package_path: P,
        hash: &str,
    ) -> Result<Vec<String>, String> {
        let dest = self.get_package_path(hash);
        
        if dest.exists() {
            return Err(format!("Package {} already exists in store", hash));
        }

        create_dir_all(&dest)
            .map_err(|e| format!("Failed to create package directory: {}", e))?;

        // Open and decompress the package file
        let file = File::open(package_path.as_ref())
            .map_err(|e| format!("Failed to open package file: {}", e))?;
        
        let decoder = ZstdDecoder::new(file)
            .map_err(|e| format!("Failed to create zstd decoder: {}", e))?;

        // Extract tarball
        let mut archive = Archive::new(decoder);
        archive.unpack(&dest)
            .map_err(|e| format!("Failed to extract archive: {}", e))?;

        // List extracted files
        let files = self.list_package_files(hash)?;
        Ok(files)
    }

    // Extract a generic tarball to store
    pub fn extract_tarball<P: AsRef<Path>>(
        &self,
        tarball_path: P,
        hash: &str,
    ) -> Result<Vec<String>, String> {
        let dest = self.get_package_path(hash);
        
        if dest.exists() {
            return Err(format!("Package {} already exists in store", hash));
        }

        create_dir_all(&dest)
            .map_err(|e| format!("Failed to create package directory: {}", e))?;

        let file = File::open(tarball_path.as_ref())
            .map_err(|e| format!("Failed to open tarball: {}", e))?;

        let mut archive = Archive::new(file);
        archive.unpack(&dest)
            .map_err(|e| format!("Failed to extract archive: {}", e))?;

        let files = self.list_package_files(hash)?;
        Ok(files)
    }

    // Extract files from a directory into the store
    pub fn copy_directory<P: AsRef<Path>>(
        &self,
        source_dir: P,
        hash: &str,
    ) -> Result<Vec<String>, String> {
        let dest = self.get_package_path(hash);
        
        if dest.exists() {
            return Err(format!("Package {} already exists in store", hash));
        }

        create_dir_all(&dest)
            .map_err(|e| format!("Failed to create package directory: {}", e))?;

        // recursively copy all files
        copy_dir_recursive(source_dir.as_ref(), &dest)?;

        let files = self.list_package_files(hash)?;
        Ok(files)
    }

    // List all files in a package
    pub fn list_package_files(&self, hash: &str) -> Result<Vec<String>, String> {
        let package_path = self.get_package_path(hash);
        
        if !package_path.exists() {
            return Err(format!("Package {} not found in store", hash));
        }

        let mut files = Vec::new();
        for entry in WalkDir::new(&package_path) {
            let entry = entry.map_err(|e| format!("Failed to walk directory: {}", e))?;
            if entry.file_type().is_file() {
                let relative = entry.path()
                    .strip_prefix(&package_path)
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                files.push(relative);
            }
        }

        Ok(files)
    }

    // Calculate total size of a package in store
    pub fn get_package_size(&self, hash: &str) -> Result<u64, String> {
        let package_path = self.get_package_path(hash);
        
        if !package_path.exists() {
            return Err(format!("Package {} not found in store", hash));
        }

        let mut total_size = 0u64;
        for entry in WalkDir::new(&package_path) {
            let entry = entry.map_err(|e| format!("Failed to walk directory: {}", e))?;
            if entry.file_type().is_file() {
                total_size += entry.metadata()
                    .map_err(|e| format!("Failed to get file metadata: {}", e))?
                    .len();
            }
        }

        Ok(total_size)
    }

    // Remove a package from store
    pub fn remove_package(&self, hash: &str) -> Result<(), String> {
        let package_path = self.get_package_path(hash);
        
        if !package_path.exists() {
            return Ok(()); // already gone
        }

        fs::remove_dir_all(&package_path)
            .map_err(|e| format!("Failed to remove package: {}", e))?;

        Ok(())
    }

    // Get the links base directory
    pub fn links_path(&self) -> &Path {
        &self.links_path
    }

    // List orphaned packages (not in database)
    pub fn list_all_hashes(&self) -> Result<Vec<String>, String> {
        let mut hashes = Vec::new();
        
        if !self.store_path.exists() {
            return Ok(hashes);
        }

        let entries = fs::read_dir(&self.store_path)
            .map_err(|e| format!("Failed to read store directory: {}", e))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
            if entry.file_type()
                .map_err(|e| format!("Failed to get file type: {}", e))?
                .is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    hashes.push(name.to_string());
                }
            }
        }

        Ok(hashes)
    }

    // Garbage collect orphaned packages
    pub fn garbage_collect(&self, installed_hashes: &[String]) -> Result<Vec<String>, String> {
        let all_hashes = self.list_all_hashes()?;
        let mut removed = Vec::new();

        for hash in all_hashes {
            if !installed_hashes.contains(&hash) {
                println!("Removing orphaned package: {}", hash);
                self.remove_package(&hash)?;
                removed.push(hash);
            }
        }

        Ok(removed)
    }
}

impl Default for PackageStore {
    fn default() -> Self {
        Self::new().expect("Failed to initialize package store")
    }
}

// helper to recursively copy directories
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    if !dst.exists() {
        create_dir_all(dst)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    for entry in fs::read_dir(src)
        .map_err(|e| format!("Failed to read directory: {}", e))? {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)
                .map_err(|e| format!("Failed to copy file: {}", e))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_store_creation() {
        let temp_dir = TempDir::new().unwrap();
        let store = PackageStore::with_paths(
            temp_dir.path().join("store").to_str().unwrap(),
            temp_dir.path().join("links").to_str().unwrap(),
        ).unwrap();

        assert!(store.store_path.exists());
        assert!(store.links_path.exists());
    }

    #[test]
    fn test_has_package() {
        let temp_dir = TempDir::new().unwrap();
        let store = PackageStore::with_paths(
            temp_dir.path().join("store").to_str().unwrap(),
            temp_dir.path().join("links").to_str().unwrap(),
        ).unwrap();

        assert!(!store.has_package("test_hash"));
        create_dir_all(store.get_package_path("test_hash")).unwrap();
        assert!(store.has_package("test_hash"));
    }
}

