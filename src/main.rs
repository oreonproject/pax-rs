use std::{env, path::Path};

pub use {
    commands::{Command, PostAction},
    flags::Flag,
    settings::SettingsYaml,
    statebox::StateBox,
};

pub mod endpoints_init;
pub mod install;

pub fn main() {
    let args: Vec<String> = env::args().collect();
    let mut args = args.iter();
    let name = args
        .next()
        .map(|arg| Path::new(arg).file_name().map(|x| x.to_str()))
        .unwrap_or(None)
        .unwrap_or(None)
        .unwrap_or("pax");
    let sample_flag = Flag::new(
        Some('s'),
        "sample",
        "does nothing",
        false,
        false,
        |_states, _flag| {
            println!("Did nothing successfully.");
        },
    );
    // get first arg after -c or --consume
    let consumable_flag = Flag::new(
        Some('c'),
        "consume",
        "consumes the next arg",
        true,
        false,
        |states, flag| {
            if let Some(flag) = flag {
                if states.insert(&flag, "https://oreonproject.org/").is_ok() {
                    println!("Got flag {flag}!");
                } else {
                    println!("WARN: Reused flag {flag}!");
                }
            } else {
                println!("FATAL: Missing flag!");
            }
        },
    );
    // Main command
    let main_command = Command::new(
        name,
        Vec::new(),
        "PAX is the official package manager for Oreon 11.",
        vec![sample_flag, consumable_flag],
        Some(vec![install::build, endpoints_init::build]),
        |_command, _args| PostAction::GetHelp,
        &[],
    );
    // Run the command with the provided arguments
    main_command.run(args);
}
