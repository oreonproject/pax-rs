use std::{fs::DirBuilder, io::Write, path::PathBuf, process::Command};

use flags::Flag;
use nix::unistd;

pub fn get_dir() -> Result<PathBuf, String> {
    let path = PathBuf::from("/etc/pax");
    if !path.exists() && DirBuilder::new().create(&path).is_err() {
        err!("Failed to create pax directory!")
    } else {
        Ok(path)
    }
}

pub fn get_metadata_dir() -> Result<PathBuf, String> {
    let mut path = get_dir()?;
    path.push("installed");
    if !path.exists() && DirBuilder::new().create(&path).is_err() {
        err!("Failed to create pax installation directory!")
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

pub fn yes_flag() -> Flag {
    Flag::new(
        Some('y'),
        "yes",
        "Bypasses applicable confirmation dialogs.",
        false,
        false,
        |states, _| {
            states.shove("yes", true);
        },
    )
}

// I learned this basic macro from Kernel dev
#[macro_export]
macro_rules! err {
    ($fmt:literal $(, $args:expr)*) => {Err(format!($fmt $(, $args)*))};
}

pub fn choice(message: &str, default_yes: bool) -> Result<bool, String> {
    print!(
        "{} [{}]: ",
        message,
        if default_yes { "Y/n" } else { "y/N" }
    );
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        return err!("\nFailed to read terminal input!");
    }
    if default_yes {
        if ["no", "n", "false", "f"].contains(&input.to_lowercase().trim()) {
            Ok(false)
        } else {
            Ok(true)
        }
    } else if ["yes", "y", "true", "t"].contains(&input.to_lowercase().trim()) {
        Ok(true)
    } else {
        Ok(false)
    }
}
