use std::{env, io::Write, iter::once, process::Command as RunCommand, slice::Iter};

use flags::Flag;
use statebox::StateBox;

// Helper return enum for handlers
enum HandlerResult {
    ContinueOuter,
    ReturnEarly,
}

// The action to perform once a command has run
pub enum PostAction {
    Elevate,
    GetHelp,
    PullSources,
    Return,
}

// Extraction of complex type
type Subcommand = Option<Vec<fn(parents: &[String]) -> Command>>;

pub struct Command {
    pub name: String,
    pub aliases: Vec<String>,
    pub about: String,
    pub flags: Vec<Flag>,
    pub subcommands: Subcommand,
    states: StateBox,
    pub run_func: fn(states: &StateBox, args: Option<&[String]>) -> PostAction,
    pub hierarchy: Vec<String>,
}

impl PartialEq for Command {
    // Superfluous PartialEq implementation to allow for struct field equality checks.
    fn eq(
        &self,
        Command {
            name: _,
            aliases: _,
            about: _,
            flags: _,
            subcommands: _,
            states: _,
            run_func: _,
            hierarchy: _,
        }: &Self,
    ) -> bool {
        false
    }
}

impl Command {
    // Create new command
    pub fn new(
        name: &str,
        aliases: Vec<String>,
        about: &str,
        flags: Vec<Flag>,
        subcommands: Subcommand,
        run_func: fn(states: &StateBox, args: Option<&[String]>) -> PostAction,
        hierarchy: &[String],
    ) -> Self {
        Command {
            name: name.to_string(),
            aliases,
            about: about.to_string(),
            flags,
            subcommands,
            states: StateBox::new(),
            run_func,
            hierarchy: hierarchy.to_vec(),
        }
    }
    // Returns a hierarchy list of all the parents of the command for the "Run" tip at the bottom of the help command.
    fn compile_parents(&self) -> Vec<String> {
        let mut hierarchy = self.hierarchy.clone();
        hierarchy.push(self.name.clone());
        hierarchy.to_vec()
    }
    pub fn help(&self) -> String {
        // Make help message
        let mut help = String::new();
        help.push_str(&format!("{}\n", self.about));
        let mut commands = String::new();
        let mut aliases = String::new();

        // Show possible commands, flags, and aliases
        let mut attrs = String::from(&format!("Usage:\n  {} [flags]\n", self.name));
        let mut flags = String::from("\nFlags:\n");

        // Apply flags to the command
        for flag in &self.flags {
            flags.push_str(&format!("  {}\n", flag.help()));
        }

        // Add the help flag
        flags.push_str(&format!("  -h, --help\thelp for {}", self.name));

        // Check if there are subcommands or aliases
        if let Some(subcommands) = &self.subcommands
            && *subcommands != Vec::new()
        {
            attrs.push_str(&format!("  {} [command]\n", self.name));
            commands = String::from("\nAvailable Commands:\n");
            for command in subcommands {
                let command = (command)(&[]);
                commands.push_str(&format!("  {}\t{}\n", command.name, command.about));
            }
        }
        if self.aliases != Vec::<String>::new() {
            aliases = format!("\nAliases:\n  {}, ", self.name);
            for alias in &self.aliases {
                aliases.push_str(&format!("{}, ", alias));
            }
            aliases = format!("{}\n", aliases.trim_end_matches(", "));
        }
        help.push_str(&format!("{attrs}{commands}{aliases}{flags}"));
        if self.subcommands.is_some() {
            help.push_str(&format!(
                "\n\nUse `{} [command] --help` for more information about a command.",
                self.compile_parents()
                    .iter()
                    .fold(String::new(), |acc, x| format!("{acc} {x}"))
                    .trim()
            ));
        }
        help
    }
    // Run the command
    pub fn run(self, mut args: Iter<'_, String>) {
        let mut m_self = self;
        let mut first_arg = true;
        // store breakpoint
        let mut opr: Option<(usize, Option<String>)> = None;

        // outer loop over args
        'outer: while let Some(arg) = args.next() {
            if let Some(l_arg) = arg.strip_prefix("--") {
                match m_self.handle_long_flag(l_arg, &mut args, &mut opr) {
                    HandlerResult::ContinueOuter => continue 'outer,
                    HandlerResult::ReturnEarly => return,
                }
            } else if let Some(s_arg) = arg.strip_prefix("-") {
                match m_self.handle_short_flags(s_arg, &mut args, &mut opr) {
                    HandlerResult::ContinueOuter => continue 'outer,
                    HandlerResult::ReturnEarly => return,
                }
            } else if first_arg {
                m_self.try_handle_subcommand(arg, &mut args);
                return;
            }
            first_arg = false;
        }
        if let Some((flag_idx, val)) = opr {
            let flag = &m_self.flags[flag_idx];
            (flag.run_func)(&mut m_self.states, val)
        } else {
            m_self.handle_post_action((m_self.run_func)(&m_self.states, None));
        }
    }

    fn handle_long_flag(
        &mut self,
        l_arg: &str,
        args: &mut Iter<'_, String>,
        opr: &mut Option<(usize, Option<String>)>,
    ) -> HandlerResult {
        match l_arg {
            // Help flag
            "help" => {
                println!("{}", self.help());
                HandlerResult::ReturnEarly
            }
            _ => {
                // Regular flags
                for (i, flag) in self.flags.iter().enumerate() {
                    if flag.long == l_arg {
                        let val = if flag.consumer {
                            args.next().cloned()
                        } else {
                            None
                        };
                        if flag.breakpoint {
                            if opr.is_some() {
                                panic!("Multiple breakpoint arguments supplied!");
                            }
                            *opr = Some((i, val));
                        } else {
                            (flag.run_func)(&mut self.states, val)
                        }
                        return HandlerResult::ContinueOuter;
                    }
                }
                let error = format!("unknown flag: '{l_arg}'");
                println!("Error: {error}\n{}\n\n{error}", self.help());
                HandlerResult::ReturnEarly
            }
        }
    }

    fn handle_short_flags(
        &mut self,
        s_arg: &str,
        args: &mut Iter<'_, String>,
        opr: &mut Option<(usize, Option<String>)>,
    ) -> HandlerResult {
        'mid: for chr in s_arg.chars() {
            match chr {
                // Help flag
                'h' => {
                    println!("{}", self.help());
                    return HandlerResult::ReturnEarly;
                }
                c => {
                    for (i, flag) in self.flags.iter().enumerate() {
                        if flag.short == Some(c) {
                            let val = if flag.consumer {
                                args.next().cloned()
                            } else {
                                None
                            };
                            if flag.breakpoint {
                                if opr.is_some() {
                                    panic!("Multiple breakpoint arguments supplied!");
                                }
                                *opr = Some((i, val));
                            } else {
                                (flag.run_func)(&mut self.states, val)
                            }
                            continue 'mid;
                        }
                    }
                    let error = format!("unknown shorthand flag: '{c}' in -{s_arg}");
                    println!("Error: {error}\n{}\n\n{error}", self.help());
                    return HandlerResult::ReturnEarly;
                }
            }
        }
        HandlerResult::ContinueOuter
    }

    fn try_handle_subcommand(self, arg: &str, args: &mut Iter<'_, String>) {
        let parents = &self.compile_parents();
        if let Some(subcommands) = self.subcommands {
            for command in subcommands {
                let command = (command)(parents);
                if command.name == arg {
                    command.run(args.clone());
                    return;
                } else {
                    for alias in &command.aliases {
                        if alias == arg {
                            command.run(args.clone());
                            return;
                        }
                    }
                }
            }
            let error = format!("unknown command \"{arg}\" for \"{}\"", self.name);
            println!(
                "Error: {error}\nRun {} --help for usage.\n{error}",
                self.name
            );
        } else {
            // Takes the first argument (which was popped from the front of args at the 'outer loop of `run()`) and adds it to the remaining arguments, before calling the main function.
            let args = once(arg.to_string())
                .chain(args.cloned())
                .collect::<Vec<String>>();
            self.handle_post_action((self.run_func)(&self.states, Some(&args)));
        }
    }

    fn handle_post_action(&self, action: PostAction) {
        match action {
            PostAction::Elevate => {
                println!("The action you attempted to perform requires root privileges.");
                match choice("Would you like to try perform this action as sudo?", false) {
                    None => println!("\nFailed to read terminal input!"),
                    Some(true) => {
                        println!("Attempting to elevate execution...");
                        let _ = std::io::stdout().flush();
                        let mut cmd = RunCommand::new("sudo");
                        if cmd.args(env::args()).status().is_err() {
                            println!("Failed to acquire sudo!");
                        }
                    }
                    Some(false) => (),
                }
            }
            PostAction::GetHelp => println!("{}", self.help()),
            PostAction::PullSources => {
                match choice("\x1B[2K\rMissing sources.txt! Try pull them now?", false) {
                    None => println!("\nFailed to read terminal input!"),
                    Some(true) => {
                        let args = env::args().collect::<Vec<String>>();
                        let mut args = args.iter();
                        let program = args.next();
                        if let Some(program) = program {
                            let mut cmd = RunCommand::new(program);
                            if cmd.args(["pax-init", "--force"]).status().is_err() {
                                println!("Failed to re-execute!");
                                return;
                            }
                            let mut cmd = RunCommand::new(program);
                            if cmd.args(args).status().is_err() {
                                println!("Failed to re-execute!");
                            }
                        } else {
                            println!("Failed to locate program!");
                        }
                    }
                    Some(false) => (),
                }
            }
            PostAction::Return => (),
        }
    }
}

fn choice(message: &str, default_yes: bool) -> Option<bool> {
    print!(
        "{} [{}]: ",
        message,
        if default_yes { "Y/n" } else { "y/N" }
    );
    let _ = std::io::stdout().flush();
    let mut input = String::new();
    if std::io::stdin().read_line(&mut input).is_err() {
        println!("\nFailed to read terminal input!");
        return None;
    }
    if default_yes {
        if ["no", "n", "false", "f"].contains(&input.to_lowercase().trim()) {
            Some(false)
        } else {
            Some(true)
        }
    } else if ["yes", "y", "true", "t"].contains(&input.to_lowercase().trim()) {
        Some(true)
    } else {
        Some(false)
    }
}
