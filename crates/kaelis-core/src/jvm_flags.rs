//! JVM flags for a Minecraft client instance.
//!
//! Modern HotSpot already defaults to G1 with ergonomic young-generation,
//! region, and pause sizing. Paper's current guidance is to start modern
//! runtimes with no GC flags, and Oracle's tuning guide asks for observed GC
//! evidence before pinning those knobs. The launcher therefore sets only the
//! heap and the collector, and leaves the rest to the runtime; instance-level
//! custom flags are appended by the launch path. See ADR-018.
const MIN_HEAP_MB: u32 = 512;
const MAX_HEAP_MB: u32 = 65_536;

pub fn default_flags(ram_mb: u32) -> Vec<String> {
    default_flags_with_min(ram_mb, MIN_HEAP_MB)
}

/// Build heap flags with an explicit initial heap. The initial heap is clamped
/// to the same sensible range and never exceeds the maximum heap, so a stale
/// or hand-edited instance cannot make the JVM reject its own arguments.
pub fn default_flags_with_min(ram_mb: u32, min_ram_mb: u32) -> Vec<String> {
    let heap = ram_mb.clamp(MIN_HEAP_MB, MAX_HEAP_MB);
    let initial = min_ram_mb.clamp(MIN_HEAP_MB, heap);
    vec![
        format!("-Xms{initial}M"),
        format!("-Xmx{heap}M"),
        "-XX:+UseG1GC".to_owned(),
        // Reference processing runs in parallel on every collector thread. It
        // is a stable, non-experimental switch: where the heap has many weak
        // references (Minecraft's registries do), it shortens the pause without
        // pinning any region, pause, or thread count.
        "-XX:+ParallelRefProcEnabled".to_owned(),
    ]
}

#[cfg(test)]
mod tests {
    use super::{MAX_HEAP_MB, MIN_HEAP_MB, default_flags, default_flags_with_min};

    #[test]
    fn clamps_unusable_heap_requests() {
        assert!(default_flags(0).iter().any(|flag| flag == "-Xmx512M"));
        assert!(
            default_flags(u32::MAX)
                .iter()
                .any(|flag| flag == &format!("-Xmx{MAX_HEAP_MB}M"))
        );
        assert!(
            default_flags(MIN_HEAP_MB)
                .iter()
                .any(|flag| flag == "-Xmx512M")
        );
    }

    #[test]
    fn keeps_the_requested_heap_untouched_inside_bounds() {
        let flags = default_flags(4096);
        assert!(flags.iter().any(|flag| flag == "-Xmx4096M"));
        assert!(flags.iter().any(|flag| flag == "-Xms512M"));
    }

    #[test]
    fn initial_heap_is_bounded_by_the_maximum_heap() {
        let flags = default_flags_with_min(2048, 8192);
        assert!(flags.iter().any(|flag| flag == "-Xms2048M"));
        assert!(flags.iter().any(|flag| flag == "-Xmx2048M"));
        let flags = default_flags_with_min(4096, 128);
        assert!(flags.iter().any(|flag| flag == "-Xms512M"));
    }

    #[test]
    fn does_not_pin_experimental_gc_knobs() {
        let flags = default_flags(8192);
        assert!(flags.iter().any(|flag| flag == "-XX:+UseG1GC"));
        assert!(
            !flags
                .iter()
                .any(|flag| flag.contains("UnlockExperimentalVMOptions"))
        );
        assert!(!flags.iter().any(|flag| flag.starts_with("-Xmn")));
    }
}
