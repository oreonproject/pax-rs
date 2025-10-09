use std::{
    fs::{DirBuilder, File},
    io::{Read, Write},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

#[derive(PartialEq, Serialize, Deserialize, Debug, Clone)]
pub struct SettingsYaml {
    pub sources: Vec<String>,
    #[serde(default = "default_distro_version")]
    pub distro_version: String,
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

fn default_distro_version() -> String {
    "oreon-11".to_string()
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
    // Try to auto-initialize from existing settings files first
    if let Ok(settings) = auto_initialize_settings() {
        return Ok(settings);
    }

    // If no settings exist, create a new one and initialize it
    // NOTE: Only create if file doesn't exist (never overwrite existing)
    let file_path = get_settings_path()?;
    
    if file_path.exists() {
        // File exists but couldn't be parsed - return error
        return Err(String::from("Settings file exists but is invalid. Please check /etc/pax/settings.yaml"));
    }

    let settings = SettingsYaml {
        sources: vec!["http://localhost:8080".to_string()],
        distro_version: default_distro_version(),
        db_path: default_db_path(),
        store_path: default_store_path(),
        cache_path: default_cache_path(),
        links_path: default_links_path(),
        parallel_downloads: default_parallel_downloads(),
        verify_signatures: default_verify_signatures(),
    };

    let settings_content = match serde_norway::to_string(&settings) {
        Ok(content) => content,
        Err(_) => return Err(String::from("Failed to serialize default settings!")),
    };

    let mut file = File::create(&file_path)
        .map_err(|_| String::from("Failed to create settings file!"))?;

    if file.write_all(settings_content.as_bytes()).is_err() {
        return Err(String::from("Failed to write default settings!"));
    }

    Ok(settings)
}

pub fn get_settings_or_local() -> Result<SettingsYaml, String> {
    // Try to get settings normally first
    if let Ok(settings) = get_settings() {
        return Ok(settings);
    }

    // If no settings exist, return local-only settings
    Ok(SettingsYaml {
        sources: Vec::new(),
        distro_version: default_distro_version(),
        db_path: default_db_path(),
        store_path: default_store_path(),
        cache_path: default_cache_path(),
        links_path: default_links_path(),
        parallel_downloads: default_parallel_downloads(),
        verify_signatures: default_verify_signatures(),
    })
}

fn auto_initialize_settings() -> Result<SettingsYaml, String> {
    // Try to read existing settings.yml from multiple locations
    // Check user location first (for development), then system location
    let settings_locations = [
        "/tmp/pax/settings.yaml",
        "/etc/pax/settings.yaml",
    ];

    for location in &settings_locations {
        if let Ok(mut file) = File::open(location) {
            let mut contents = String::new();
            if file.read_to_string(&mut contents).is_ok() && !contents.trim().is_empty() {
                if let Ok(settings) = serde_norway::from_str::<SettingsYaml>(&contents) {
                    // Only return settings if they have valid sources AND other required fields
                    if !settings.sources.is_empty() && !settings.db_path.is_empty() && !settings.store_path.is_empty() {
                        return Ok(settings);
                    }
                } else {
                    // If YAML parsing fails, try the next location
                    continue;
                }
            }
        }
    }

    // If no settings.yml found, return error - user should run 'pax init'

    Err(String::from("No repository endpoints found. Please run 'pax init' to initialize pax with default settings"))
}

pub fn set_settings(settings: SettingsYaml) -> Result<(), String> {
    // Get the settings file path - DO NOT create if doesn't exist
    let file_path = get_settings_path()?;
    
    let settings_content = match serde_norway::to_string(&settings) {
        Ok(settings) => settings,
        Err(_) => return Err(String::from("Failed to parse SettingsYaml to string!")),
    };
    
    // Write to existing file or create new one
    match std::fs::write(&file_path, settings_content.as_bytes()) {
        Ok(_) => Ok(()),
        Err(_) => Err(String::from("Failed to write to settings file!")),
    }
}

// Get the settings file path without creating it
fn get_settings_path() -> Result<PathBuf, String> {
    // Try system path first
    let system_dir = PathBuf::from("/etc/pax");
    let system_path = system_dir.join("settings.yaml");
    
    // Check if system path exists or can be created
    if system_path.exists() {
        return Ok(system_path);
    }
    
    if system_dir.exists() || DirBuilder::new().create(&system_dir).is_ok() {
        return Ok(system_path);
    }

    // Fall back to user path for development
    let user_dir = PathBuf::from("/tmp/pax");
    let user_path = user_dir.join("settings.yaml");
    
    if user_path.exists() {
        return Ok(user_path);
    }
    
    if user_dir.exists() || DirBuilder::new().create(&user_dir).is_ok() {
        return Ok(user_path);
    }

    Err(String::from("Failed to access settings directory!"))
}
