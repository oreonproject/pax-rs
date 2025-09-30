use std::{
    fs::{DirBuilder, File},
    io::{Read, Write},
    path::PathBuf,
};

use serde::{Deserialize, Serialize};

#[derive(PartialEq, Serialize, Deserialize, Debug)]
pub struct SettingsYaml {
    pub sources: Vec<String>,
}

pub fn get_settings() -> Result<SettingsYaml, String> {
    let mut file = affirm_path()?;
    let mut sources = String::new();
    match file.read_to_string(&mut sources) {
        Ok(file) => file,
        Err(_) => return Err(String::from("Failed to read file!")),
    };
    let sources = match serde_norway::from_str(&sources) {
        Ok(settings_yaml) => settings_yaml,
        Err(_) => return Err(String::from("Failed to parse data into SettingsYaml!")),
    };
    Ok(sources)
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
