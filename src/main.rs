use std::env;

pub use {command::Command, flag::Flag, statebox::StateBox};
pub mod command;
pub mod flag;
pub mod statebox;

pub mod install;

// use crate::{Command, Flag, StateBox, install};

fn main() {
    // Skip first arg, which is the executable name
    let args: Vec<String> = env::args().skip(1).collect();
    let sample_flag = Flag {
        short: 's',
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
        short: 'c',
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
        "pax",
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
    command.run(args.iter());
}
