//! Game preparation and launch.
//!
//! - Preparation: the version's libraries, natives and assets (from `vl-meta`) are checked and
//!   downloaded (`vl-net`) as one task (`vl-tasks`).
//! - The command line: Java (`vl-java`), memory and window settings, game and JVM arguments,
//!   pre-launch, wrapper and post-exit commands, environment variables.
//! - The game process: start, forced stop, exit code and crash detection, several processes of
//!   one instance.
//! - The game log is streamed line by line as `vl_core::log::LogLine` events.
