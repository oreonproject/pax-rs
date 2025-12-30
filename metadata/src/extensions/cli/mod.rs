use commands::Command;
use statebox::StateBox;
use utils::PostAction;

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "extension",
        vec![String::from("ext")],
        "Manage PAX extensions",
        vec![],
        Some(vec![build_add]),
        run,
        hierarchy,
    )
}

fn run(_states: &StateBox, _args: Option<&[String]>) -> PostAction {
    PostAction::GetHelp
}

fn build_add(parents: &[String]) -> Command {
    Command::new(
        "add",
        vec![String::from("a")],
        "Add an extension",
        vec![],
        None,
        add_run,
        parents,
    )
}

fn add_run(_states: &StateBox, args: Option<&[String]>) -> PostAction {
    if let Some(args) = args {
        if !args.is_empty() {
            println!("Adding extension: {}", args.join(" "));
            return PostAction::Return;
        }
    }
    println!("No extension specified. Use `pax extension add --help` for usage.");
    PostAction::GetHelp
}
