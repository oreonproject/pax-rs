use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use utils::{err, get_metadata_dir};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryCredentials {
    pub repository_url: String,
    pub auth_type: AuthType,
    pub credentials: AuthCredentials,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    pub last_used: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuthType {
    Basic,
    Bearer,
    ApiKey,
    OAuth2,
    ClientCertificate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AuthCredentials {
    Basic { username: String, password: String },
    Bearer { token: String },
    ApiKey { key: String, header: Option<String> },
    OAuth2 { 
        client_id: String, 
        client_secret: String, 
        access_token: Option<String>,
        refresh_token: Option<String>,
    },
    ClientCertificate { 
        cert_path: PathBuf, 
        key_path: PathBuf,
        password: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositoryAuthConfig {
    pub repository_url: String,
    pub verify_ssl: bool,
    pub timeout_seconds: u64,
    pub retry_count: u32,
    pub custom_headers: HashMap<String, String>,
}

pub struct RepositoryAuthManager {
    credentials: HashMap<String, RepositoryCredentials>,
    configs: HashMap<String, RepositoryAuthConfig>,
    master_password: Option<String>,
}

impl RepositoryAuthManager {
    pub fn new() -> Self {
        Self {
            credentials: HashMap::new(),
            configs: HashMap::new(),
            master_password: None,
        }
    }

    pub fn set_master_password(&mut self, password: String) {
        self.master_password = Some(password);
    }

    pub fn add_credentials(
        &mut self,
        repository_url: String,
        auth_type: AuthType,
        credentials: AuthCredentials,
        expires_at: Option<u64>,
    ) -> Result<(), String> {
        let creds = RepositoryCredentials {
            repository_url: repository_url.clone(),
            auth_type,
            credentials,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            expires_at,
            last_used: None,
        };

        self.credentials.insert(repository_url, creds);
        self.save_credentials()?;
        Ok(())
    }

    pub fn add_config(&mut self, config: RepositoryAuthConfig) -> Result<(), String> {
        self.configs.insert(config.repository_url.clone(), config);
        self.save_configs()?;
        Ok(())
    }

    pub fn get_credentials(&mut self, repository_url: &str) -> Option<&mut RepositoryCredentials> {
        if let Some(creds) = self.credentials.get_mut(repository_url) {
            // Update last used timestamp
            creds.last_used = Some(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            );
            Some(creds)
        } else {
            None
        }
    }

    pub fn get_config(&self, repository_url: &str) -> Option<&RepositoryAuthConfig> {
        self.configs.get(repository_url)
    }

    pub fn remove_credentials(&mut self, repository_url: &str) -> Result<(), String> {
        if self.credentials.remove(repository_url).is_some() {
            self.save_credentials()?;
            println!("Removed credentials for repository: {}", repository_url);
        } else {
            return err!("No credentials found for repository: {}", repository_url);
        }
        Ok(())
    }

    pub fn remove_config(&mut self, repository_url: &str) -> Result<(), String> {
        if self.configs.remove(repository_url).is_some() {
            self.save_configs()?;
            println!("Removed config for repository: {}", repository_url);
        } else {
            return err!("No config found for repository: {}", repository_url);
        }
        Ok(())
    }

    pub fn list_repositories(&self) -> Vec<&String> {
        let mut repos: Vec<&String> = self.credentials.keys().collect();
        repos.sort();
        repos
    }

    pub fn cleanup_expired_credentials(&mut self) -> Result<(), String> {
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let expired_repos: Vec<String> = self.credentials
            .iter()
            .filter(|(_, creds)| {
                creds.expires_at.map_or(false, |expires| expires < current_time)
            })
            .map(|(url, _)| url.clone())
            .collect();

        for repo_url in expired_repos {
            self.remove_credentials(&repo_url)?;
        }

        Ok(())
    }

    pub fn authenticate_request(
        &mut self,
        repository_url: &str,
        mut request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder, String> {
        if let Some(creds) = self.get_credentials(repository_url) {
            request = match &creds.auth_type {
                AuthType::Basic => {
                    if let AuthCredentials::Basic { username, password } = &creds.credentials {
                        request.basic_auth(username, Some(password))
                    } else {
                        return err!("Invalid credentials type for Basic auth");
                    }
                }
                AuthType::Bearer => {
                    if let AuthCredentials::Bearer { token } = &creds.credentials {
                        request.bearer_auth(token)
                    } else {
                        return err!("Invalid credentials type for Bearer auth");
                    }
                }
                AuthType::ApiKey => {
                    if let AuthCredentials::ApiKey { key, header } = &creds.credentials {
                        let header_name = header.as_deref().unwrap_or("X-API-Key");
                        request.header(header_name, key)
                    } else {
                        return err!("Invalid credentials type for API Key auth");
                    }
                }
                AuthType::OAuth2 => {
                    if let AuthCredentials::OAuth2 { access_token, .. } = &creds.credentials {
                        if let Some(token) = access_token {
                            request.bearer_auth(token)
                        } else {
                            return err!("No access token available for OAuth2");
                        }
                    } else {
                        return err!("Invalid credentials type for OAuth2");
                    }
                }
                AuthType::ClientCertificate => {
                    // Client certificate authentication would be handled differently
                    // This is a placeholder for now
                    request
                }
            };
        }

        // Apply repository-specific config
        if let Some(config) = self.get_config(repository_url) {
            // Add custom headers
            for (key, value) in &config.custom_headers {
                request = request.header(key, value);
            }
        }

        Ok(request)
    }

    pub fn refresh_oauth2_token(&mut self, repository_url: &str) -> Result<(), String> {
        if let Some(creds) = self.credentials.get_mut(repository_url) {
            if let AuthCredentials::OAuth2 { 
                client_id: _, 
                client_secret: _, 
                refresh_token,
                access_token,
                ..
            } = &mut creds.credentials {
                
                if let Some(refresh_token) = refresh_token {
                    // This would make an actual OAuth2 refresh request
                    // For now, we'll just simulate it
                    println!("Refreshing OAuth2 token for {}", repository_url);
                    
                    // In a real implementation, you would:
                    // 1. Make a POST request to the OAuth2 token endpoint
                    // 2. Parse the response to get new access_token and refresh_token
                    // 3. Update the credentials
                    
                    *access_token = Some("new_access_token".to_string());
                    *refresh_token = "new_refresh_token".to_string();
                    
                    self.save_credentials()?;
                } else {
                    return err!("No refresh token available for OAuth2");
                }
            } else {
                return err!("Repository {} does not use OAuth2 authentication", repository_url);
            }
        } else {
            return err!("No credentials found for repository: {}", repository_url);
        }

        Ok(())
    }

    fn save_credentials(&self) -> Result<(), String> {
        let mut creds_path = get_metadata_dir()?;
        creds_path.push("repository_credentials.yaml");

        let mut file = File::create(&creds_path)
            .map_err(|_| "Failed to create credentials file")?;

        // Encrypt credentials if master password is set
        let data = if let Some(_master_password) = &self.master_password {
            // In a real implementation, you would encrypt the credentials here
            serde_norway::to_string(&self.credentials)
                .map_err(|_| "Failed to serialize credentials")?
        } else {
            serde_norway::to_string(&self.credentials)
                .map_err(|_| "Failed to serialize credentials")?
        };

        file.write_all(data.as_bytes())
            .map_err(|_| "Failed to write credentials file")?;

        Ok(())
    }

    fn save_configs(&self) -> Result<(), String> {
        let mut configs_path = get_metadata_dir()?;
        configs_path.push("repository_configs.yaml");

        let mut file = File::create(&configs_path)
            .map_err(|_| "Failed to create configs file")?;

        let data = serde_norway::to_string(&self.configs)
            .map_err(|_| "Failed to serialize configs")?;

        file.write_all(data.as_bytes())
            .map_err(|_| "Failed to write configs file")?;

        Ok(())
    }

    pub fn load_credentials(&mut self) -> Result<(), String> {
        let mut creds_path = get_metadata_dir()?;
        creds_path.push("repository_credentials.yaml");

        if creds_path.exists() {
            let mut file = File::open(&creds_path)
                .map_err(|_| "Failed to open credentials file")?;

            let mut contents = String::new();
            file.read_to_string(&mut contents)
                .map_err(|_| "Failed to read credentials file")?;

            // Decrypt credentials if master password is set
            let data = if let Some(_master_password) = &self.master_password {
                // In a real implementation, you would decrypt the credentials here
                contents
            } else {
                contents
            };

            self.credentials = serde_norway::from_str(&data)
                .map_err(|_| "Failed to parse credentials file")?;
        }

        Ok(())
    }

    pub fn load_configs(&mut self) -> Result<(), String> {
        let mut configs_path = get_metadata_dir()?;
        configs_path.push("repository_configs.yaml");

        if configs_path.exists() {
            let mut file = File::open(&configs_path)
                .map_err(|_| "Failed to open configs file")?;

            let mut contents = String::new();
            file.read_to_string(&mut contents)
                .map_err(|_| "Failed to read configs file")?;

            self.configs = serde_norway::from_str(&contents)
                .map_err(|_| "Failed to parse configs file")?;
        }

        Ok(())
    }

    pub fn load_all(&mut self) -> Result<(), String> {
        self.load_credentials()?;
        self.load_configs()?;
        Ok(())
    }

    pub fn export_credentials(&self, path: &PathBuf) -> Result<(), String> {
        let mut file = File::create(path)
            .map_err(|_| format!("Failed to create export file {}", path.display()))?;

        let data = serde_norway::to_string(&self.credentials)
            .map_err(|_| "Failed to serialize credentials for export")?;

        file.write_all(data.as_bytes())
            .map_err(|_| format!("Failed to write export file {}", path.display()))?;

        Ok(())
    }

    pub fn import_credentials(&mut self, path: &PathBuf) -> Result<(), String> {
        let mut file = File::open(path)
            .map_err(|_| format!("Failed to open import file {}", path.display()))?;

        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .map_err(|_| format!("Failed to read import file {}", path.display()))?;

        let imported_creds: HashMap<String, RepositoryCredentials> = serde_norway::from_str(&contents)
            .map_err(|_| "Failed to parse imported credentials")?;

        for (url, creds) in imported_creds {
            self.credentials.insert(url, creds);
        }

        self.save_credentials()?;
        Ok(())
    }
}

impl Default for RepositoryAuthManager {
    fn default() -> Self {
        Self::new()
    }
}
