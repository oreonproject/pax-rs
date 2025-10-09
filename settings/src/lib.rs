use std::{
    fs::File,
    io::{Read, Write},
    path::PathBuf,
    thread::sleep,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use utils::{PostAction, err, get_dir, is_root};

#[derive(PartialEq, Serialize, Deserialize, Debug)]
pub struct SettingsYaml {
    pub locked: bool,
    pub version: String,
    pub sources: Vec<String>,
}

impl SettingsYaml {
    pub fn new() -> Self {
        SettingsYaml {
            locked: false,
            version: env!("SETTINGS_YAML_VERSION").to_string(),
            sources: Vec::new(),
        }
    }
    pub fn set_settings(self) -> Result<(), String> {
        let mut file = match File::create(affirm_path()?) {
            Ok(file) => file,
            Err(_) => return err!("Failed to open SettingsYaml as WO!"),
        };
        let settings = match serde_norway::to_string(&self) {
            Ok(settings) => settings,
            Err(_) => return err!("Failed to parse SettingsYaml to string!"),
        };
        match file.write_all(settings.as_bytes()) {
            Ok(_) => Ok(()),
            Err(_) => err!("Failed to write to file!"),
        }
    }
    pub fn get_settings() -> Result<Self, String> {
        let mut file = match File::open(affirm_path()?) {
            Ok(file) => file,
            Err(_) => return err!("Failed to open SettingsYaml as RO!"),
        };
        let mut sources = String::new();
        if file.read_to_string(&mut sources).is_err() {
            return err!("Failed to read file!");
        };
        let sources = match serde_norway::from_str(&sources) {
            Ok(settings_yaml) => settings_yaml,
            Err(_) => return err!("Failed to parse data into SettingsYaml!"),
        };
        Ok(sources)
    }
}

impl Default for SettingsYaml {
    fn default() -> Self {
        Self::new()
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
                        err!("Failed to write to file!")
                    }
                } else {
                    err!("Failed to serialize settings!")
                }
            }
            Err(_) => err!("Failed to create settings file!"),
        }
    } else if path.is_file() {
        if File::open(&path).is_ok() {
            Ok(path)
        } else {
            err!("Failed to read settings file!")
        }
    } else {
        err!("Settings file is of unexpected type!")
    }
}

pub fn acquire_lock() -> Result<Option<PostAction>, String> {
    if !is_root() {
        return Ok(Some(PostAction::Elevate));
    }
    let mut settings = SettingsYaml::get_settings()?;
    loop {
        if settings.locked {
            for i in 0..100 {
                print!(
                    "\x1B[2K\r\x1B[91mAwaiting program lock. Retrying in {:.2}s...\x1B[0m",
                    (500 - i) as f32 / 100f32
                );
                let _ = std::io::stdout().flush();
                sleep(Duration::from_millis(10));
            }
            for i in 0..100 {
                print!(
                    "\x1B[2K\r\x1B[93mAwaiting program lock. Retrying in {:.2}s\x1B[0m...",
                    (400 - i) as f32 / 100f32
                );
                let _ = std::io::stdout().flush();
                sleep(Duration::from_millis(10));
            }
            for i in 0..100 {
                print!(
                    "\x1B[2K\r\x1B[95mAwaiting program lock. Retrying in {:.2}s\x1B[0m...",
                    (300 - i) as f32 / 100f32
                );
                let _ = std::io::stdout().flush();
                sleep(Duration::from_millis(10));
            }
            for i in 0..100 {
                print!(
                    "\x1B[2K\r\x1B[94mAwaiting program lock. Retrying in {:.2}s\x1B[0m...",
                    (200 - i) as f32 / 100f32
                );
                let _ = std::io::stdout().flush();
                sleep(Duration::from_millis(10));
            }
            for i in 0..100 {
                print!(
                    "\x1B[2K\r\x1B[92mAwaiting program lock. Retrying in {:.2}s\x1B[0m...",
                    (100 - i) as f32 / 100f32
                );
                let _ = std::io::stdout().flush();
                sleep(Duration::from_millis(10));
            }
            println!("\x1B[2K\r\x1B[92mAwaiting program lock. Retrying now\x1B[0m...");
            settings = SettingsYaml::get_settings()?;
        } else {
            break;
        }
    }
    if settings.sources.is_empty() {
        return Ok(Some(PostAction::PullSources));
    }
    settings.locked = true;
    settings.set_settings()?;
    Ok(None)
}

pub fn remove_lock() -> Result<(), String> {
    let mut settings = SettingsYaml::get_settings()?;
    settings.locked = false;
    settings.set_settings()
}
