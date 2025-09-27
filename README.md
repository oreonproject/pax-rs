Pax Package Manager (Rust)

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
