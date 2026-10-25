//! Kaelis core: brand, instances, JVM flags, user automation, single-instance
//! mutex, per-instance launch guards, ephemeral RAM logs, and the fileless
//! memory sweeper.
pub mod automation;
pub mod brand;
pub mod ephemeral_log;
pub mod format;
pub mod handoff;
pub mod instance;
pub mod instance_guard;
pub mod junk;
pub mod jvm_flags;
pub mod running;
pub mod single_instance;
pub mod stream;
pub mod sweeper;

pub use brand::{DISCORD_HUB, LAUNCHER_NAME};
pub use format::human_bytes;
