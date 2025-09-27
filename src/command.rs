use std::{iter::once, slice::Iter};

use crate::{Flag, StateBox};

// Helper return enum for handlers
enum HandlerResult {
    ContinueOuter,
    ReturnEarly,
}

pub struct Command {
    pub name: String,
    pub aliases: Vec<String>,
    pub about: String,
    pub flags: Vec<Flag>,
    pub subcommands: Option<Vec<Command>>,
    states: StateBox,
    pub run_func: fn(states: &StateBox, args: Option<&[String]>),
    pub man: String,
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
            man: _,
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
        subcommands: Option<Vec<Command>>,
        run_func: fn(states: &StateBox, args: Option<&[String]>),
        man: &str,
    ) -> Self {
        Command {
            name: name.to_string(),
            aliases,
            about: about.to_string(),
            flags,
            subcommands,
            states: StateBox::new(),
            run_func,
            man: man.to_string(),
        }
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
        flags.push_str(&format!("  -h, --help\thelp for {}\n", self.name));

        // Check if there are subcommands or aliases
        if let Some(subcommands) = &self.subcommands
            && *subcommands != Vec::new()
        {
            attrs.push_str(&format!("  {} [command]\n", self.name));
            commands = String::from("\nAvailable Commands:\n");
            for command in subcommands {
                commands.push_str(&format!("  {}\t{}\n", command.name, command.man));
            }
        }
        if self.aliases != Vec::<String>::new() {
            aliases = format!("\nAliases:\n  {}, ", self.name);
            for alias in &self.aliases {
                aliases.push_str(&format!("{}, ", alias));
            }
            aliases = format!("{}\n", aliases.trim_end_matches(", "));
        }
        help.push_str(&format!("{attrs}{commands}{aliases}{flags}\n"));
        help.push_str(&format!(
            "Use {} [command] --help for more information about a command.",
            self.name
        ));
        help
    }
    // Run the command
    pub fn run(self, mut args: Iter<'_, String>) {
        let mut m_self = self;
        let mut first_arg = true;
        // store breakpoint
        let mut opr: Option<(usize, Option<String>)> = None;

        // outer loop over args
        'outer: while let Some(arg) = args.nth(0) {
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
                // match m_self.try_handle_subcommand(arg, &mut args) {
                //     HandlerResult::ReturnEarly => return,
                //     HandlerResult::ContinueOuter => {}
                // }
                m_self.try_handle_subcommand(arg, &mut args);
                return;
            }
            first_arg = false;
        }

        if let Some((flag_idx, val)) = opr {
            let flag = &m_self.flags[flag_idx];
            (flag.run_func)(&mut m_self.states, val.as_ref())
        } else {
            (m_self.run_func)(&m_self.states, None)
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
                            args.nth(0).cloned()
                        } else {
                            None
                        };

                        if flag.breakpoint {
                            if opr.is_some() {
                                panic!("Multiple breakpoint arguments supplied!");
                            }
                            *opr = Some((i, val));
                        } else {
                            (flag.run_func)(&mut self.states, val.as_ref())
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
                'h' => {
                    println!("{}", self.help());
                    return HandlerResult::ReturnEarly;
                }
                c => {
                    for (i, flag) in self.flags.iter().enumerate() {
                        if flag.short == c {
                            let val = if flag.consumer {
                                args.nth(0).cloned()
                            } else {
                                None
                            };

                            if flag.breakpoint {
                                if opr.is_some() {
                                    panic!("Multiple breakpoint arguments supplied!");
                                }

                                *opr = Some((i, val));
                            } else {
                                (flag.run_func)(&mut self.states, val.as_ref())
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
        if let Some(subcommands) = self.subcommands {
            for command in subcommands {
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
            let error = format!("unknown comand \"{arg}\" for \"{}\"", self.name);
            println!(
                "Error: {error}\nRun {} --help for usage.\n{error}",
                self.name
            );
        } else {
            // Takes the first argument (which was popped from the front of args at the 'outer loop of `run()`) and adds it to the remaining arguments, before calling the main function.
            let args = once(arg.to_string())
                .chain(args.cloned())
                .collect::<Vec<String>>();
            (self.run_func)(&self.states, Some(&args))
        }
    }
}
