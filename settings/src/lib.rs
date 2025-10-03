use std::{
    fs::{DirBuilder, File},
    io::{Read, Write},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

#[derive(PartialEq, Serialize, Deserialize, Debug, Clone)]
pub struct SettingsYaml {
    pub sources: Vec<String>,
    #[serde(default = "default_db_path")]
    pub db_path: String,
    #[serde(default = "default_store_path")]
    pub store_path: String,
    #[serde(default = "default_cache_path")]
    pub cache_path: String,
    #[serde(default = "default_links_path")]
    pub links_path: String,
    #[serde(default = "default_parallel_downloads")]
    pub parallel_downloads: usize,
    #[serde(default = "default_verify_signatures")]
    pub verify_signatures: bool,
}

fn default_db_path() -> String {
    "/opt/pax/db/pax.db".to_string()
}

fn default_store_path() -> String {
    "/opt/pax/store".to_string()
}

fn default_cache_path() -> String {
    "/var/cache/pax".to_string()
}

fn default_links_path() -> String {
    "/opt/pax/links".to_string()
}

fn default_parallel_downloads() -> usize {
    3
}

fn default_verify_signatures() -> bool {
    true
}

pub fn get_settings() -> Result<SettingsYaml, String> {
    // Try to read existing settings
    if let Ok(mut file) = affirm_path() {
        let mut contents = String::new();
        if file.read_to_string(&mut contents).is_ok() {
            if let Ok(settings) = serde_norway::from_str(&contents) {
                return Ok(settings);
            }
        }
    }
    
    // If settings don't exist, try to auto-initialize from system endpoints
    auto_initialize_settings()
}

fn auto_initialize_settings() -> Result<SettingsYaml, String> {
    // Look for system endpoints file
    let endpoints_locations = [
        "/etc/pax/endpoints.txt",
        "/usr/share/pax/endpoints.txt",
        "/etc/oreon/pax-endpoints.txt",
    ];
    
    for location in &endpoints_locations {
        if let Ok(contents) = std::fs::read_to_string(location) {
            let sources: Vec<String> = contents
                .lines()
                .filter(|line| !line.trim().is_empty() && !line.trim().starts_with('#'))
                .map(|line| line.trim().to_string())
                .collect();
            
            if !sources.is_empty() {
                let settings = SettingsYaml {
                    sources,
                    db_path: default_db_path(),
                    store_path: default_store_path(),
                    cache_path: default_cache_path(),
                    links_path: default_links_path(),
                    parallel_downloads: default_parallel_downloads(),
                    verify_signatures: default_verify_signatures(),
                };
                
                // Try to save settings for next time
                let _ = set_settings(settings.clone());
                
                return Ok(settings);
            }
        }
    }
    
    Err(String::from("No repository endpoints found. Please create /etc/pax/endpoints.txt with repository URLs"))
}

pub fn set_settings(settings: SettingsYaml) -> Result<(), String> {
    let mut file = affirm_path()?;
    let settings = match serde_norway::to_string(&settings) {
        Ok(settings) => settings,
        Err(_) => return Err(String::from("Failed to parse SettingsYaml to string!")),
    };
    match file.write_all(settings.as_bytes()) {
        Ok(_) => Ok(()),
        Err(_) => Err(String::from("Failed to write to file!")),
    }
}

fn affirm_path() -> Result<File, String> {
    let mut path = PathBuf::from("/etc/pax");
    if !path.exists() && DirBuilder::new().create(&path).is_err() {
        return Err(String::from("Failed to create pax directory!"));
    }
    path.push("settings.yaml");
    if path.is_file() || !path.exists() {
        let file = match File::create(path) {
            Ok(file) => file,
            Err(_) => return Err(String::from("Failed to create settings file!")),
        };
        Ok(file)
    } else {
        Err(String::from("Settings file is of unexpected type!"))
    }
}
