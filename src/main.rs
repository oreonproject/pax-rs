use std::env;

use pax::{Command, Flag, StateBox, install};

fn main() {
    // Skip first arg, which is the executable name
    let args: Vec<String> = env::args().skip(1).collect();
    let sample_flag = Flag {
        short: 's',
        long: String::from("sample"),
        about: String::from("does nothing"),
        consumer: false,
        breakpoint: false,
        run_func: | _parent: &mut StateBox, _flag: Option<&String>| {
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
        run_func: | _parent: &mut StateBox, flag: Option<&String>| {
            println!("Got flag {flag:?}!");
        },
    };
    // Main command
    let command = Command::new(
        "pax",
        Vec::new(),
        "PAX is the official package manager for the Oreon 11.",
        vec![sample_flag, consumable_flag],
        vec![install::install()],
        |states: &StateBox| {
            println!("Hello, World!\n{}", states.len());
        },
        "Run 'pax <command> --help' for more information on a command.",
    );
    // Run the command with the provided arguments
    command.run(args.iter());
}