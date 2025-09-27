use std::{any::Any, collections::HashMap, slice::Iter};

pub mod install;

pub struct Command {
    pub name: String,
    pub aliases: Vec<String>,
    pub about: String,
    pub flags: Vec<Flag>,
    pub subcommands: Vec<Command>,
    pub states: StateBox,
    pub run_func: fn(states: &StateBox),
    pub man: String,
}

impl PartialEq for Command {
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
    pub fn new(
        name: &str,
        aliases: Vec<String>,
        about: &str,
        flags: Vec<Flag>,
        subcommands: Vec<Command>,
        run_func: fn(states: &StateBox),
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
        let mut help = String::new();
        help.push_str(&format!("{}\n", self.about));
        let mut commands = String::new();
        let mut aliases = String::new();
        let mut attrs = String::from(&format!("Usage:\n  {} [flags]\n", self.name));
        let mut flags = String::from("\nFlags:\n");
        for flag in &self.flags {
            flags.push_str(&format!("  {}\n", flag.help()));
        }
        flags.push_str(&format!("  -h, --help\thelp for {}\n", self.name));
        if self.subcommands != Vec::new() {
            attrs.push_str(&format!("  {} [command]\n", self.name));
            commands = String::from("\nAvailable Commands:\n");
            for command in &self.subcommands {
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
    pub fn run(self, mut args: Iter<'_, String>) {
        let mut m_self = self;
        let mut first_arg = true;
        let mut opr = None;
        'outer: while let Some(arg) = args.nth(0) {
            if let Some(l_arg) = arg.strip_prefix("--") {
                match l_arg {
                    "help" => {
                        println!("{}", m_self.help());
                        return;
                    }
                    _ => {
                        for flag in &m_self.flags {
                            if flag.long == l_arg {
                                let val = if flag.consumer { args.nth(0) } else { None };
                                if flag.breakpoint {
                                    if opr.is_some() {
                                        panic!("Multiple breakpoint arguments supplied!");
                                    }
                                    opr = Some((flag, val));
                                } else {
                                    (flag.run_func)(&mut m_self.states, val)
                                }
                                continue 'outer;
                            }
                        }
                        let error = format!("unknown flag: '{l_arg}'");
                        println!("Error: {error}\n{}\n\n{error}", m_self.help());
                        return;
                    }
                }
            } else if let Some(s_arg) = arg.strip_prefix("-") {
                'mid: for chr in s_arg.chars() {
                    match chr {
                        'h' => {
                            println!("{}", m_self.help());
                            return;
                        }
                        c => {
                            for flag in &m_self.flags {
                                if flag.short == c {
                                    let val = if flag.consumer { args.nth(0) } else { None };
                                    if flag.breakpoint {
                                        if opr.is_some() {
                                            panic!("Multiple breakpoint arguments supplied!");
                                        }
                                        opr = Some((flag, val));
                                    } else {
                                        (flag.run_func)(&mut m_self.states, val)
                                    }
                                    continue 'mid;
                                }
                            }
                            let error = format!("unknown shorthand flag: '{c}' in -{s_arg}");
                            println!("Error: {error}\n{}\n\n{error}", m_self.help());
                            return;
                        }
                    }
                }
            } else if first_arg {
                for command in m_self.subcommands {
                    if command.name == *arg {
                        command.run(args);
                        return;
                    } else {
                        for alias in &command.aliases {
                            if *alias == *arg {
                                command.run(args);
                                return;
                            }
                        }
                    }
                }
                let error = format!("unknown comand \"{arg}\" for \"{}\"", m_self.name);
                println!(
                    "Error: {error}\nRun {} --help for usage.\n{error}",
                    m_self.name
                );
                return;
            }
            first_arg = false;
        }
        if let Some((opr, val)) = opr {
            (opr.run_func)(&mut m_self.states, val)
        } else {
            (m_self.run_func)(&m_self.states)
        }
    }
}

pub struct Flag {
    pub short: char,
    pub long: String,
    pub about: String,
    pub consumer: bool,
    pub breakpoint: bool,
    pub run_func: fn(parent: &mut StateBox, flag: Option<&String>),
}

impl PartialEq for Flag {
    fn eq(
        &self,
        Flag {
            short: _,
            long: _,
            about: _,
            consumer: _,
            breakpoint: _,
            run_func: _,
        }: &Self,
    ) -> bool {
        false
    }
}

impl Flag {
    pub fn help(&self) -> String {
        let mut help = String::new();
        help.push_str(&format!("-{}, --{}\t{}", self.short, self.long, self.about));
        help
    }
}

pub struct StateBox {
    store: HashMap<&'static str, Box<dyn Any>>,
}

impl StateBox {
    pub fn new() -> Self {
        StateBox {
            store: HashMap::new(),
        }
    }
    pub fn insert<T: 'static>(&mut self, key: &'static str, value: T) -> Result<(), String> {
        if self.store.contains_key(key) {
            return Err(String::from(
                "Key already exists! If you wish to update this value, use `set()` method instead.",
            ));
        }
        self.store.insert(key, Box::new(value));
        Ok(())
    }
    pub fn remove(&mut self, key: &str) -> Result<(), String> {
        match self.store.remove_entry(key) {
            Some(_) => Ok(()),
            None => Err(String::from("Cannot remove nonexistant key!")),
        }
    }
    pub fn get<T: 'static>(&self, key: &str) -> Option<&T> {
        self.store.get(key)?.downcast_ref::<T>()
    }
    pub fn set<T: 'static>(&mut self, key: &str, value: T) -> Result<(), String> {
        if let Some(state) = self.store.get_mut(key) {
            *state = Box::new(value);
            Ok(())
        } else {
            Err(String::from(
                "Key not found. If you wish to create this value, use `insert()` method instead.",
            ))
        }
    }
    pub fn push<T: 'static>(&mut self, _key: &str, _value: T) -> ! {
        //Learned the '!' (bang) return type from RUst Kernel dev ;P
        unimplemented!()
    }
    pub fn pop<T: 'static>(&mut self, key: &str) -> Option<T> {
        self.store
            .remove(key)?
            .downcast::<T>()
            .map(|x| Some(*x))
            .ok()?
    }
    pub fn shove<T: 'static>(&mut self, key: &'static str, value: T) {
        if let Some(state) = self.store.get_mut(key) {
            *state = Box::new(value)
        } else {
            self.store.insert(key, Box::new(value));
        }
    }
    pub fn yank(&mut self, key: &str) {
        // WARNING: This function has VERY different connotation to the 'yank' from NVIM!
        self.store.remove(key);
    }
    pub fn len(&self) -> usize {
        self.store.len()
    }
    // This is to make Clippy happy
    pub fn is_empty(&self) -> bool {
        self.store.is_empty()
    }
}

impl Default for StateBox {
    fn default() -> Self {
        Self::new()
    }
}
