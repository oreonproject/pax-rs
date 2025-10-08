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
    // Try to auto-initialize from existing settings files first
    if let Ok(settings) = auto_initialize_settings() {
        return Ok(settings);
    }

    // If no settings exist, create a new one and initialize it
    let mut file = affirm_path()?;
    let settings = SettingsYaml {
        sources: vec!["http://localhost:8080".to_string()],
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
    // Try system path first
    let mut path = PathBuf::from("/etc/pax");
    if path.exists() || DirBuilder::new().create(&path).is_ok() {
        path.push("settings.yaml");
        if path.is_file() || !path.exists() {
            if let Ok(file) = File::create(&path) {
                return Ok(file);
            }
        }
    }

    // Fall back to user path for development
    let mut user_path = PathBuf::from("/tmp/pax");
    if user_path.exists() || DirBuilder::new().create(&user_path).is_ok() {
        user_path.push("settings.yaml");
        if let Ok(file) = File::create(&user_path) {
            return Ok(file);
        }
    }

    Err(String::from("Failed to create settings file in any location!"))
}
