# Hello!
I'm just gonna add this readme rq b4 i push to github. I will likely work on this when I'm offline (read: on the train etc.) SO if commits may be pushed online a couple hours after made. The structs etc. shld be done in the root `lib.rs`, and implementaitons should be done in the main `main.rs` and nested `mod.rs`s.
> [!WARNING]
> The most up-to-date version of this repo (at least what I'm adding) will be on my local NAS - github will receive the _commit_ updates, but the NAS will receive the _On Save_ updates. As such, please 🙏 make a PR if you wish to introduce breaking changes.

# WYA?
I'm still working on the Flags system of `lib.rs` - more specifically, the `run()` function of the `impl Command` (yes Command not Flags), so dw abt that breaking. I'm trying very hard to mimic the output of the `Go` version of `pax`, so things may seem weird.
Speaking of weird, idk why but i decided not to use `clap` etc. libraries cuz I decided to challenge myself to see if I can build a command system myself. (Side note: If this proves to be a drawback, feel free to change `lib.rs` for a crate - but PR please!)

# Structure
uhh each command in their own folder, e.g. say there are commands `cmd1` and `cmd2`, and `cmd2` has commands `nested1` and `nested2`, the structure shld look like so (this is to make it easier to find libraries etc.):
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
│   │   └── lib.rs
│   ├── lib.rs
│   └── main.rs
├── Cargo.lock
└── Cargo.toml
```

# GTG!

Soz i _really_ gotta run, ill expand this soon. Quick Q tho: I may implement this https://github.com/DitherDude/browser/wiki/Universal-information#binary-flags, but b4 i do so i need to confirm if this is how linux syntax is meant to be handled.

