//! The tracing compiler shim of the rucc-postgres design, section 12.4.
//!
//! Postgres's build systems call `cc`, or whatever `CC` names, several thousand times in a build:
//! once per translation unit, once per link, and a few hundred times more while configure or meson
//! asks the compiler what it can do. `rpg build` points all of those at `rpg-cc`, which runs the
//! real compiler unchanged and appends one line of JSON to `compile.jsonl` for every call.
//!
//! The shim adds nothing that changes code. The one argument it may add is `-frucc-trace=<file>`,
//! and only when the real compiler is rucc and says it understands the option. That flag writes
//! timings to a file and has no effect on what rucc produces.
//!
//! This library holds the parts a test can reach: the record, the reading of a command line, the
//! configuration, and hashing. The binary in `main.rs` is the glue that runs the compiler.

pub mod args;
pub mod config;
pub mod digest;
pub mod record;
pub mod usage;
