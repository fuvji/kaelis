//! Kaelis core: brand, instances, JVM flags, single-instance mutex,
//! ephemeral RAM logs, and the fileless memory sweeper.
pub mod brand;
pub mod ephemeral_log;
pub mod handoff;
pub mod instance;
pub mod junk;
pub mod jvm_flags;
pub mod single_instance;
pub mod sweeper;

pub use brand::{DISCORD_HUB, LAUNCHER_NAME};
