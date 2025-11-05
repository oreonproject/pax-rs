use commands::Command;
use flags::Flag;
use settings::{SettingsYaml, acquire_lock, remove_lock};
use statebox::StateBox;
use utils::{PostAction, choice, err};

pub fn build(hierarchy: &[String]) -> Command {
    let setting = Flag::new(
        Some('s'),
        "set",
        "Command to set options in the SettingsYAML file.",
        true,
        true,
        set_handle,
    );
    Command::new(
        "configure",
        vec![String::from("c")],
        "Configures internal pax settings.",
        vec![setting, utils::yes_flag()],
        None,
        |_, _| PostAction::GetHelp,
        hierarchy,
    )
}

fn set_handle(states: &mut StateBox, arg: Option<String>) {
    match acquire_lock() {
        Ok(Some(_)) => {
            println!("Did not expect a PostAction at this time.");
            return;
        }
        Err(fault) => {
            print!("{fault}");
            return;
        }
        _ => (),
    };
    let settings = match SettingsYaml::get_settings() {
        Ok(settings) => settings,
        Err(fault) => {
            println!("{fault}");
            return;
        }
    };
    if let Err(fault) = set_func(states, arg, settings) {
        println!("{fault}");
    };
    if let Err(fault) = remove_lock() {
        println!("{fault}");
    }
}

fn set_func(
    states: &mut StateBox,
    arg: Option<String>,
    mut settings: SettingsYaml,
) -> Result<(), String> {
    let Some(arg) = arg else {
        return err!("Missing an argument!");
    };
    let Some((key, value)) = arg.split_once('=') else {
        return err!("Invalid syntax. please use `--set \"key=value\"`.");
    };
    match key {
        "exec" => {
            let val = if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            };
            println!(
                "Will change setting `exec` from \x1B[95m{:?}\x1B[0m to \x1B[95m{val:?}\x1B[0m.",
                settings.exec
            );
            if states.get("yes").is_none_or(|x: &bool| !*x) {
                match choice("Proceed?", true) {
                    Err(message) => return err!("{message}"),
                    Ok(false) => return err!("Abort."),
                    Ok(true) => (),
                }
            }
            settings.exec = val;
        }
        _ => return err!("Unrecognized key {key}!"),
    }
    settings.set_settings()?;
    Ok(())
}
