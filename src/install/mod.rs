use crate::{Command, StateBox};

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "install",
        vec![String::from("i")],
        "Install the application from a specified path",
        Vec::new(),
        None,
        install_work,
        hierarchy,
    )
}

fn install_work(_states: &StateBox, args: Option<&[String]>) {
    let apps = if let Some(args) = args {
        let mut apps = String::new();
        for arg in args {
            apps.push_str(&format!(" {}", arg));
        }
        apps
    } else {
        String::new()
    };
    println!("(not) Installing{}...", apps);
}
