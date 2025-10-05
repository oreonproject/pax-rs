use metadata::{MetaDataKind, build_deps};
use settings::get_settings;
use std::{collections::HashSet, fs};
use tokio::runtime::Runtime;
use utils::is_root;

use crate::{Command, PostAction, StateBox};

pub fn build(hierarchy: &[String]) -> Command {
    Command::new(
        "install",
        vec![String::from("i")],
        "Install the application from a specified path",
        Vec::new(),
        None,
        run,
        hierarchy,
    )
}

fn run(_: &StateBox, args: Option<&[String]>) -> PostAction {
    if !is_root() {
        return PostAction::Elevate;
    }
    let args = match args {
        None => return PostAction::Return,
        Some(args) => args,
    };
    print!("Reading sources...");
    let sources = match get_settings() {
        Ok(settings) => settings.sources,
        Err(_) => return PostAction::PullSources,
    };
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(_) => {
            println!("Error creating runtime!");
            return PostAction::Return;
        }
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
                                        println!("Failed to free {}!", file.display());
                                        return PostAction::Return;
                                    }
                                }
                                None => println!("{name} is already at the latest version."),
                            },
                            Err(message) => {
                                println!(
                                    "Error installing package {name}!\nReported error: `{message}`",
                                );
                                return PostAction::Return;
                            }
                        }
                    }
                }
            }
        }
        Err(fault) => println!("\x1B[2K\r{fault}"),
    }
    PostAction::Return
}
