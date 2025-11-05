use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use settings::OriginKind;
use utils::err;

#[derive(Debug, Clone)]
pub struct DebRepositoryClient {
    base_url: String,
    client: Client,
}

impl DebRepositoryClient {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url,
            client: Client::new(),
        }
    }

    pub fn from_origin(origin: &OriginKind) -> Option<Self> {
        match origin {
            OriginKind::Deb(url) | OriginKind::Apt(url) => Some(Self::new(url.clone())),
            _ => None,
        }
    }

    pub async fn list_packages(&self) -> Result<Vec<DebPackageInfo>, String> {
        // Try to fetch Packages.gz or Packages file
        let packages_url = format!("{}/Packages.gz", self.base_url);
        let packages_text_url = format!("{}/Packages", self.base_url);
        
        let response = match self.client.get(&packages_url).send().await {
            Ok(response) => response,
            Err(_) => {
                self.client.get(&packages_text_url).send().await
                    .map_err(|e| format!("Failed to fetch package list: {}", e))?
            }
        };

        if !response.status().is_success() {
            return err!("Failed to fetch package list: {}", response.status());
        }

        let content = response.text().await
            .map_err(|e| format!("Failed to read package list: {}", e))?;

        // Check if it's gzipped
        let packages_content = if packages_url.ends_with(".gz") {
            self.decompress_gzip(&content)?
        } else {
            content
        };

        self.parse_packages_file(&packages_content)
    }

    pub async fn get_package(&self, package_name: &str, version: Option<&str>) -> Result<DebPackageInfo, String> {
        let packages = self.list_packages().await?;
        
        let package = packages.iter()
            .find(|p| p.name == package_name)
            .ok_or_else(|| format!("Package {} not found", package_name))?;

        if let Some(version) = version {
            if package.version != version {
                return err!("Package {} version {} not found (available: {})", package_name, version, package.version);
            }
        }

        Ok(package.clone())
    }

    pub async fn download_package(&self, package_info: &DebPackageInfo) -> Result<Vec<u8>, String> {
        let response = self.client
            .get(&package_info.url)
            .send()
            .await
            .map_err(|e| format!("Failed to download package: {}", e))?;

        if !response.status().is_success() {
            return err!("Failed to download package: {}", response.status());
        }

        let bytes = response.bytes().await
            .map_err(|e| format!("Failed to read package data: {}", e))?;

        Ok(bytes.to_vec())
    }

    fn parse_packages_file(&self, content: &str) -> Result<Vec<DebPackageInfo>, String> {
        let mut packages = Vec::new();
        let mut current_package = HashMap::new();
        
        for line in content.lines() {
            let line = line.trim();
            
            if line.is_empty() {
                // End of package entry
                if !current_package.is_empty() {
                    if let Some(package) = self.parse_package_entry(&current_package)? {
                        packages.push(package);
                    }
                    current_package.clear();
                }
            } else if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_lowercase();
                let value = value.trim().to_string();
                current_package.insert(key, value);
            }
        }

        // Handle last package if file doesn't end with empty line
        if !current_package.is_empty() {
            if let Some(package) = self.parse_package_entry(&current_package)? {
                packages.push(package);
            }
        }

        Ok(packages)
    }

    fn parse_package_entry(&self, entry: &HashMap<String, String>) -> Result<Option<DebPackageInfo>, String> {
        let name = entry.get("package").ok_or("Missing Package field")?;
        let version = entry.get("version").ok_or("Missing Version field")?;
        let description = entry.get("description").unwrap_or(&"No description".to_string()).clone();
        let filename = entry.get("filename").ok_or("Missing Filename field")?;
        let default_size = "0".to_string();
        let size_str = entry.get("size").unwrap_or(&default_size);
        let size = size_str.parse::<u64>().unwrap_or(0);

        // Parse dependencies
        let mut dependencies = Vec::new();
        if let Some(depends) = entry.get("depends") {
            dependencies = self.parse_dependencies(depends);
        }

        let url = format!("{}/{}", self.base_url, filename);

        Ok(Some(DebPackageInfo {
            name: name.clone(),
            version: version.clone(),
            description,
            size,
            url,
            dependencies,
            architecture: entry.get("architecture").unwrap_or(&"all".to_string()).clone(),
            section: entry.get("section").unwrap_or(&"misc".to_string()).clone(),
            priority: entry.get("priority").unwrap_or(&"optional".to_string()).clone(),
        }))
    }

    fn parse_dependencies(&self, depends: &str) -> Vec<String> {
        depends.split(',')
            .map(|dep| dep.trim().split_whitespace().next().unwrap_or("").to_string())
            .filter(|dep| !dep.is_empty())
            .collect()
    }

    fn decompress_gzip(&self, data: &str) -> Result<String, String> {
        use flate2::read::GzDecoder;
        use std::io::Read;
        
        let bytes = data.as_bytes();
        let mut decoder = GzDecoder::new(bytes);
        let mut decompressed = String::new();
        decoder.read_to_string(&mut decompressed)
            .map_err(|e| format!("Failed to decompress gzip: {}", e))?;
        
        Ok(decompressed)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebPackageInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub size: u64,
    pub url: String,
    pub dependencies: Vec<String>,
    pub architecture: String,
    pub section: String,
    pub priority: String,
}

pub async fn test_deb_connection(origin: &OriginKind) -> Result<bool, String> {
    let client = match DebRepositoryClient::from_origin(origin) {
        Some(client) => client,
        None => return Ok(false),
    };

    // Try to list packages to test connection
    match client.list_packages().await {
        Ok(_) => Ok(true),
        Err(e) => {
            println!("Deb repository connection test failed: {}", e);
            Ok(false)
        }
    }
}
