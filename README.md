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
A quick glance over this project will reveal how few dependencies it has - namely `core` (builtin), `std` (builtin), and `lib.rs`. `lib.rs` is the 'backend' of the command parser, and holds all the implementation logic for creating commands. There is no real reason why `lib.rs` is used instead of the standard [`clap`](https://crates.io/crates/clap) crate, so `lib.rs` may be swapped out in the future. For now, though, `Command` and `Flag` types will be documented below.

> [!CAUTION]
> The current implementations load every command and sub-command into RAM all at once. This will hopefully be patched in another branch, so the implementation of `lib.rs` may change slightly in a future Pull Request.

## Command
| Struct Field | Usage |
|:------------:|-------|
|Name|This is the name of the command, and used as an argument to its parent argument to call its functionality.|
|Aliases|List of alternate names/arguments used to call this command.|
|About|The information that is displayed at the top of the help message for the command.|
|Flags|Flags that can be used on the command. See below.|
|Subcommands|The child commands that can be called from this command.|
|States|Settings that are written to via flags, and then used to alter the execution of the command.|
|Run_func|The function that is responsible for the logic of the command.|
|Man|Originally meant for the manual information, but instead is used as a brief description of the command for its parent's help flag. **Will likely be renamed**.|

## Flags
| Struct Field | Usage |
|:------------:|-------|
|Short|The short flag name. **Will be changed from `char` to `Option<char>`**.|
|Long|The long flag name.|
|About|Brief description used for help information.|
|Consumer|Whether this flag takes in an argument. Follows the guidelines specified [here](https://github.com/DitherDude/browser/wiki/Universal-information#binary-flags).|
|Breakpoint|Whether this flag should prevent the execution of its parent command's logic. If this is the case, it is stored to be executed _after_ the logic of all other user-specified flags are run. Note: The program will panic if multiple `breakpoint` flags are specified on the CLI.|
|Run_func|The function that is responsible for the logic of the flag.|
