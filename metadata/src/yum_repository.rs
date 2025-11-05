use reqwest::Client;
use serde::{Deserialize, Serialize};
use settings::OriginKind;
use utils::err;

#[derive(Debug, Clone)]
pub struct YumRepositoryClient {
    base_url: String,
    client: Client,
}

impl YumRepositoryClient {
    pub fn new(base_url: String) -> Self {
        Self {
            base_url,
            client: Client::new(),
        }
    }

    pub fn from_origin(origin: &OriginKind) -> Option<Self> {
        match origin {
            OriginKind::Yum(url) | OriginKind::Rpm(url) => Some(Self::new(url.clone())),
            _ => None,
        }
    }

    pub async fn list_packages(&self) -> Result<Vec<YumPackageInfo>, String> {
        // First, get the repomd.xml to find the correct primary.xml filename
        let repomd_url = format!("{}/repodata/repomd.xml", self.base_url);
        let repomd_response = self.client.get(&repomd_url).send().await
            .map_err(|e| format!("Failed to fetch repomd.xml: {}", e))?;

        if !repomd_response.status().is_success() {
            return err!("Failed to fetch repomd.xml: {}", repomd_response.status());
        }

        let repomd_content = repomd_response.text().await
            .map_err(|e| format!("Failed to read repomd.xml: {}", e))?;

        // Parse repomd.xml to find the primary.xml.gz filename
        let primary_filename = self.parse_repomd_for_primary(&repomd_content)?;
        let primary_url = format!("{}/{}", self.base_url, primary_filename);
        
        let response = self.client.get(&primary_url).send().await
            .map_err(|e| format!("Failed to fetch package list: {}", e))?;

        if !response.status().is_success() {
            return err!("Failed to fetch package list: {}", response.status());
        }

        let bytes = response.bytes().await
            .map_err(|e| format!("Failed to read package list: {}", e))?;

        // Check if it's gzipped and decompress if needed
        let packages_content = if primary_url.ends_with(".gz") {
            self.decompress_gzip_bytes(&bytes)?
        } else {
            String::from_utf8(bytes.to_vec())
                .map_err(|e| format!("Failed to convert bytes to string: {}", e))?
        };

        // Show parsing message
        print!("\rParsing packages... [                    ] 0 packages");
        std::io::Write::flush(&mut std::io::stdout()).ok();
        
        let result = self.parse_primary_xml(&packages_content);
        
        // Clear progress line
        print!("\r                                           \r");
        std::io::Write::flush(&mut std::io::stdout()).ok();
        
        result
    }

    pub async fn get_package(&self, package_name: &str, version: Option<&str>) -> Result<YumPackageInfo, String> {
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

    pub async fn download_package(&self, package_info: &YumPackageInfo) -> Result<Vec<u8>, String> {
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

    pub fn parse_repomd_for_primary(&self, repomd_xml: &str) -> Result<String, String> {
        // Simple XML parsing to find the primary.xml.gz filename
        let mut in_primary_data = false;
        for line in repomd_xml.lines() {
            let line = line.trim();
            if line.contains("type=\"primary\"") {
                in_primary_data = true;
            } else if in_primary_data && line.contains("<location href=") {
                // Look for href attribute
                if let Some(href_start) = line.find("href=\"") {
                    if let Some(href_end) = line[href_start + 6..].find("\"") {
                        let filename = &line[href_start + 6..href_start + 6 + href_end];
                        return Ok(filename.to_string());
                    }
                }
            } else if in_primary_data && line.contains("</data>") {
                break;
            }
        }
        err!("Could not find primary.xml.gz filename in repomd.xml")
    }

    fn parse_primary_xml(&self, xml: &str) -> Result<Vec<YumPackageInfo>, String> {
        let mut packages = Vec::new();
        
        // Start animated progress bar
        let bar_width = 20;
        let mut position = 0i32;
        let mut direction = 1i32;
        let mut frame_counter = 0;
        
        // Split by package blocks
        let package_blocks: Vec<&str> = xml.split("<package type=\"rpm\">").collect();
        
        for block in package_blocks.iter().skip(1) { // Skip first empty block
            if let Some(package_end) = block.find("</package>") {
                let package_xml = &block[..package_end];
                if let Some(package_info) = self.parse_single_package(package_xml)? {
                    packages.push(package_info);
                    
                    // Update animation every package
                    frame_counter += 1;
                    if frame_counter % 5 == 0 {
                        let bar = self.generate_bar(position, bar_width);
                        print!("\rParsing packages... [{}] {} packages", bar, packages.len());
                        std::io::Write::flush(&mut std::io::stdout()).ok();
                        
                        // Update position with ping-pong effect
                        position += direction;
                        if position >= bar_width as i32 - 1 {
                            direction = -1;
                        } else if position <= 0 {
                            direction = 1;
                        }
                    }
                }
            }
        }
        
        Ok(packages)
    }
    
    fn generate_bar(&self, position: i32, width: usize) -> String {
        let pos = position.max(0).min((width - 1) as i32) as usize;
        let mut bar = vec![' '; width];
        bar[pos] = '#';
        bar.iter().collect()
    }
    
    fn parse_single_package(&self, package_xml: &str) -> Result<Option<YumPackageInfo>, String> {
        // Simple regex-based parsing
        let mut name = None;
        let mut version = None;
        let mut release = None;
        let mut arch = None;
        let mut summary = None;
        let mut description = None;
        let mut location = None;
        let mut dependencies = Vec::new();
        
        for line in package_xml.lines() {
            let line = line.trim();
            
            // Extract package info
            if line.starts_with("<name>") && line.ends_with("</name>") {
                name = Some(line[6..line.len()-7].to_string());
            } else if line.starts_with("<arch>") && line.ends_with("</arch>") {
                arch = Some(line[6..line.len()-7].to_string());
            } else if line.starts_with("<summary>") && line.ends_with("</summary>") {
                summary = Some(line[9..line.len()-10].to_string());
            } else if line.starts_with("<description>") && line.ends_with("</description>") {
                description = Some(line[12..line.len()-13].to_string());
            } else if line.contains("href=\"") {
                if let Some(start) = line.find("href=\"") {
                    if let Some(end) = line[start+6..].find("\"") {
                        location = Some(line[start+6..start+6+end].to_string());
                    }
                }
            } else if line.starts_with("<version ") {
                if let Some(ver_start) = line.find("ver=\"") {
                    if let Some(ver_end) = line[ver_start+5..].find("\"") {
                        version = Some(line[ver_start+5..ver_start+5+ver_end].to_string());
                    }
                }
                if let Some(rel_start) = line.find("rel=\"") {
                    if let Some(rel_end) = line[rel_start+5..].find("\"") {
                        release = Some(line[rel_start+5..rel_start+5+rel_end].to_string());
                    }
                }
            } else if line.contains("name=") && line.contains("rpm:entry") {
                // Extract dependency from attributes: <rpm:entry name="pkgname"/>
                if let Some(start) = line.find("name=\"") {
                    if let Some(end) = line[start+6..].find("\"") {
                        let dep_name = line[start+6..start+6+end].to_string();
                        if !dep_name.is_empty() 
                            && !dep_name.contains("rpmlib")
                            && !dep_name.contains("(")
                            && !dep_name.contains(")")
                        {
                            // Extract just the package name before any operators
                            let clean_name = dep_name.split_whitespace().next().unwrap_or(&dep_name).to_string();
                            if !clean_name.is_empty() && !dependencies.contains(&clean_name) {
                                dependencies.push(clean_name);
                            }
                        }
                    }
                }
            }
        }
        
        if let (Some(name), Some(version), Some(release), Some(arch)) = (name, version, release, arch) {
            let full_version = format!("{}-{}", version, release);
            let url = location.map(|loc| format!("{}/{}", self.base_url, loc)).unwrap_or_default();
            
            // Silently parse dependencies without spamming output
            
            Ok(Some(YumPackageInfo {
                name,
                version: full_version,
                description: description.unwrap_or(summary.unwrap_or_default()),
                size: 0,
                url,
                dependencies,
                architecture: arch,
                release,
                epoch: "0".to_string(),
            }))
        } else {
            Ok(None)
        }
    }

    fn decompress_gzip_bytes(&self, bytes: &[u8]) -> Result<String, String> {
        use flate2::read::GzDecoder;
        use std::io::Read;
        
        let mut decoder = GzDecoder::new(bytes);
        let mut decompressed = String::new();
        decoder.read_to_string(&mut decompressed)
            .map_err(|e| format!("Failed to decompress gzip: {}", e))?;
        
        Ok(decompressed)
    }

    #[allow(dead_code)]
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
pub struct YumPackageInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub size: u64,
    pub url: String,
    pub dependencies: Vec<String>,
    pub architecture: String,
    pub release: String,
    pub epoch: String,
}

pub async fn test_yum_connection(origin: &OriginKind) -> Result<bool, String> {
    let client = match YumRepositoryClient::from_origin(origin) {
        Some(client) => client,
        None => return Ok(false),
    };

    // Try to list packages to test connection
    match client.list_packages().await {
        Ok(_) => Ok(true),
        Err(e) => {
            println!("Yum repository connection test failed: {}", e);
            Ok(false)
        }
    }
}
