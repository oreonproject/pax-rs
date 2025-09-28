use std::{env, path::Path};

pub use {command::Command, flag::Flag, statebox::StateBox};
pub mod command;
pub mod flag;
pub mod statebox;

pub mod install;

// use crate::{Command, Flag, StateBox, install};

fn main() {
    let args: Vec<String> = env::args().collect();
    let mut args = args.iter();
    let name = args
        .next()
        .map(|arg| Path::new(arg).file_name().map(|x| x.to_str()))
        .unwrap_or(None)
        .unwrap_or(None)
        .unwrap_or("pax");
    let sample_flag = Flag {
        short: Some('s'),
        long: String::from("sample"),
        about: String::from("does nothing"),
        consumer: false,
        breakpoint: false,
        run_func: |_parent, _flag| {
            println!("Did nothing successfully.");
        },
    };
    // get first arg after -c or --consume
    let consumable_flag = Flag {
        short: Some('c'),
        long: String::from("consume"),
        about: String::from("consumes the next arg"),
        consumer: true,
        breakpoint: false,
        run_func: |_parent, flag| {
            println!("Got flag {flag:?}!");
        },
    };
    // Main command
    let command = Command::new(
        name,
        Vec::new(),
        "PAX is the official package manager for the Oreon 11.",
        vec![sample_flag, consumable_flag],
        Some(vec![install::build]),
        |states, _args| {
            println!("Hello, World!\n{}", states.len());
        },
        &[],
    );
    // Run the command with the provided arguments
    command.run(args);
}
