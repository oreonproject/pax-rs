use reqwest::Client;
use serde::{Deserialize, Serialize};
use settings::OriginKind;
use utils::err;

#[derive(Debug, Clone)]
pub struct CloudflareR2Client {
    bucket: String,
    account_id: String,
    #[allow(dead_code)]
    access_key_id: Option<String>,
    #[allow(dead_code)]
    secret_access_key: Option<String>,
    region: Option<String>,
    client: Client,
}

impl CloudflareR2Client {
    pub fn new(
        bucket: String,
        account_id: String,
        access_key_id: Option<String>,
        secret_access_key: Option<String>,
        region: Option<String>,
    ) -> Self {
        Self {
            bucket,
            account_id,
            access_key_id,
            secret_access_key,
            region,
            client: Client::new(),
        }
    }

    pub fn from_origin(origin: &OriginKind) -> Option<Self> {
        match origin {
            OriginKind::CloudflareR2 {
                bucket,
                account_id,
                access_key_id,
                secret_access_key,
                region,
            } => Some(Self::new(
                bucket.clone(),
                account_id.clone(),
                access_key_id.clone(),
                secret_access_key.clone(),
                region.clone(),
            )),
            _ => None,
        }
    }

    fn get_endpoint(&self) -> String {
        let _region = self.region.as_deref().unwrap_or("auto");
        format!("https://{}.{}.r2.cloudflarestorage.com", self.bucket, self.account_id)
    }

    fn get_public_endpoint(&self) -> String {
        format!("https://pub-{}.r2.dev", self.bucket)
    }

    pub async fn list_packages(&self) -> Result<Vec<PackageInfo>, String> {
        let endpoint = format!("{}/packages/", self.get_endpoint());
        
        let response = self.client
            .get(&endpoint)
            .send()
            .await
            .map_err(|e| format!("Failed to list packages from R2: {}", e))?;

        if !response.status().is_success() {
            return err!("Failed to list packages: {}", response.status());
        }

        let text = response.text().await
            .map_err(|e| format!("Failed to read response: {}", e))?;

        // Parse the response - this could be JSON, XML, or HTML depending on R2 configuration
        self.parse_package_list(&text)
    }

    pub async fn get_package(&self, package_name: &str, version: Option<&str>) -> Result<PackageInfo, String> {
        let version = version.unwrap_or("latest");
        let endpoint = format!("{}/packages/{}/{}.pax", self.get_public_endpoint(), package_name, version);
        
        let response = self.client
            .head(&endpoint)
            .send()
            .await
            .map_err(|e| format!("Failed to check package {}: {}", package_name, e))?;

        if !response.status().is_success() {
            return err!("Package {} version {} not found", package_name, version);
        }

        // Extract metadata from headers or make another request for metadata
        let size = response.headers()
            .get("content-length")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        Ok(PackageInfo {
            name: package_name.to_string(),
            version: version.to_string(),
            description: format!("Package {} from Cloudflare R2", package_name),
            size,
            url: endpoint,
            dependencies: Vec::new(),
        })
    }

    pub async fn download_package(&self, package_info: &PackageInfo) -> Result<Vec<u8>, String> {
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

    fn parse_package_list(&self, response: &str) -> Result<Vec<PackageInfo>, String> {
        // Try to parse as JSON first
        if let Ok(packages) = serde_json::from_str::<Vec<PackageInfo>>(response) {
            return Ok(packages);
        }

        // Try to parse as XML (S3-compatible format)
        if let Ok(packages) = self.parse_s3_xml(response) {
            return Ok(packages);
        }

        // Try to parse as HTML directory listing
        if let Ok(packages) = self.parse_html_listing(response) {
            return Ok(packages);
        }

        err!("Failed to parse package list from R2 response")
    }

    fn parse_s3_xml(&self, xml: &str) -> Result<Vec<PackageInfo>, String> {
        // Parse S3-compatible XML response
        // This is a simplified parser - in production you'd want a proper XML parser
        let mut packages = Vec::new();
        
        // Look for <Key> elements that end with .pax
        for line in xml.lines() {
            if line.contains("<Key>") && line.contains(".pax</Key>") {
                if let Some(start) = line.find("<Key>") {
                    if let Some(end) = line.find("</Key>") {
                        let key = &line[start + 5..end];
                        if let Some(package_info) = self.parse_package_key(key) {
                            packages.push(package_info);
                        }
                    }
                }
            }
        }

        Ok(packages)
    }

    fn parse_html_listing(&self, html: &str) -> Result<Vec<PackageInfo>, String> {
        // Parse HTML directory listing
        let mut packages = Vec::new();
        
        for line in html.lines() {
            if line.contains(".pax") {
                // Extract filename from HTML
                if let Some(start) = line.find("href=\"") {
                    if let Some(end) = line.find("\"") {
                        let filename = &line[start + 6..end];
                        if let Some(package_info) = self.parse_package_key(filename) {
                            packages.push(package_info);
                        }
                    }
                }
            }
        }

        Ok(packages)
    }

    fn parse_package_key(&self, key: &str) -> Option<PackageInfo> {
        // Parse key like "packages/zlib/1.3.1/zlib-1.3.1-x86_64v3.pax"
        let parts: Vec<&str> = key.split('/').collect();
        if parts.len() >= 3 && parts[0] == "packages" {
            let name = parts[1].to_string();
            let version = parts[2].to_string();
            let _filename = parts.last()?.to_string();
            
            Some(PackageInfo {
                name: name.clone(),
                version,
                description: format!("Package {} from Cloudflare R2", name),
                size: 0, // Will be filled in when we actually fetch the package
                url: format!("{}/{}", self.get_public_endpoint(), key),
                dependencies: Vec::new(),
            })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub size: u64,
    pub url: String,
    pub dependencies: Vec<String>,
}

pub async fn test_r2_connection(origin: &OriginKind) -> Result<bool, String> {
    let client = match CloudflareR2Client::from_origin(origin) {
        Some(client) => client,
        None => return Ok(false),
    };

    // Try to list packages to test connection
    match client.list_packages().await {
        Ok(_) => Ok(true),
        Err(e) => {
            println!("R2 connection test failed: {}", e);
            Ok(false)
        }
    }
}
