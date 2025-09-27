use crate::{Command, StateBox};

pub fn install() -> Command {
    Command::new(
        "install",
        vec![String::from("i")],
        "Install the application from a specified path",
        Vec::new(),
        Vec::new(),
        install_work,
        "KEKW",
    )
}

fn install_work(_states: &StateBox) {
    println!("(not) Installing...");
}
