use commands::Command;
use metadata::get_packages;
use settings::SettingsYaml;
use settings::acquire_lock;
use statebox::StateBox;
use tokio::runtime::Runtime;
use utils::PostAction;
use utils::choice;

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "install",
        vec![String::from("i")],
        "Install the application from a specified path",
        vec![utils::specific_flag(), utils::yes_flag()],
        None,
        run,
        hierarchy,
    )
}

fn run(states: &StateBox, args: Option<&[String]>) -> PostAction {
    match acquire_lock() {
        Ok(Some(action)) => return action,
        Err(fault) => return PostAction::Fuck(fault),
        _ => (),
    }
    let mut args = match args {
        None => return PostAction::NothingToDo,
        Some(args) => args.iter(),
    };
    print!("Reading sources...");
    let sources = match SettingsYaml::get_settings() {
        Ok(settings) => settings.sources,
        Err(_) => return PostAction::PullSources,
    };
    if sources.is_empty() {
        return PostAction::PullSources;
    }
    let mut data = Vec::new();
    if states.get("specific").is_some_and(|x| *x) {
        while let Some(name) = args.next()
            && let Some(ver) = args.next()
        {
            data.push((name, Some(ver)));
        }
    } else {
        args.for_each(|x| data.push((x, None)));
    }
    let Ok(runtime) = Runtime::new() else {
        return PostAction::Fuck(String::from("Error creating runtime!"));
    };
    let data = match runtime.block_on(get_packages(&data)) {
        Ok(data) => data,
        Err(fault) => return PostAction::Fuck(fault),
    };
    println!();
    if data.is_empty() {
        return PostAction::NothingToDo;
    }
    println!(
        "\nThe following package(s) will be INSTALLED: \x1B[92m{}\x1B[0m",
        data.iter()
            .fold(String::new(), |acc, x| format!("{acc} {}", x.metadata.name))
            .trim()
    );
    if data.iter().any(|x| !x.run_deps.is_empty()) {
        println!(
            "The following package(s) will be MODIFIED:  \x1B[93m{}\x1B[0m",
            data.iter()
                .flat_map(|x| x.list_deps(true))
                .fold(String::new(), |acc, x| format!("{acc} {x}"))
                .trim()
        );
        if states.get("yes").is_none_or(|x: &bool| !*x) {
            match choice("Continue?", true) {
                Err(message) => return PostAction::Fuck(message),
                Ok(false) => return PostAction::Fuck(String::from("Aborted.")),
                Ok(true) => (),
            };
        }
    }
    for data in data {
        if let Err(fault) = data.install(&sources, &runtime) {
            return PostAction::Fuck(fault);
        }
    }
    PostAction::Return
}
