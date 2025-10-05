use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};
use utils::get_dir;

#[derive(PartialEq, Serialize, Deserialize, Debug)]
pub struct SettingsYaml {
    pub version: String,
    pub sources: Vec<String>,
}

impl SettingsYaml {
    pub fn new() -> Self {
        SettingsYaml {
            version: env!("SETTINGS_YAML_VERSION").to_string(),
            sources: Vec::new(),
        }
    }
}

impl Default for SettingsYaml {
    fn default() -> Self {
        Self::new()
    }
}

pub fn get_settings() -> Result<SettingsYaml, String> {
    let mut file = match File::open(affirm_path()?) {
        Ok(file) => file,
        Err(_) => return Err(String::from("Failed to open SettingsYaml as RO!")),
    };
    let mut sources = String::new();
    if file.read_to_string(&mut sources).is_err() {
        return Err(String::from("Failed to read file!"));
    };
    let sources = match serde_norway::from_str(&sources) {
        Ok(settings_yaml) => settings_yaml,
        Err(_) => return Err(String::from("Failed to parse data into SettingsYaml!")),
    };
    Ok(sources)
}

pub fn set_settings(settings: SettingsYaml) -> Result<(), String> {
    let mut file = match File::create(affirm_path()?) {
        Ok(file) => file,
        Err(_) => return Err(String::from("Failed to open SettingsYaml as WO!")),
    };
    let settings = match serde_norway::to_string(&settings) {
        Ok(settings) => settings,
        Err(_) => return Err(String::from("Failed to parse SettingsYaml to string!")),
    };
    match file.write_all(settings.as_bytes()) {
        Ok(_) => Ok(()),
        Err(_) => Err(String::from("Failed to write to file!")),
    }
}

fn affirm_path() -> Result<PathBuf, String> {
    let mut path = get_dir()?;
    path.push("settings.yaml");
    if !path.exists() {
        match File::create(&path) {
            Ok(mut file) => {
                if let Ok(new_settings) = serde_norway::to_string(&SettingsYaml::new()) {
                    if file.write_all(new_settings.as_bytes()).is_ok() {
                        Ok(path)
                    } else {
                        Err(String::from("Failed to write to file!"))
                    }
                } else {
                    Err(String::from("Failed to serialize settings!"))
                }
            }
            Err(_) => Err(String::from("Failed to create settings file!")),
        }
    } else if path.is_file() {
        if File::open(&path).is_ok() {
            Ok(path)
        } else {
            Err(String::from("Failed to read settings file!"))
        }
    } else {
        Err(String::from("Settings file is of unexpected type!"))
    }
}
