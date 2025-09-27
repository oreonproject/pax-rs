use std::env;

use paxr::{Command, Flag, StateBox, install};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let sample_flag = Flag {
        short: 's',
        long: String::from("sample"),
        about: String::from("does nothing"),
        consumer: false,
        breakpoint: false,
        run_func: sample_work,
    };
    let consumable_flag = Flag {
        short: 'c',
        long: String::from("consume"),
        about: String::from("consumes the next arg"),
        consumer: true,
        breakpoint: false,
        run_func: consumable_work,
    };
    let command = Command::new(
        "pax",
        Vec::new(),
        "PAX is the official package manager for the Oreon 11.",
        vec![sample_flag, consumable_flag],
        vec![install::install()],
        main_work,
        "There is no manual. 'Go' sucks.",
    );
    command.run(args.iter());
}

fn main_work(states: &StateBox) {
    println!("Hello, World!\n{}", states.len());
}

fn sample_work(_parent: &mut StateBox, _flag: Option<&String>) {
    println!("Did nothing successfully.");
}

fn consumable_work(_parent: &mut StateBox, flag: Option<&String>) {
    println!("Got flag {flag:?}!");
}
