//! JVM flags for a Minecraft client instance.
//!
//! HotSpot already picks G1 with ergonomic region, young-generation, and pause
//! sizing, so this list does not pin region sizes, pause targets, survivor
//! ratios, or thread counts: those are server-tuning knobs that mispredict a
//! client's bursty allocation, and this project has no FPS harness that could
//! prove one of them helps. The same reasoning rules out `-XX:+DisableExplicitGC`
//! (a mod that calls `System.gc()` to release direct buffers would then leak
//! them) and `-XX:+UseStringDeduplication` (a concurrent GC phase paid on every
//! young collection, unmeasured on low-core machines).
//!
//! What remains is the small set of switches that are correct for a 60–240 FPS
//! window on any modern machine:
//!
//! * parallel reference processing, because Minecraft's registries and caches
//!   produce many weak references;
//! * no shared `hsperfdata` file mapping, because no launcher tool reads it and
//!   the JVM otherwise keeps a mapped file updated for its whole life.
//!
//! Every switch here is a product flag available in Java 8u20 and newer, so a
//! legacy instance and a current one get the same arguments.
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
        // The JVM otherwise keeps a shared hsperfdata mapping updated on a
        // timer. Nothing Kaelis starts reads it, so the mapping is pure work.
        "-XX:+PerfDisableSharedMem".to_owned(),
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
        // Region sizing and survivor ratios are server knobs that mispredict a
        // client's allocation pattern.
        assert!(!flags.iter().any(|flag| flag.starts_with("-XX:G1")));
        assert!(
            !flags
                .iter()
                .any(|flag| flag.starts_with("-XX:SurvivorRatio"))
        );
        assert!(!flags.iter().any(|flag| flag.starts_with("-XX:MaxTenuring")));
    }

    #[test]
    fn keeps_unmeasured_gc_knobs_out() {
        let flags = default_flags(4096);
        assert!(
            flags.iter().any(|flag| flag == "-XX:+PerfDisableSharedMem"),
            "hsperfdata mapping should be off"
        );
        for rejected in [
            "-XX:MaxGCPauseMillis=200",
            "-XX:+UseStringDeduplication",
            "-XX:+DisableExplicitGC",
        ] {
            assert!(
                !flags.iter().any(|flag| flag == rejected),
                "{rejected} is unmeasured and must stay out"
            );
        }
    }
}
