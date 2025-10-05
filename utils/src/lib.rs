use std::{fs::DirBuilder, path::PathBuf, process::Command};

use nix::unistd;

pub fn get_dir() -> Result<PathBuf, String> {
    let path = PathBuf::from("/etc/pax");
    if !path.exists() && DirBuilder::new().create(&path).is_err() {
        Err(String::from("Failed to create pax directory!"))
    } else {
        Ok(path)
    }
}

pub fn get_metadata_dir() -> Result<PathBuf, String> {
    let mut path = get_dir()?;
    path.push("installed");
    if !path.exists() && DirBuilder::new().create(&path).is_err() {
        Err(String::from("Failed to create pax installation directory!"))
    } else {
        Ok(path)
    }
}

pub fn is_root() -> bool {
    unistd::geteuid().as_raw() == 0
}

pub fn tmpfile() -> Option<PathBuf> {
    Some(PathBuf::from(
        String::from_utf8_lossy(&Command::new("mktemp").output().ok()?.stdout).trim(),
    ))
}
