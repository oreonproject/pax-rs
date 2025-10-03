use std::{env, path::Path};

pub use {
    commands::{Command, PostAction},
    flags::Flag,
    settings::SettingsYaml,
    statebox::StateBox,
};

pub mod adapters;
pub mod clean;
pub mod compile;
pub mod crypto;
pub mod database;
pub mod download;
pub mod info;
pub mod install;
pub mod list;
pub mod lock;
pub mod logging;
pub mod provides;
pub mod remove;
pub mod repository;
pub mod resolver;
pub mod search;
pub mod store;
pub mod symlinks;
pub mod transaction;
pub mod trust;
pub mod update;
pub mod verify;

pub fn main() {
    // Initialize logger (ignore errors if we don't have permissions assuming you are a dumbass) (JK, i had no idea what i was saying there...)
    let _ = logging::init_logger();
    
    let args: Vec<String> = env::args().collect();
    let mut args = args.iter();
    let name = args
        .next()
        .map(|arg| Path::new(arg).file_name().map(|x| x.to_str()))
        .unwrap_or(None)
        .unwrap_or(None)
        .unwrap_or("pax");
    
    // Main command
    let main_command = Command::new(
        name,
        Vec::new(),
        "PAX is the official package manager for Oreon 11 - A universal package manager with cross-distro support",
        Vec::new(),
        Some(vec![
            install::build,
            remove::build,
            update::build,
            search::build,
            info::build,
            list::build,
            clean::build,
            compile::build,
            trust::build,
        ]),
        |_command, _args| PostAction::GetHelp,
        &[],
    );
    
    // Run the command with the provided arguments
    main_command.run(args);
}
