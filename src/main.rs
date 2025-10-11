use std::{env, path::Path};

pub use {
    commands::Command,
    flags::Flag,
    settings::{SettingsYaml, acquire_lock},
    statebox::StateBox,
    utils::{PostAction, choice, err},
};

pub mod emancipate;
pub mod install;
pub mod pax_init;
pub mod remove;
pub mod update;
pub mod upgrade;

pub fn main() {
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
        "PAX is the official package manager for Oreon 11.",
        vec![],
        Some(vec![
            emancipate::build,
            install::build,
            pax_init::build,
            remove::build_purge,
            remove::build_remove,
            update::build,
            upgrade::build,
        ]),
        |_command, _args| PostAction::GetHelp,
        &[],
    );
    // Run the command with the provided arguments
    main_command.run(args);
}
