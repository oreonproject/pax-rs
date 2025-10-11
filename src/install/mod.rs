use metadata::{MetaDataKind, build_deps};
use settings::SettingsYaml;
use settings::acquire_lock;
use std::{collections::HashSet, fs};
use tokio::runtime::Runtime;

use crate::{Command, PostAction, StateBox};

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

fn run(_: &StateBox, args: Option<&[String]>) -> PostAction {
    match acquire_lock() {
        Ok(Some(action)) => return action,
        Err(fault) => return PostAction::Fuck(fault),
        _ => (),
    }
    let args = match args {
        None => return PostAction::NothingToDo,
        Some(args) => args,
    };
    print!("Reading sources...");
    let sources = match SettingsYaml::get_settings() {
        Ok(settings) => settings.sources,
        Err(_) => return PostAction::PullSources,
    };
    if sources.is_empty() {
        return PostAction::PullSources;
    }
    let Ok(runtime) = Runtime::new() else {
        return PostAction::Fuck(String::from("Error creating runtime!"));
    };
    match build_deps(args, &sources, &runtime, &mut HashSet::new(), false) {
        Ok(mut packages) => {
            println!();
            packages.reverse();
            for package in packages {
                match &package.kind {
                    MetaDataKind::Pax => {
                        let name = package.name.to_string();
                        match package.install_package(&sources, &runtime) {
                            Ok(file) => match file {
                                Some(file) => {
                                    if fs::remove_file(&file).is_err() {
                                        return PostAction::Fuck(format!(
                                            "Failed to free {}!",
                                            file.display()
                                        ));
                                    }
                                }
                                None => println!("{name} is already at the latest version."),
                            },
                            Err(message) => {
                                return PostAction::Fuck(format!(
                                    "\x1B[0mError installing package {name}!\nReported error: \"\x1B[91m{message}\x1B[0m\""
                                ));
                            }
                        }
                    }
                }
            }
        }
        Err(fault) => return PostAction::Fuck(fault),
    }
    PostAction::Return
}
