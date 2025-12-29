use std::{env, path::Path};

pub mod configure;
pub mod emancipate;
pub mod info;
pub mod install;
pub mod isocreate;
pub mod list;
pub mod pax_init;
pub mod remove;
pub mod repo;
pub mod search;
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
    let main_command = commands::Command::new(
        name,
        Vec::new(),
        "PAX is the official package manager for Oreon.",
        vec![],
        Some(vec![
            configure::build,
            emancipate::build,
            info::build,
            install::build,
            isocreate::build,
            list::build,
            pax_init::build,
            remove::build_purge,
            remove::build_remove,
            repo::build,
            metadata::extensions::cli::build,
            search::build,
            update::build,
            upgrade::build,
        ]),
        |_command, _args| utils::PostAction::GetHelp,
        &[],
    );
    // Run the command with the provided arguments
    main_command.run(args);
}
