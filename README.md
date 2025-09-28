Pax Package Manager (Rust)

# Structure
To make the structure of this repo better for readability, each subcommand will be placed in its own folder within the directory of its parent command - e.g. say there are commands `cmd1` and `cmd2`, with `cmd2` having commands `nested1` and `nested2`, the directory structure should look like so:
```
.
├── src
│   ├── cmd1
│   │   └── mod.rs
│   ├── cmd2
│   │   ├── nested1
│   │   │   └── mod.rs
│   │   ├── nested2
│   │   │   └── mod.rs
│   │   └── mod.rs
│   ├── command.rs
│   ├── flag.rs
│   ├── main.rs
│   └── statebox.rs
├── Cargo.lock
└── Cargo.toml
```

# Pseudo-docs
A quick glance over this project will reveal how few dependancies it has - namely `core` (builtin), `std` (builtin). There is no real reason why `command.rs`,`flag.rs`,and`statebox.rs` are used instead of the standard [`clap`](https://crates.io/crates/clap) crate, so they may be swapped out in the future. For now, though, `Command` and `Flag` types will be documented below.

## Command
| Struct Field | Usage |
|:------------:|-------|
|Name|This is the name of the command, and used as an argument to its parent argument to call its functionality.|
|Aliases|List of alternate names/arguments used to call this command.|
|About|The information that is displayed at the top of the help message for the command.|
|Flags|Flags that can be used on the command. See below.|
|Subcommands|Constructors of child commands that can be called from this command. Note that this is not a list of `Command`, which reduces the memory usage of the program.|
|States|Settings that are written to via flags, and then used to alter the execution of the command.|
|Run_func|The function that is responsible for the logic of the command.|
|Hierarchy|A list of all the prior `Command`s' names for use in the help command.|

## Flags
| Struct Field | Usage |
|:------------:|-------|
|Short|The short flag name, if any.|
|Long|The long flag name.|
|About|Brief description used for help information.|
|Consumer|Whether this flag takes in an argument. Follows the guidelines specified [here](https://github.com/DitherDude/browser/wiki/Universal-information#binary-flags).|
|Breakpoint|Whether this flag should prevent the execution of its parent command's logic. If this is the case, it is stored to be executed _after_ the logic of all other user-specified flags are run. Note: The program will panic if multiple `breakpoint` flags are specified on the CLI.|
|Run_func|The function that is responsible for the logic of the flag.|
