use std::{env, path::Path};

pub use {
    commands::{Command, PostAction},
    flags::Flag,
    settings::SettingsYaml,
    statebox::StateBox,
    utils::err,
};

pub mod emancipate;
pub mod endpoints_init;
pub mod install;
pub mod remove;
pub mod update;

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
            endpoints_init::build,
            install::build,
            remove::build_remove,
            remove::build_purge,
            update::build,
            emancipate::build,
        ]),
        |_command, _args| PostAction::GetHelp,
        &[],
    );
    // Run the command with the provided arguments
    main_command.run(args);
}
