//! Kaelis desktop interface: translucent glass panels, async mod discovery,
//! Microsoft/offline accounts, profile-head textures, and idle frame control.

mod mark;
mod platform;
mod theme;
mod tray;

use eframe::egui;
use kaelis_api::account_import::{self, ImportScan, ImportedAccount};
use kaelis_api::accounts::{
    AccountKind, AccountMode, LauncherSettings, SavedAccount, load_settings, migrated_accounts,
    resolved_mode, save_settings,
};
use kaelis_api::auth::{
    AuthSession, DeviceCodeResponse, MicrosoftAuth, OfflineProfile, fetch_crafatar_head,
    fetch_head_for_username, offline_profile, valid_username,
};
use kaelis_api::launch::{LaunchIdentity, run_instance};
use kaelis_api::mods::{
    ContentKind, InstallEvent, InstallReport, ModPlatformClient, ModProvider, ModSearchItem,
    ModSearchResults,
};
use kaelis_api::portable;
use kaelis_api::release::LauncherRelease;
use kaelis_api::repair::{
    DuplicateRepair, RepairReport, disable_duplicate_mods, repair_environment,
};
use kaelis_api::runtime::RuntimeInstallProgress;
use kaelis_api::updates::{ModUpdate, UpdateReport, apply_update, check_updates};
use kaelis_api::vanilla::InstallProgress;
use kaelis_api::versions::{VersionEntry, VersionManifest, fetch_manifest};
use kaelis_core::automation::{self, Automation, Rule, Trigger};
use kaelis_core::ephemeral_log::EphemeralLog;
use kaelis_core::handoff::HandoffServer;
use kaelis_core::instance::{DuplicateOptions, Instance};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use theme::{accent, accent_on, accent_soft};
use tokio::runtime::Handle;
use tokio::sync::{Semaphore, mpsc, watch};

/// Accent is user-chosen at runtime; see [`theme`]. This is the only violet
/// that is not derived from it: the brand mark's second facet keeps its own
/// identity instead of turning muddy at some hues.
const VIOLET: egui::Color32 = egui::Color32::from_rgb(158, 146, 240);
/// OLED palette. Panels sit at or near true black so a pixel that draws no
/// content is off, and the few in-between tones stay within a narrow band:
/// on an OLED panel a mid-grey card glows like a lamp in a dark room.
const GLASS: egui::Color32 = egui::Color32::from_rgba_premultiplied(7, 8, 10, 244);
const GLASS_LIGHT: egui::Color32 = egui::Color32::from_rgba_premultiplied(14, 16, 20, 246);
const BORDER: egui::Color32 = egui::Color32::from_rgb(22, 25, 31);
const BORDER_BRIGHT: egui::Color32 = egui::Color32::from_rgb(40, 45, 55);
const MUTED: egui::Color32 = egui::Color32::from_rgb(128, 136, 151);
const TEXT: egui::Color32 = egui::Color32::from_rgb(236, 240, 247);
const SURFACE: egui::Color32 = egui::Color32::from_rgba_premultiplied(1, 2, 3, 252);
const SURFACE_ALT: egui::Color32 = egui::Color32::from_rgba_premultiplied(7, 8, 10, 252);
const SURFACE_HOVER: egui::Color32 = egui::Color32::from_rgba_premultiplied(13, 15, 19, 252);
/// Destructive actions (erase, remove) use this pair so danger reads the same
/// in every surface: outlined when idle, filled when hovered.
const DANGER: egui::Color32 = egui::Color32::from_rgb(255, 107, 112);
const DANGER_TINT: (u8, u8, u8) = (255, 99, 105);
/// Soft geometry throughout: controls are capsules, cards are gently rounded
/// and nothing in the shell presents a hard 90° corner.
const CORNER_SM: f32 = 10.0;
const CORNER_MD: f32 = 13.0;
const CORNER_LG: f32 = 15.0;
/// Every button, field, and icon action shares these two numbers, so a row of
/// mixed controls sits on one baseline and reads as the same material. A
/// single geometry is also why the UI cannot drift back into mismatched
/// heights as new controls are added.
const CONTROL_HEIGHT: f32 = 26.0;
const CONTROL_RADIUS: f32 = 11.0;
const CORNER_XL: f32 = 16.0;
/// The shell adapts to the window instead of assuming a desktop-sized canvas.
/// Below this width the navigation rail collapses to glyphs and stacked
/// layouts replace multi-column grids.
const RAIL_WIDTH: f32 = 960.0;
/// Content stops growing past this width; on a wide monitor the extra space
/// becomes margin instead of kilometre-long label rows.
const CONTENT_MAX: f32 = 1180.0;
const SIDEBAR_WIDTH: f32 = 184.0;
const RAIL_SIDEBAR_WIDTH: f32 = 56.0;
const MOD_ICON_BYTES: usize = 1024 * 1024;
const MOD_ICON_SIZE: f32 = 32.0;
const MOD_ICON_CONCURRENCY: usize = 6;
/// Provider thumbnails are at most 512 px; refuse larger decodes outright so
/// a hostile or malformed image cannot stall a decoder thread or spike RAM.
const MOD_ICON_MAX_EDGE: u32 = 1024;
const MOD_ICON_MAX_ALLOC: u64 = 32 * 1024 * 1024;
/// Instance artwork is local, so it may be a little larger than a provider
/// thumbnail, but decoding stays bounded to a single 8 MiB read.
const INSTANCE_ICON_BYTES: u64 = 8 * 1024 * 1024;
/// Stored artwork edge. 256 px covers a 96 px card at 2x and stays small.
const ARTWORK_EDGE: u32 = 256;
/// Typing pauses this long before the mod browser queries both platforms.
///
/// Long enough to fold a burst of keystrokes into one request, short enough
/// that the first result does not feel like a wait; an explicit Enter still
/// searches on the spot.
const MOD_SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);
/// Results requested per provider per page. Paging, not truncation, is what
/// keeps the catalogue browsable.
const MOD_SEARCH_LIMIT: u32 = 30;
const MOD_SEARCH_ID: &str = "kaelis-mod-search";
const MOD_REFINE_ID: &str = "kaelis-mod-refine";
const INSTANCE_FILTER_ID: &str = "kaelis-instance-filter";
const INSTALLED_FILTER_ID: &str = "kaelis-installed-filter";
/// Focus handle for the artwork field, so Ctrl+V can tell an image paste from
/// an ordinary text paste.
const ARTWORK_FIELD_ID: &str = "kaelis-artwork-field";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Instances,
    Content,
    Account,
    Settings,
}

impl Tab {
    const ALL: [Self; 5] = [
        Self::Overview,
        Self::Instances,
        Self::Content,
        Self::Account,
        Self::Settings,
    ];

    fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Instances => "Instances",
            Self::Content => "Content",
            Self::Account => "Account",
            Self::Settings => "Settings",
        }
    }
}

/// The switches the launcher remembers between runs.
///
/// Compared as one value once per frame, so a checkbox can change any of them
/// without every draw site remembering to save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UiSwitches {
    show_snapshots: bool,
    show_alpha: bool,
    show_beta: bool,
    install_dependencies: bool,
    search_modrinth: bool,
    search_curseforge: bool,
    search_as_typed: bool,
}

#[derive(Debug, Default)]
pub struct FrameGovernor {
    pub focused: bool,
    /// Whether a spinner or progress bar is on screen right now.
    pub animating: bool,
}

impl FrameGovernor {
    /// Repaints are only forced while something actually moves. An idle
    /// launcher is fully event-driven, so it costs nothing between clicks.
    pub fn target_fps(&self) -> u16 {
        if self.focused && self.animating {
            60
        } else {
            0
        }
    }

    fn apply(&self, context: &egui::Context) {
        let fps = self.target_fps();
        if fps > 0 {
            context.request_repaint_after(std::time::Duration::from_secs_f64(1.0 / f64::from(fps)));
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StatusKind {
    Info,
    Busy,
    Success,
    Warning,
    Error,
}

impl StatusKind {
    fn color(self) -> egui::Color32 {
        match self {
            Self::Info => egui::Color32::from_rgb(198, 208, 222),
            Self::Busy => accent(),
            Self::Success => egui::Color32::from_rgb(126, 231, 165),
            Self::Warning => egui::Color32::from_rgb(255, 195, 120),
            Self::Error => egui::Color32::from_rgb(255, 133, 133),
        }
    }
}

#[derive(Debug, Clone)]
enum InstanceAction {
    Play,
    /// End this instance's game process now.
    Stop,
    Select,
    Edit,
    /// Pick card artwork straight from the file picker.
    PickIcon,
    InstallVanilla,
    InstallJava,
    Settings,
    OpenFolder,
    OpenMods,
    /// Reveal any folder inside the instance (`mods`, `resourcepacks`, …).
    OpenPath(PathBuf),
    /// Write a portable `.kaelis.zip` for this instance.
    Export,
    ShowJson,
    ManageContent(ContentKind),
    /// Delete the instance's whole tree and record in one step.
    Erase,
    /// Copy the instance into a new one, optionally with its world saves.
    Duplicate {
        saves: bool,
    },
}

/// Where a modpack archive comes from: a provider download staged in the temp
/// folder, or a file the user dropped onto the launcher.
enum ModpackSource {
    // Boxed so the source stays small next to a path or a link; the search
    // item is only read once inside the install task.
    Remote(Box<ModSearchItem>),
    Link(kaelis_api::mods::ProjectLink),
    /// A direct HTTPS link to a `.mrpack` archive.
    Url(String),
    Local(PathBuf),
}

#[derive(Debug, Clone, Copy)]
enum ModFileAction {
    Toggle { disabled: bool },
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstanceFolder {
    Root,
    Mods,
    Saves,
    ResourcePacks,
    ShaderPacks,
    Screenshots,
    Logs,
    Json,
}

impl InstanceFolder {
    fn label(self) -> &'static str {
        match self {
            Self::Root => "Instance folder",
            Self::Mods => "Mods",
            Self::Saves => "Saves",
            Self::ResourcePacks => "Resource packs",
            Self::ShaderPacks => "Shader packs",
            Self::Screenshots => "Screenshots",
            Self::Logs => "Logs",
            Self::Json => "Instance JSON",
        }
    }

    fn relative_path(self) -> Option<&'static str> {
        match self {
            Self::Root | Self::Json => None,
            Self::Mods => Some("mods"),
            Self::Saves => Some("saves"),
            Self::ResourcePacks => Some("resourcepacks"),
            Self::ShaderPacks => Some("shaderpacks"),
            Self::Screenshots => Some("screenshots"),
            Self::Logs => Some("logs"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CheckState {
    Ok,
    Warn,
    Missing,
}

impl CheckState {
    fn color(self) -> egui::Color32 {
        match self {
            Self::Ok => StatusKind::Success.color(),
            Self::Warn => StatusKind::Warning.color(),
            Self::Missing => StatusKind::Error.color(),
        }
    }
}

/// Filesystem readiness, computed when the instance list changes rather than
/// on every frame.
#[derive(Debug, Clone, Copy)]
struct InstanceHealth {
    metadata: CheckState,
    client: CheckState,
    java: CheckState,
    ram: CheckState,
}

impl InstanceHealth {
    fn check(instance: &Instance) -> Self {
        let version = instance.mc_version.as_str();
        let valid_version = !version.is_empty()
            && version.len() <= 64
            && version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
        let version_dir = valid_version.then(|| instance.root.join("versions").join(version));
        let metadata = version_dir
            .as_ref()
            .is_some_and(|dir| dir.join(format!("{version}.json")).is_file());
        let client = version_dir
            .as_ref()
            .is_some_and(|dir| dir.join(format!("{version}.jar")).is_file());
        let java = match instance.java_path.as_ref() {
            Some(path) if path.is_file() => CheckState::Ok,
            Some(_) => CheckState::Missing,
            None if kaelis_api::runtime::managed_java(&instance.root).is_some() => CheckState::Ok,
            None => CheckState::Warn,
        };
        let ram = if (512..=65_536).contains(&instance.ram_mb) {
            CheckState::Ok
        } else {
            CheckState::Missing
        };
        Self {
            metadata: if metadata {
                CheckState::Ok
            } else {
                CheckState::Missing
            },
            client: if client {
                CheckState::Ok
            } else {
                CheckState::Missing
            },
            java,
            ram,
        }
    }
}

/// Presentation strings and readiness for one instance, rebuilt only when the
/// instance list or install state changes.
struct InstanceCard {
    detail: String,
    root: String,
    content: String,
    last_played: String,
    notes: String,
    notes_preview: String,
    health: InstanceHealth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModSort {
    Relevance,
    Downloads,
    Name,
}

#[derive(Debug, Clone, Copy)]
struct ModView {
    sort: ModSort,
    show_modrinth: bool,
    show_curseforge: bool,
}

impl ModSort {
    const ALL: [Self; 3] = [Self::Relevance, Self::Downloads, Self::Name];

    fn label(self) -> &'static str {
        match self {
            Self::Relevance => "Best match",
            Self::Downloads => "Most downloaded",
            Self::Name => "Name A-Z",
        }
    }
}

/// Everything the content tab can ask for during one frame. Buttons only
/// record intent; state changes happen after the panes are drawn so no widget
/// borrows a field another widget is about to rewrite.
#[derive(Default)]
struct ContentRequest {
    search: bool,
    retry_search: bool,
    clear_search: bool,
    reset_filters: bool,
    clear_queue: bool,
    cancel_install: bool,
    cancel_modpack: bool,
    rescan: bool,
    open_folder: bool,
    /// Open an arbitrary folder (instance root, packs, worlds).
    open_path: Option<PathBuf>,
    /// Open the platform file manager to add files to the current channel.
    add_files: bool,
    repair: bool,
    duplicates: bool,
    kind: Option<ContentKind>,
    install: Option<usize>,
    preview: Option<String>,
    file_action: Option<(ModFileAction, PathBuf, bool)>,
    erase: Option<EraseTarget>,
    arm_delete: Option<String>,
    cancel_delete: bool,
    dismiss_report: bool,
    copy_list: bool,
    load_more: bool,
    check_updates: bool,
    apply_update: Option<usize>,
    apply_all_updates: bool,
}

/// One-click removal of a project that is already installed. Only the project
/// identity is captured while drawing; the files are resolved afterwards from
/// the current scan, so a stale row can never delete a path the scan no longer
/// associates with it.
#[derive(Clone)]
struct EraseTarget {
    kind: ContentKind,
    /// Search row key, reused to drop that row's stale install badge.
    key: String,
    slug: String,
    tokens: Vec<String>,
    title: String,
}

/// One queued or running install, bound to the instance it targets so a tab or
/// instance switch can never write into the wrong `mods/` directory.
#[derive(Debug, Clone)]
enum ManualImport {
    Url(String),
    File(PathBuf),
}

#[derive(Clone)]
struct InstallJob {
    key: String,
    instance_id: String,
    item: ModSearchItem,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ModInstallState {
    Queued,
    Running,
    Done { summary: String, warnings: usize },
    Cancelled,
    Failed(String),
}

fn install_state_key(instance_id: &str, item_key: &str) -> String {
    format!("{instance_id}\u{1f}{item_key}")
}

/// Per-search presentation strings, built once instead of on every frame.
#[derive(Clone)]
struct ModRow {
    key: String,
    project_url: String,
    author_line: String,
    meta_line: String,
    categories_line: String,
    compatibility_line: String,
    kind: ContentKind,
    match_tokens: Vec<String>,
    /// Lowercase title + slug + description, used by the instant refine box.
    search_text: String,
    relevance: u8,
}

#[derive(Default)]
pub(crate) struct InstalledScan {
    files: Vec<InstalledFile>,
    summary: String,
    /// Required mod ids that no enabled installed jar provides.
    missing: Vec<String>,
    /// Enabled jars that declare the same mod id more than once.
    conflicts: Vec<String>,
    /// Loader implied by the installed jars, if any.
    detected_loader: Option<&'static str>,
}

struct InstalledFile {
    name: String,
    path: PathBuf,
    tokens: Vec<String>,
    /// Mod ids/names read from `fabric.mod.json`, `quilt.mod.json`,
    /// `META-INF/mods.toml`, or `mcmod.info`, normalized to lowercase
    /// alphanumerics. Includes nested jar-in-jar modules.
    ids: Vec<String>,
    /// Top-level ids only, used for duplicate detection.
    primary_ids: Vec<String>,
    /// Required mod ids declared by Fabric/Quilt metadata.
    requires: Vec<String>,
    disabled: bool,
}

impl InstalledScan {
    /// Prefer metadata ids (which survive renamed jars), then fall back to a
    /// contiguous token match against the filename.
    fn match_file(&self, slug: &str, tokens: &[String]) -> Option<&InstalledFile> {
        let wanted = normalize_mod_id(slug);
        let by_id =
            |file: &&InstalledFile| !wanted.is_empty() && file.ids.iter().any(|id| id == &wanted);
        let by_tokens = |file: &&InstalledFile| {
            !tokens.is_empty()
                && file
                    .tokens
                    .windows(tokens.len())
                    .any(|window| window == tokens)
        };
        self.files
            .iter()
            .filter(|file| !file.disabled)
            .find(by_id)
            .or_else(|| {
                self.files
                    .iter()
                    .filter(|file| !file.disabled)
                    .find(by_tokens)
            })
            .or_else(|| self.files.iter().find(by_id))
            .or_else(|| self.files.iter().find(by_tokens))
    }

    /// Every jar that belongs to one project, so erasing removes duplicates
    /// too. Ids win over filename tokens: a renamed jar whose metadata still
    /// names the project is erased, while a look-alike name is not.
    fn files_matching(&self, slug: &str, tokens: &[String]) -> Vec<&InstalledFile> {
        let wanted = normalize_mod_id(slug);
        let by_id: Vec<&InstalledFile> = if wanted.is_empty() {
            Vec::new()
        } else {
            self.files
                .iter()
                .filter(|file| file.ids.iter().any(|id| id == &wanted))
                .collect()
        };
        if by_id.is_empty() {
            self.match_file(slug, tokens).into_iter().collect()
        } else {
            by_id
        }
    }
}

/// One installed resource pack or shader pack. The mod list keeps its richer
/// metadata scan in [`InstalledScan`], and modpacks are instances of their own.
struct ContentEntry {
    name: String,
    path: PathBuf,
    tokens: Vec<String>,
    disabled: bool,
    detail: String,
    search_text: String,
}

/// Cached non-mod content inventory for the selected instance. Scanning happens
/// on selection, install completion, or an explicit rescan, never per frame.
#[derive(Default)]
pub(crate) struct ContentLibrary {
    resource_packs: Vec<ContentEntry>,
    shaders: Vec<ContentEntry>,
}

impl ContentLibrary {
    fn scan(instance: &Instance) -> Self {
        Self {
            resource_packs: scan_pack_dir(&instance.root.join("resourcepacks")),
            shaders: scan_pack_dir(&instance.root.join("shaderpacks")),
        }
    }

    /// Modpacks are whole instances, so they have no per-instance entry list.
    fn entries(&self, kind: ContentKind) -> &[ContentEntry] {
        match kind {
            ContentKind::ResourcePack => &self.resource_packs,
            ContentKind::Shader => &self.shaders,
            ContentKind::Mod | ContentKind::Modpack => &[],
        }
    }

    fn count(&self, kind: ContentKind, mods: &InstalledScan) -> usize {
        match kind {
            ContentKind::Mod => mods.files.len(),
            _ => self.entries(kind).len(),
        }
    }

    fn match_entry(
        &self,
        kind: ContentKind,
        slug: &str,
        tokens: &[String],
    ) -> Option<&ContentEntry> {
        let wanted = normalize_mod_id(slug);
        let by_slug =
            |entry: &&ContentEntry| !wanted.is_empty() && normalize_mod_id(&entry.name) == wanted;
        let by_tokens = |entry: &&ContentEntry| {
            !tokens.is_empty()
                && entry
                    .tokens
                    .windows(tokens.len())
                    .any(|window| window == tokens)
        };
        self.entries(kind)
            .iter()
            .filter(|entry| !entry.disabled)
            .find(by_slug)
            .or_else(|| {
                self.entries(kind)
                    .iter()
                    .filter(|entry| !entry.disabled)
                    .find(by_tokens)
            })
            .or_else(|| self.entries(kind).iter().find(by_slug))
    }
}

fn scan_pack_dir(directory: &Path) -> Vec<ContentEntry> {
    let mut entries = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                return None;
            }
            let lower = name.to_ascii_lowercase();
            let (stem, disabled) = if let Some(stem) = lower.strip_suffix(".zip.disabled") {
                (stem, true)
            } else if lower.ends_with(".zip") {
                (lower.trim_end_matches(".zip"), false)
            } else if metadata.is_dir() {
                (lower.as_str(), false)
            } else {
                return None;
            };
            let detail = if metadata.is_dir() {
                "Folder".to_owned()
            } else {
                format_bytes(metadata.len())
            };
            Some(ContentEntry {
                search_text: name.to_ascii_lowercase(),
                name,
                path,
                tokens: significant_tokens(stem),
                disabled,
                detail,
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.0} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn normalize_mod_id(value: &str) -> String {
    value
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect()
}

enum ModIconState {
    Loading,
    Ready(egui::TextureHandle),
    Failed,
}

/// A finished modpack import: the instance it created, how many files landed,
/// and the pack's declared version and loader.
pub(crate) struct ModpackOutcome {
    instance: Instance,
    imported: usize,
    pack: kaelis_api::mrpack::PackSummary,
}

/// Erases the half-created instance if the import future is dropped (Cancel)
/// or returns early. The successful path disarms it before building the event.
struct ModpackCleanup(Option<Instance>);

impl Drop for ModpackCleanup {
    fn drop(&mut self) {
        if let Some(instance) = self.0.take() {
            let _ = instance.erase();
        }
    }
}

/// Removes a downloaded pack archive when the import is aborted. Local packs
/// are the user's file and never wrapped in this guard.
struct PackArchiveCleanup(Option<PathBuf>);

impl Drop for PackArchiveCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(crate) enum GuiEvent {
    ModpackInstalled {
        generation: u64,
        result: Box<Result<ModpackOutcome, String>>,
    },
    ModpackProgress {
        generation: u64,
        detail: String,
    },
    InstallStopped,
    /// A jar/content scan finished off the UI thread. `generation` and `id`
    /// together make a late scan from an abandoned selection harmless.
    InstalledScan {
        id: String,
        generation: u64,
        scan: InstalledScan,
        library: ContentLibrary,
    },
    /// The aborted modpack worker has actually stopped, so its cleanup guard
    /// has erased the half-built instance and the list can be re-read safely.
    ModpackStopped,
    DeviceCode(Result<DeviceCodeResponse, String>),
    SignedIn(Result<AuthSession, String>),
    Restored(Result<AuthSession, String>),
    Avatar(Result<egui::ColorImage, String>),
    /// Head portrait for one row of the account list. `None` with
    /// `failed: false` means the name owns no premium skin.
    AccountHead {
        key: String,
        result: Option<egui::ColorImage>,
        failed: bool,
    },
    /// Head resolved from an offline username. `None` means no premium account
    /// owns the name, so the vanilla Steve face stays in place.
    OfflineAvatar {
        name: String,
        result: Option<egui::ColorImage>,
        /// A transport failure must not be cached as "this name has no skin".
        failed: bool,
    },
    ModIcon {
        generation: u64,
        key: String,
        result: Result<egui::ColorImage, String>,
    },
    InstanceIcon {
        id: String,
        result: Result<egui::ColorImage, String>,
    },
    Versions(Result<VersionManifest, String>),
    /// A newer published launcher build, if GitHub has one.
    LauncherRelease(Option<LauncherRelease>),
    /// A verified update finished downloading, or the reason it could not.
    #[cfg(windows)]
    UpdatePrepared(Result<Option<kaelis_api::updater::PreparedUpdate>, String>),
    ModSearch {
        generation: u64,
        query: String,
        results: ModSearchResults,
    },
    ModInstallProgress(String),
    ModInstalled(Result<InstallReport, String>),
    ManualInstalled(Result<InstallReport, String>),
    EnvironmentRepaired(Result<RepairReport, String>),
    DuplicatesResolved(Result<DuplicateRepair, String>),
    /// A mod-update check finished for one instance.
    Updates {
        instance_id: String,
        result: Result<UpdateReport, String>,
    },
    /// One mod update finished downloading and replacing its jar.
    ModUpdated {
        project_id: String,
        title: String,
        result: Result<String, String>,
    },
    VanillaProgress {
        instance_id: String,
        progress: InstallProgress,
    },
    VanillaInstalled {
        instance_id: String,
        result: Result<(), String>,
    },
    RuntimeProgress {
        instance_id: String,
        progress: RuntimeInstallProgress,
    },
    RuntimeInstalled {
        instance_id: String,
        result: Result<PathBuf, String>,
    },
    /// One line of a pre-launch repair pass, plus a download count when a
    /// phase has measurable progress.
    InstanceRepairProgress {
        instance_id: String,
        message: String,
        progress: Option<(usize, usize)>,
    },
    /// A pre-launch repair pass ended. `Ok` carries the summary; a launch the
    /// user asked for continues on either answer.
    InstanceRepaired {
        instance_id: String,
        result: Result<String, String>,
    },
    Notice(String),
    /// Account files were read from another launcher or a picked file.
    AccountsImported(Result<ImportScan, String>),
    /// One finished batch of files added from the file manager, with the
    /// batch label so a mixed drop reads correctly.
    FilesAdded(Result<(InstallReport, String), String>),
    /// A portable instance archive finished exporting or importing.
    Portable(Result<(String, bool), String>),
    /// One automation command finished, in the background.
    Automation(automation::RuleRun),
    /// A picture chosen from the native file picker.
    ArtworkPicked(PathBuf),
    /// Artwork chosen from the file picker for one instance card, applied
    /// immediately instead of waiting for the editor's Save.
    InstanceArtworkPicked {
        id: String,
        path: PathBuf,
    },
    GameStarted(String),
    GameExited {
        instance_id: String,
        message: String,
    },
    GameFailed {
        instance_id: String,
        error: String,
    },
    /// A finished background copy of one instance into a new one.
    InstanceDuplicated(Box<Result<(Instance, String), String>>),
    Handoff(Vec<String>),
    Tray(tray::TrayAction),
}

/// Shared live-game counter that restores the window when the last worker
/// ends, including a worker whose future is dropped or panics.
struct LiveGame {
    count: Arc<AtomicUsize>,
}

impl LiveGame {
    fn start(count: Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self { count }
    }
}

impl Drop for LiveGame {
    fn drop(&mut self) {
        if self.count.fetch_sub(1, Ordering::SeqCst) == 1 {
            platform::restore_now();
        }
    }
}

/// Change the per-user sign-in entry from the CLI or a diagnostic tool.
///
/// One implementation behind both the Settings checkbox and `--startup`, so
/// the two can never disagree about the registry value.
pub fn set_launch_at_startup(enabled: bool) -> Result<(), String> {
    platform::set_startup(enabled)
}

pub struct KaelisApp {
    http: reqwest::Client,
    platforms: ModPlatformClient,
    runtime: Handle,
    sender: mpsc::UnboundedSender<GuiEvent>,
    receiver: mpsc::UnboundedReceiver<GuiEvent>,
    _tray: Option<tray::TrayController>,
    sweeper: Option<tokio::task::JoinHandle<()>>,
    governor: FrameGovernor,
    /// When the window last lost focus; the idle hibernation timer.
    unfocused_since: Option<std::time::Instant>,
    /// Set after one idle trim, so pages are released once per focus loss.
    idle_trimmed: bool,
    /// A portable archive is being written or unpacked.
    portable_busy: bool,
    /// Folder of the last export, so a second one starts where the first went.
    last_export_dir: Option<PathBuf>,
    current_tab: Tab,
    status: String,
    status_kind: StatusKind,
    instances: Vec<Instance>,
    instance_cards: HashMap<String, InstanceCard>,
    instance_icons: HashMap<String, ModIconState>,
    /// Source path behind each cached icon, so a reload can keep decoded
    /// textures instead of re-reading every instance picture.
    instance_icon_paths: HashMap<String, PathBuf>,
    editing: Option<Instance>,
    edit_icon: String,
    edit_java: String,
    edit_jvm_args: String,
    edit_game_args: String,
    edit_env: String,
    edit_min_ram: u32,
    edit_custom_min_ram: bool,
    edit_custom_window: bool,
    edit_window_width: u32,
    edit_window_height: u32,
    edit_notes: String,
    edit_group: String,
    edit_quick_join: String,
    edit_pre_launch: String,
    edit_post_exit: String,
    selected_instance: Option<String>,
    installing_instance: Option<String>,
    installing_runtime: Option<String>,
    new_instance_name: String,
    new_instance_version: String,
    new_instance_loader: String,
    versions: Vec<VersionEntry>,
    show_alpha: bool,
    show_beta: bool,
    show_snapshots: bool,
    content_kind: ContentKind,
    /// On a narrow window the content studio shows one pane at a time; this
    /// remembers whether the installed list or the browser is in front.
    content_show_installed: bool,
    content_library: ContentLibrary,
    manual_url: String,
    manual_path: String,
    manual_status: String,
    manual_busy: bool,
    content_import_open: bool,
    preview_key: Option<String>,
    mod_query: String,
    mod_loader: String,
    mod_results: ModSearchResults,
    mod_results_query: String,
    mod_rows: Vec<ModRow>,
    mod_search_generation: u64,
    /// Pages requested from the providers so far; the next page starts at
    /// `mod_pages * MOD_SEARCH_LIMIT`.
    mod_pages: u32,
    /// Whether the last page filled up, so another "Load more" is worthwhile.
    mod_has_more: bool,
    mod_pending_signature: String,
    mod_last_request: String,
    mod_signature_changed_at: Option<std::time::Instant>,
    mod_auto_search: bool,
    mod_refine: String,
    mod_sort: ModSort,
    mod_show_modrinth: bool,
    mod_show_curseforge: bool,
    installed: Option<InstalledScan>,
    /// Bumped for every scan request so a scan that finishes after the user
    /// selected another instance cannot overwrite the newer result.
    installed_scan_generation: u64,
    /// Kept so a background scan can wake the window when it finishes; the
    /// context is cheap to clone and does not borrow the frame.
    egui_context: egui::Context,
    installed_filter: String,
    installed_missing_only: bool,
    mod_icons: HashMap<String, ModIconState>,
    mod_icon_slots: Arc<Semaphore>,
    mod_icon_generation: u64,
    mod_icon_epoch: Arc<AtomicU64>,
    installing_job: Option<InstallJob>,
    install_task: Option<tokio::task::JoinHandle<()>>,
    install_queue: VecDeque<InstallJob>,
    install_states: HashMap<String, ModInstallState>,
    install_detail: Option<String>,
    install_started_at: Option<std::time::Instant>,
    install_dependencies: bool,
    last_install: Option<InstallReport>,
    install_progress: Option<(usize, usize)>,
    confirm_mod_delete: Option<String>,
    repairing_mods: bool,
    auto_repair_before_launch: bool,
    auto_resolve_duplicates: bool,
    /// Set when launch waits for the automatic dependency repair to finish.
    pending_launch: Option<(Instance, LaunchIdentity)>,
    detected_loader: Option<String>,
    instance_filter: String,
    search_busy: bool,
    auth_busy: bool,
    auth_status: String,
    auth_device: Option<DeviceCodeResponse>,
    /// When the shown code stops being accepted, so the panel can count down
    /// instead of failing only when the poll expires.
    auth_device_until: Option<std::time::Instant>,
    auth_code_copied: bool,
    auth_cancel: Option<watch::Sender<bool>>,
    microsoft_session: Option<AuthSession>,
    last_account: String,
    account_mode: AccountMode,
    offline_account: Option<OfflineProfile>,
    offline_name: String,
    avatar: Option<egui::TextureHandle>,
    /// Username → head cache for offline profiles. `None` records a name no
    /// premium account owns, so retyping it never hits the network again.
    offline_skins: HashMap<String, Option<egui::ColorImage>>,
    offline_skin_pending: Option<String>,
    /// Every offline head request in flight, so the account list and the
    /// active avatar never look up the same username twice.
    offline_skin_inflight: HashSet<String>,
    offline_skin_due: Option<std::time::Instant>,
    offline_typing_avatar: Option<egui::TextureHandle>,
    offline_typing_key: String,
    /// Saved identities, newest last. Microsoft and offline rows share one
    /// list so switching is a single click either way.
    accounts: Vec<SavedAccount>,
    /// Head portraits per account key. `Some(None)` records a name with no
    /// premium skin, so Steve is drawn without another request; a missing key
    /// means the head has not arrived yet.
    account_heads: HashMap<String, Option<egui::TextureHandle>>,
    account_head_pending: HashSet<String>,
    /// Names whose head request failed, with the attempt time. A failure is
    /// retried after a minute instead of on the next frame.
    account_head_retry: HashMap<String, std::time::Instant>,
    /// Shared vanilla face for rows whose name owns no premium skin, so those
    /// rows still read as a player instead of a letter.
    steve_head_texture: Option<egui::TextureHandle>,
    /// Instances with a live game process, by id. Several may run at once;
    /// the window only sleeps in the tray while at least one is alive.
    running_instances: std::collections::HashSet<String>,
    /// Stop handle per running instance. Sending `true` ends that JVM inside
    /// its own launch task, so Quit never waits on a window, a frame, or a
    /// second process.
    stop_signals: HashMap<String, tokio::sync::watch::Sender<bool>>,
    /// Live game count shared with launch workers, so only the worker that
    /// ends the last game restores the hidden window.
    live_games: Arc<AtomicUsize>,
    settings_ram_mb: u32,
    settings_jvm_args: String,
    /// Whole-interface scale; `0.0` means automatic. Mirrors egui's zoom
    /// factor so the choice survives a restart.
    ui_scale: f32,
    /// The automatic step has not been resolved for this window yet.
    auto_scale_pending: bool,
    /// Display height at 100 % zoom, in native-DPI points. Used to notice a
    /// monitor or window change without reacting to the zoom it produces.
    auto_basis: f32,
    /// Keyboard navigation is in use, so focused controls paint a ring.
    keyboard_nav: bool,
    /// Zoom last pushed into egui, so a Ctrl +/- change can be persisted
    /// without rewriting settings on every idle frame.
    applied_zoom: f32,
    screenshot_path: Option<PathBuf>,
    screenshot_frame: u32,
    screenshot_at: u32,
    /// Title of the modpack being downloaded and imported, if any.
    modpack_installing: Option<String>,
    modpack_detail: String,
    /// The import worker, so Cancel can stop it instead of waiting for the
    /// whole pack. Aborting drops its cleanup guard, which erases the partial
    /// instance root.
    modpack_task: Option<tokio::task::JoinHandle<()>>,
    /// Invalidates progress and completion events from a cancelled import.
    modpack_generation: u64,
    /// A per-user sign-in entry starts Kaelis hidden in the tray.
    launch_at_startup: bool,
    /// Live inner size of the window in logical points, tracked so a settled
    /// resize can be written to settings.
    window_size: [f32; 2],
    /// When the size has stopped changing, the write happens; `None` means the
    /// size on disk is already current.
    window_size_due: Option<std::time::Instant>,
    /// The persisted switches as they were last written, compared once per
    /// frame so any checkbox can change them without its own save call.
    ui_switches_saved: UiSwitches,
    /// Whether the compositor has been asked for rounded corners yet, and how
    /// many frames the ask has been retried for.
    #[cfg(windows)]
    window_rounded: bool,
    #[cfg(windows)]
    window_round_tries: u8,
    /// The window is hidden because a game started, not by the user. Only this
    /// case restores the window automatically when the last game exits.
    hidden_for_game: bool,
    /// Latest mod-update report and the instance it belongs to, so switching
    /// instances can never show another instance's updates.
    update_report: Option<(String, UpdateReport)>,
    updates_busy: bool,
    /// Newest GitHub release newer than this build; `None` when up to date or
    /// when the anonymous check could not reach GitHub.
    launcher_release: Option<LauncherRelease>,
    /// An update is downloading, verifying, or being installed right now.
    update_busy: bool,
    /// `current_file` of every jar being replaced right now.
    updating_mods: HashSet<String>,
    /// User commands: named shell lines fired by an event or an interval.
    automation: Automation,
    /// Last few automation results, newest last. Bounded; never a log file.
    automation_log: Vec<String>,
    automation_draft_name: String,
    automation_draft_command: String,
    automation_draft_when: String,
    /// Rule id being edited, so Save replaces instead of adding a duplicate.
    automation_editing: Option<String>,
    /// Startup rules run on the first frame only.
    automation_started: bool,
}

/// Install the visual language: OLED-black surfaces, one control radius, and
/// selection colours from the fixed Kaelis blue.
///
/// Called once at startup. Keeping every colour in one place means a visual
/// change cannot leave a stale hyperlink or focus ring behind, which is what
/// happens when each widget is edited in isolation.
fn install_style(context: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = egui::Color32::TRANSPARENT;
    visuals.window_fill = GLASS;
    visuals.window_rounding = egui::Rounding::same(CORNER_LG);
    visuals.menu_rounding = egui::Rounding::same(CORNER_MD);
    // Flat surfaces and hairline strokes do the separating. A drop shadow
    // under every card is the fastest way to make a dark UI look muddy, it
    // lights up pixels an OLED panel would otherwise switch off, and it is
    // feathered geometry the painter has to tessellate on every open menu.
    // Removing it costs nothing and saves all three.
    visuals.window_shadow = egui::epaint::Shadow::NONE;
    visuals.popup_shadow = egui::epaint::Shadow::NONE;
    // No global text override: egui derives placeholder/hint text from the
    // widget foreground, so an override would make every hint look like a
    // real value. Colour the widget states instead.
    visuals.override_text_color = None;
    let text_stroke = egui::Stroke::new(1.0_f32, TEXT);
    visuals.widgets.noninteractive.fg_stroke = text_stroke;
    visuals.widgets.inactive.fg_stroke = text_stroke;
    visuals.widgets.hovered.fg_stroke = text_stroke;
    visuals.widgets.active.fg_stroke = text_stroke;
    visuals.widgets.open.fg_stroke = text_stroke;
    // Slider rails and progress tracks use this fill; a transparent value
    // left them invisible on the dark surface.
    visuals.widgets.noninteractive.bg_fill = egui::Color32::from_rgb(17, 20, 25);
    visuals.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(15, 17, 22);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(15, 17, 22);
    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(25, 29, 36);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(25, 29, 36);
    visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgb(32, 37, 45);
    visuals.widgets.active.bg_fill = egui::Color32::from_rgb(32, 37, 45);
    // A hairline makes a control legible without turning it into a box.
    // It also gives checkboxes a visible well: their fill alone vanished
    // against the near-black surface.
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, BORDER_BRIGHT);
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, BORDER_BRIGHT);
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0_f32, BORDER_BRIGHT);
    visuals.extreme_bg_color = egui::Color32::BLACK;
    visuals.faint_bg_color = egui::Color32::from_rgb(8, 9, 12);
    // One radius for every control, one step tighter than the cards: inputs
    // and buttons read as the same material without looking pill-shaped.
    let control_rounding = egui::Rounding::same(CONTROL_RADIUS);
    visuals.widgets.inactive.rounding = control_rounding;
    visuals.widgets.hovered.rounding = control_rounding;
    visuals.widgets.active.rounding = control_rounding;
    visuals.widgets.open.rounding = control_rounding;
    visuals.selection.bg_fill = theme::accent_alpha(64);
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, accent_soft());
    visuals.hyperlink_color = accent();
    context.set_visuals(visuals);
    let mut style = (*context.style()).clone();
    // Two pixels tighter on both axes than the old shell. Sixty rows of
    // settings, mods, or instances fit without changing a single label.
    style.spacing.item_spacing = egui::vec2(7.0, 5.0);
    // Compact controls: two pixels of side padding less than the old shell,
    // one less vertically. Every text button (Install, Create, Search, Copy,
    // Erase) loses the same dead margin, and the 26 px interaction height keeps
    // its centre line, so the rows only get shorter.
    style.spacing.button_padding = egui::vec2(9.0, 3.0);
    style.spacing.interact_size.y = CONTROL_HEIGHT;
    style.spacing.slider_width = 190.0;
    style.spacing.scroll.bar_width = 4.0;
    style.spacing.scroll.floating = true;
    style.spacing.indent = 12.0;
    style.spacing.menu_margin = egui::Margin::same(5.0);
    for widget in [
        &mut style.visuals.widgets.inactive,
        &mut style.visuals.widgets.hovered,
        &mut style.visuals.widgets.active,
    ] {
        widget.expansion = 0.0;
    }
    style.visuals.window_stroke = egui::Stroke::new(1.0_f32, BORDER);
    style
        .text_styles
        .insert(egui::TextStyle::Heading, egui::FontId::proportional(20.0));
    style
        .text_styles
        .insert(egui::TextStyle::Body, egui::FontId::proportional(12.0));
    style
        .text_styles
        .insert(egui::TextStyle::Button, egui::FontId::proportional(11.0));
    style
        .text_styles
        .insert(egui::TextStyle::Small, egui::FontId::proportional(9.5));
    // The local `visuals` above carries the widget states; `style.visuals` is
    // the copy egui will actually paint, so the border and text styles applied
    // here are the ones that survive.
    context.set_style(style);
}

impl KaelisApp {
    pub fn new(
        creation: &eframe::CreationContext<'_>,
        http: reqwest::Client,
        runtime: Handle,
        sweeper: tokio::task::JoinHandle<()>,
        handoff: Option<HandoffServer>,
        start_hidden: bool,
    ) -> Self {
        // Settings no longer asks for a CurseForge key, so any key an older
        // build stored is removed once instead of sitting unused.
        kaelis_api::secrets::forget_legacy_curseforge_key();
        // A finished update leaves its script and, after an aborted download, a
        // partial file behind. Only files old enough to be abandoned are
        // touched, so a swap that is still running is never disturbed.
        kaelis_api::updater::purge_staging();
        // An update that replaced this build in place renamed the old one aside
        // while it was still running, so the backup could only be deleted once
        // that process was gone — which is now: this start-up waited for it.
        kaelis_api::updater::purge_old_build();
        // Windows paints the tray menu itself, so the dark preference has to
        // be installed before that menu is created.
        platform::enable_dark_menus();
        let launcher_settings = load_settings();
        if launcher_settings.launch_at_startup {
            // A portable folder that was moved leaves the old path behind;
            // rewriting it at start keeps the sign-in entry pointing here.
            let _ = platform::set_startup(true);
        }
        let (sender, receiver) = mpsc::unbounded_channel();
        if let Some(server) = handoff {
            let events = sender.clone();
            let context = creation.egui_ctx.clone();
            let _ = std::thread::Builder::new()
                .name("kaelis-handoff-bridge".to_owned())
                .spawn(move || {
                    // Blocks while idle, so a hidden viewport wakes only when
                    // another launch actually hands arguments over.
                    while let Some(arguments) = server.recv() {
                        show_window(&context);
                        context.request_repaint();
                        if events.send(GuiEvent::Handoff(arguments)).is_err() {
                            break;
                        }
                    }
                });
        }
        let tray = tray::TrayController::install(creation.egui_ctx.clone(), sender.clone()).ok();
        if start_hidden {
            platform::begin_hidden_start(creation.egui_ctx.clone());
        }
        // Windows listens for real foreground changes rather than expecting a
        // frame after focus loss, which an idle launcher never schedules.
        platform::start_idle_guard();
        install_style(&creation.egui_ctx);

        let instances = Instance::load_all().unwrap_or_default();
        let selected_instance = instances.first().map(|instance| instance.id.clone());
        let screenshot_path = std::env::var_os("KAELIS_SCREENSHOT").map(PathBuf::from);
        let current_tab = match std::env::var("KAELIS_SCREENSHOT_TAB")
            .unwrap_or_default()
            .as_str()
        {
            "instances" => Tab::Instances,
            "content" | "install" | "mods" => Tab::Content,
            "account" => Tab::Account,
            "settings" => Tab::Settings,
            _ => Tab::Overview,
        };
        let saved_scale = launcher_settings.ui_scale.clamp(0.0, 2.0);
        if saved_scale > 0.0 {
            // A remembered manual scale wins over the automatic step.
            creation.egui_ctx.set_zoom_factor(saved_scale);
        }
        let accounts = migrated_accounts(&launcher_settings);
        let saved_offline = launcher_settings.offline_account.trim();
        let saved_offline_valid = valid_username(saved_offline);
        // Older settings files have no explicit mode. A saved Microsoft token
        // keeps its previous precedence; otherwise an offline profile does.
        let account_mode = resolved_mode(&launcher_settings);
        // Restore the local profile only when it was the last chosen identity;
        // a saved Microsoft token must not silently replace it.
        let restored_offline = (account_mode == AccountMode::Offline && saved_offline_valid)
            .then(|| offline_profile(saved_offline));
        let initial_auth_status = match &restored_offline {
            Some(profile) => format!("Offline profile ready: {}", profile.name),
            None => "Choose Microsoft sign-in or an offline profile.".to_owned(),
        };
        // Both integrations are zero-setup now: Microsoft uses a one-click code
        // and CurseForge answers through the public mirror of its v1 API.
        let status = if tray.is_some() {
            "Ready".to_owned()
        } else {
            "Ready · system tray unavailable".to_owned()
        };
        // Restore the last Microsoft account from the OS keychain without a
        // device-code round trip. A stale or missing token simply leaves the
        // account signed out; the Account tab still offers a fresh sign-in.
        if account_mode == AccountMode::Microsoft && !launcher_settings.last_account.is_empty() {
            let restore_name = launcher_settings.last_account.clone();
            let sender = sender.clone();
            let repaint = creation.egui_ctx.clone();
            runtime.spawn(async move {
                let result = match MicrosoftAuth::new() {
                    Ok(auth) => auth
                        .login_with_refresh_token(&restore_name)
                        .await
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                };
                send_event(&sender, &repaint, GuiEvent::Restored(result));
            });
        }
        // Automation is the one file users are expected to edit by hand, so it
        // loads on its own and a parse failure becomes a visible line instead
        // of a crash or a silent empty list.
        let (automation_store, automation_warning) = Automation::load();
        let automation_log = automation_warning.into_iter().collect::<Vec<_>>();
        let mut app = Self {
            platforms: ModPlatformClient::new(http.clone()),
            http,
            runtime,
            sender,
            receiver,
            _tray: tray,
            sweeper: Some(sweeper),
            governor: FrameGovernor {
                focused: true,
                animating: false,
            },
            unfocused_since: None,
            idle_trimmed: false,
            portable_busy: false,
            last_export_dir: None,
            current_tab,
            status,
            status_kind: StatusKind::Info,
            instances,
            instance_cards: HashMap::new(),
            instance_icons: HashMap::new(),
            instance_icon_paths: HashMap::new(),
            editing: None,
            edit_icon: String::new(),
            edit_java: String::new(),
            edit_jvm_args: String::new(),
            edit_game_args: String::new(),
            edit_env: String::new(),
            edit_min_ram: 512,
            edit_custom_min_ram: false,
            edit_custom_window: false,
            edit_window_width: 1280,
            edit_window_height: 720,
            edit_notes: String::new(),
            edit_group: String::new(),
            edit_quick_join: String::new(),
            edit_pre_launch: String::new(),
            edit_post_exit: String::new(),
            selected_instance,
            installing_instance: None,
            installing_runtime: None,
            // Screenshot fixtures can pre-fill the name to capture the
            // conflict state without automating keyboard input.
            new_instance_name: std::env::var("KAELIS_SCREENSHOT_NAME").unwrap_or_default(),
            new_instance_version: String::new(),
            new_instance_loader: "vanilla".to_owned(),
            versions: Vec::new(),
            show_alpha: launcher_settings.show_alpha,
            show_beta: launcher_settings.show_beta,
            show_snapshots: launcher_settings.show_snapshots,
            content_kind: match std::env::var("KAELIS_SCREENSHOT_KIND")
                .unwrap_or_default()
                .as_str()
            {
                "resourcepack" | "resourcepacks" => ContentKind::ResourcePack,
                "shader" | "shaders" => ContentKind::Shader,
                "modpack" | "modpacks" => ContentKind::Modpack,
                "mod" | "mods" => ContentKind::Mod,
                _ => launcher_settings.content_kind,
            },
            content_show_installed: std::env::var_os("KAELIS_SCREENSHOT_INSTALLED").is_some(),
            content_library: ContentLibrary::default(),
            manual_url: String::new(),
            manual_path: String::new(),
            manual_status: String::new(),
            manual_busy: false,
            content_import_open: false,
            preview_key: None,
            mod_query: String::new(),
            mod_loader: "fabric".to_owned(),
            mod_results: ModSearchResults::default(),
            mod_results_query: String::new(),
            mod_rows: Vec::new(),
            mod_search_generation: 0,
            mod_pages: 0,
            mod_has_more: false,
            mod_pending_signature: String::new(),
            mod_last_request: String::new(),
            mod_signature_changed_at: None,
            mod_auto_search: launcher_settings.search_as_typed,
            mod_refine: String::new(),
            mod_sort: ModSort::Relevance,
            mod_show_modrinth: launcher_settings.search_modrinth,
            mod_show_curseforge: launcher_settings.search_curseforge,
            installed: None,
            installed_scan_generation: 0,
            egui_context: creation.egui_ctx.clone(),
            installed_filter: String::new(),
            installed_missing_only: false,
            mod_icons: HashMap::new(),
            mod_icon_slots: Arc::new(Semaphore::new(MOD_ICON_CONCURRENCY)),
            mod_icon_generation: 0,
            mod_icon_epoch: Arc::new(AtomicU64::new(0)),
            installing_job: None,
            install_task: None,
            install_queue: VecDeque::new(),
            install_states: HashMap::new(),
            install_detail: None,
            install_started_at: None,
            install_dependencies: launcher_settings.install_dependencies,
            last_install: None,
            install_progress: None,
            confirm_mod_delete: None,
            repairing_mods: false,
            auto_repair_before_launch: launcher_settings.auto_repair,
            auto_resolve_duplicates: launcher_settings.auto_resolve_duplicates,
            pending_launch: None,
            detected_loader: None,
            instance_filter: String::new(),
            search_busy: false,
            auth_busy: false,
            auth_status: initial_auth_status,
            auth_device: None,
            auth_device_until: None,
            auth_code_copied: false,
            auth_cancel: None,
            microsoft_session: None,
            last_account: launcher_settings.last_account,
            account_mode,
            offline_account: restored_offline.clone(),
            // Keep the saved name even when a Microsoft session is active, so
            // switching back to offline needs no retyping.
            offline_name: if saved_offline_valid {
                saved_offline.to_owned()
            } else {
                "Player".to_owned()
            },
            avatar: restored_offline.as_ref().map(|_| {
                creation.egui_ctx.load_texture(
                    "kaelis-profile-head",
                    steve_head(),
                    egui::TextureOptions::NEAREST,
                )
            }),
            offline_skins: HashMap::new(),
            offline_skin_pending: None,
            offline_skin_inflight: HashSet::new(),
            offline_skin_due: None,
            offline_typing_avatar: None,
            offline_typing_key: String::new(),
            accounts,
            account_heads: HashMap::new(),
            account_head_pending: HashSet::new(),
            account_head_retry: HashMap::new(),
            steve_head_texture: None,
            running_instances: std::collections::HashSet::new(),
            hidden_for_game: false,
            stop_signals: HashMap::new(),
            live_games: Arc::new(AtomicUsize::new(0)),
            settings_ram_mb: 4096,
            settings_jvm_args: String::new(),
            ui_scale: saved_scale,
            auto_scale_pending: saved_scale <= 0.0,
            auto_basis: 0.0,
            keyboard_nav: false,
            applied_zoom: if saved_scale > 0.0 { saved_scale } else { 1.0 },
            screenshot_path,
            screenshot_frame: 0,
            // Async work (a live mod search, an install) needs more than the
            // default eight frames before it is worth capturing.
            screenshot_at: std::env::var("KAELIS_SCREENSHOT_FRAME")
                .ok()
                .and_then(|value| value.parse::<u32>().ok())
                .filter(|frames| *frames > 0)
                .unwrap_or(8),
            modpack_installing: None,
            modpack_detail: String::new(),
            modpack_task: None,
            modpack_generation: 0,
            launch_at_startup: launcher_settings.launch_at_startup,
            window_size: launcher_settings.window_size,
            window_size_due: None,
            #[cfg(windows)]
            window_rounded: false,
            #[cfg(windows)]
            window_round_tries: 0,
            ui_switches_saved: UiSwitches {
                show_snapshots: launcher_settings.show_snapshots,
                show_alpha: launcher_settings.show_alpha,
                show_beta: launcher_settings.show_beta,
                install_dependencies: launcher_settings.install_dependencies,
                search_modrinth: launcher_settings.search_modrinth,
                search_curseforge: launcher_settings.search_curseforge,
                search_as_typed: launcher_settings.search_as_typed,
            },
            update_report: None,
            updates_busy: false,
            launcher_release: None,
            update_busy: false,
            updating_mods: HashSet::new(),
            automation: automation_store,
            automation_log,
            automation_draft_name: String::new(),
            automation_draft_command: String::new(),
            automation_draft_when: "before launch".to_owned(),
            automation_editing: None,
            automation_started: false,
        };
        // A game outlives a launcher that was closed, so recover the running
        // set from its process records before the first frame paints.
        app.adopt_running_games(&creation.egui_ctx);
        // First run on this computer: pick up the identities another launcher
        // already has, so setup is a click instead of retyping a name. The
        // scan is silent when there is nothing to find.
        if app.accounts.is_empty() {
            app.scan_for_accounts(&creation.egui_ctx, false);
        }
        app.clean_junk_now(creation.egui_ctx.clone());
        app.refresh_instance_cards();
        if let Some(id) = app.selected_instance.clone() {
            app.select_instance(&id);
        }
        app.reload_instance_icons(&creation.egui_ctx);
        // Jar metadata is read off the UI thread: a 200-mod instance used to
        // hold the first frame for every zip it opened.
        app.refresh_installed();
        app.request_versions(creation.egui_ctx.clone());
        // One anonymous request in the background: no startup delay, no error
        // banner when offline, and no auto-install. The footer links to the
        // release only when GitHub actually has a newer tag.
        if app.screenshot_path.is_none() {
            app.check_launcher_release(creation.egui_ctx.clone());
        }
        // A restored offline profile keeps its Steve face until the premium
        // skin for its name (if any) arrives.
        if let Some(profile) = app.offline_account.clone() {
            app.request_offline_skin(&creation.egui_ctx, &profile.name);
        }
        app
    }

    fn request_versions(&self, context: egui::Context) {
        let sender = self.sender.clone();
        let repaint = context.clone();
        let client = self.http.clone();
        self.runtime.spawn(async move {
            let result = fetch_manifest(&client)
                .await
                .map_err(|error| error.to_string());
            send_event(&sender, &repaint, GuiEvent::Versions(result));
        });
    }

    fn check_launcher_release(&self, context: egui::Context) {
        let sender = self.sender.clone();
        let repaint = context;
        let client = self.http.clone();
        self.runtime.spawn(async move {
            let release = kaelis_api::release::latest(&client)
                .await
                .ok()
                .filter(|release| {
                    kaelis_api::release::is_newer(env!("CARGO_PKG_VERSION"), &release.version)
                });
            send_event(&sender, &repaint, GuiEvent::LauncherRelease(release));
        });
    }

    /// Download and install the published build.
    ///
    /// The artifact is verified against the release's published SHA-256 before
    /// it can replace this executable; a failed download or a checksum mismatch
    /// leaves the running build untouched and keeps the footer link as the
    /// manual fallback.
    #[cfg(windows)]
    fn start_self_update(&mut self, context: egui::Context) {
        if self.update_busy {
            return;
        }
        self.update_busy = true;
        self.set_status(StatusKind::Busy, "Downloading the latest Kaelis…");
        let sender = self.sender.clone();
        let repaint = context;
        let client = self.http.clone();
        self.runtime.spawn(async move {
            let result = kaelis_api::updater::prepare(&client)
                .await
                .map_err(|error| error.to_string());
            send_event(&sender, &repaint, GuiEvent::UpdatePrepared(result));
        });
    }

    /// Hand a verified build to the swap script and end this process.
    ///
    /// Exiting is part of installing: the script waits for this pid to vanish
    /// before renaming the executable, so lingering here would stall the swap.
    #[cfg(windows)]
    fn install_prepared_update(
        &mut self,
        update: Option<kaelis_api::updater::PreparedUpdate>,
        context: &egui::Context,
    ) {
        let Some(update) = update else {
            self.update_busy = false;
            self.set_status(StatusKind::Success, "Kaelis is already up to date.");
            return;
        };
        match kaelis_api::updater::apply(&update) {
            Ok(()) => {
                self.set_status(
                    StatusKind::Success,
                    format!("Restarting into Kaelis {}…", update.version),
                );
                self.quit_now()
            }
            Err(error) => {
                self.update_busy = false;
                self.set_status(
                    StatusKind::Warning,
                    format!("Download the new build from the release page: {error}"),
                );
                if let Some(release) = &self.launcher_release {
                    context.open_url(egui::OpenUrl::new_tab(release.url.clone()));
                }
            }
        }
    }

    fn start_sign_in(&mut self, context: egui::Context) {
        if self.auth_busy {
            return;
        }
        let (cancel, cancel_receiver) = watch::channel(false);
        self.auth_cancel = Some(cancel);
        self.auth_busy = true;
        self.auth_status = "Requesting a Microsoft device code…".to_owned();
        let sender = self.sender.clone();
        let repaint = context;
        self.runtime.spawn(async move {
            let auth = match MicrosoftAuth::new() {
                Ok(auth) => auth,
                Err(error) => {
                    send_event(
                        &sender,
                        &repaint,
                        GuiEvent::DeviceCode(Err(error.to_string())),
                    );
                    return;
                }
            };
            let device = match auth.request_device_code().await {
                Ok(device) => device,
                Err(error) => {
                    send_event(
                        &sender,
                        &repaint,
                        GuiEvent::DeviceCode(Err(error.to_string())),
                    );
                    return;
                }
            };
            send_event(&sender, &repaint, GuiEvent::DeviceCode(Ok(device.clone())));
            let result = auth
                .login_with_device_code_cancellable(&device, cancel_receiver)
                .await
                .map_err(|error| error.to_string());
            send_event(&sender, &repaint, GuiEvent::SignedIn(result));
        });
    }

    /// Install a direct URL or a local file into the selected content channel.
    /// This bypasses provider metadata entirely, so it works for hand-built
    /// mods, GitHub release assets, private packs, and world archives.
    fn start_manual_import(&mut self, source: ManualImport, context: egui::Context) {
        if self.manual_busy {
            self.set_status(StatusKind::Warning, "Another import is already running.");
            return;
        }
        let Some(instance) = self.selected_instance().cloned() else {
            self.manual_status = "Select an instance before importing content.".to_owned();
            self.set_status(
                StatusKind::Warning,
                "Select an instance before importing content.",
            );
            return;
        };
        let label = match &source {
            ManualImport::Url(url) => url.clone(),
            ManualImport::File(path) => path.display().to_string(),
        };
        if let ManualImport::Url(url) = &source
            && let Some(name) = url.split(['?', '#']).next()
            && name.to_ascii_lowercase().ends_with(".mrpack")
        {
            self.content_kind = ContentKind::Modpack;
            self.reset_browse();
            self.persist_settings();
            let title = name
                .rsplit('/')
                .next()
                .map(|file| {
                    title_case(
                        file.trim_end_matches(".mrpack")
                            .replace(['-', '_'], " ")
                            .as_str(),
                    )
                })
                .unwrap_or_else(|| "Modpack".to_owned());
            let id = self.unique_instance_id(&slugify(&title));
            let game_version = self.browse_game_version().unwrap_or_default();
            self.begin_modpack_install(
                context,
                title,
                id,
                game_version,
                ModpackSource::Url(url.clone()),
            );
            return;
        }
        // A project page is resolved through its provider API; only true file
        // links are streamed, and local files are copied after archive checks.
        let project = match &source {
            ManualImport::Url(url) => kaelis_api::mods::parse_project_link(url),
            ManualImport::File(path) => {
                if path.extension().is_some_and(|extension| {
                    extension.to_string_lossy().eq_ignore_ascii_case("mrpack")
                }) {
                    self.content_kind = ContentKind::Modpack;
                    self.reset_browse();
                    self.persist_settings();
                    self.start_local_modpack_install(context, path.clone());
                    return;
                }
                None
            }
        };
        // A pack link is a whole instance, so it never goes through the
        // per-instance content installer.
        if let Some(link) = project.clone()
            && link.kind == ContentKind::Modpack
        {
            self.content_kind = ContentKind::Modpack;
            self.reset_browse();
            self.persist_settings();
            let title = title_case(&link.slug);
            let id = self.unique_instance_id(&slugify(&link.slug));
            self.begin_modpack_install(
                context,
                title,
                id,
                instance.mc_version.clone(),
                ModpackSource::Link(link),
            );
            return;
        }
        let kind = project.as_ref().map_or(self.content_kind, |link| link.kind);
        if kind != self.content_kind {
            self.content_kind = kind;
            self.reset_browse();
        }
        let loader = self.resolved_mod_loader();
        let game_version = instance.mc_version.clone();
        let include_dependencies = self.install_dependencies;
        let platforms = self.platforms.clone();
        let sender = self.sender.clone();
        let repaint = context;
        let root = instance.root.clone();
        self.manual_busy = true;
        self.manual_status = match &project {
            Some(link) => format!(
                "Resolving {} from {} into {}…",
                link.slug,
                link.provider,
                kind.directory()
            ),
            None => format!("Importing {label} into {}…", kind.directory()),
        };
        self.runtime.spawn(async move {
            let result = if let Some(link) = project {
                platforms
                    .install_project_link(
                        &link,
                        &game_version,
                        &loader,
                        &root,
                        include_dependencies,
                    )
                    .await
            } else {
                match source {
                    ManualImport::Url(url) => platforms.install_from_url(kind, &root, &url).await,
                    ManualImport::File(path) => {
                        platforms.install_local_file(kind, &root, &path).await
                    }
                }
            }
            .map_err(|error| error.to_string());
            send_event(&sender, &repaint, GuiEvent::ManualInstalled(result));
        });
    }

    /// File extensions the "Add files…" dialog offers for one channel.
    fn accepted_extensions(kind: ContentKind) -> &'static [&'static str] {
        match kind {
            ContentKind::Mod => &["jar"],
            ContentKind::ResourcePack | ContentKind::Shader => &["zip"],
            ContentKind::Modpack => &["mrpack", "zip"],
        }
    }

    /// Which folder a picked or dropped file belongs in. Extensions decide;
    /// the visible channel only breaks the tie for `.zip`, which can be either
    /// a resource pack or a shader pack.
    fn classify_local_file(path: &Path, channel: ContentKind) -> Option<ContentKind> {
        let name = path.file_name()?.to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".mrpack") {
            return Some(ContentKind::Modpack);
        }
        if name.ends_with(".jar") {
            return Some(ContentKind::Mod);
        }
        if name.ends_with(".zip") {
            return Some(match channel {
                ContentKind::Shader => ContentKind::Shader,
                _ => ContentKind::ResourcePack,
            });
        }
        None
    }

    /// Add files chosen in the platform's own file manager.
    fn pick_content_files(&mut self, context: &egui::Context) {
        if self.manual_busy || self.portable_busy {
            self.set_status(StatusKind::Warning, "Another import is already running.");
            return;
        }
        let kind = self.content_kind;
        let mut dialog = rfd::FileDialog::new()
            .set_title(format!(
                "Add {} to the instance",
                kind.label().to_ascii_lowercase()
            ))
            .add_filter(kind.label(), Self::accepted_extensions(kind))
            .add_filter("All supported content", &["jar", "zip", "mrpack"]);
        if let Some(instance) = self.selected_instance() {
            let directory = kind.install_dir(&instance.root);
            let _ = std::fs::create_dir_all(&directory);
            dialog = dialog.set_directory(&directory);
        }
        if let Some(paths) = dialog.pick_files() {
            self.add_local_files(paths, context.clone());
        }
    }

    /// Copy one batch of picked or dropped files into the instance, routing
    /// each file to the folder its extension belongs in.
    fn add_local_files(&mut self, paths: Vec<PathBuf>, context: egui::Context) {
        if paths.is_empty() {
            return;
        }
        if self.manual_busy || self.portable_busy {
            self.set_status(StatusKind::Warning, "Another import is already running.");
            return;
        }
        let Some(instance) = self.selected_instance().cloned() else {
            self.set_status(
                StatusKind::Warning,
                "Select an instance before adding files.",
            );
            return;
        };
        // A modpack archive is a whole instance, so it takes the pack path.
        if let Some(pack) = paths.iter().find(|path| {
            path.extension()
                .is_some_and(|extension| extension.to_string_lossy().eq_ignore_ascii_case("mrpack"))
        }) {
            self.start_local_modpack_install(context, pack.clone());
            return;
        }
        let mut jobs = Vec::with_capacity(paths.len());
        let mut rejected = Vec::new();
        for path in paths {
            match Self::classify_local_file(&path, self.content_kind) {
                Some(kind) => jobs.push((kind, path)),
                None => rejected.push(short_name(&path)),
            }
        }
        if jobs.is_empty() {
            self.set_status(
                StatusKind::Warning,
                format!("Nothing to add: {}", rejected.join(", ")),
            );
            return;
        }
        let count = jobs.len();
        let status = format!("Adding {count} file(s) to {}…", instance.name);
        self.spawn_local_install(instance, jobs, rejected, &context, status);
    }

    /// One installer for a batch of local files. The picker and the drop path
    /// share it, so a batch reports the same summary however it arrived.
    fn spawn_local_install(
        &mut self,
        instance: Instance,
        jobs: Vec<(ContentKind, PathBuf)>,
        rejected: Vec<String>,
        context: &egui::Context,
        status: String,
    ) {
        let root = instance.root.clone();
        let platforms = self.platforms.clone();
        let sender = self.sender.clone();
        let repaint = context.clone();
        self.manual_busy = true;
        self.manual_status = status;
        self.runtime.spawn(async move {
            let mut report = InstallReport::default();
            let mut counts: Vec<(ContentKind, usize)> = Vec::new();
            for (kind, path) in jobs {
                match platforms.install_local_file(kind, &root, &path).await {
                    Ok(installed) => {
                        report.installed.extend(installed.installed);
                        report.notes.extend(installed.notes);
                        report.warnings.extend(installed.warnings);
                        match counts.iter_mut().find(|(seen, _)| *seen == kind) {
                            Some((_, count)) => *count += 1,
                            None => counts.push((kind, 1)),
                        }
                    }
                    Err(error) => {
                        report
                            .warnings
                            .push(format!("{}: {error}", short_name(&path)));
                    }
                }
            }
            for name in rejected {
                report
                    .warnings
                    .push(format!("{name}: unsupported file type"));
            }
            report.kind = counts.first().map_or(ContentKind::Mod, |(kind, _)| *kind);
            let result = if report.installed.is_empty() {
                Err(report.warnings.join(" · "))
            } else {
                let summary = added_summary(&counts);
                Ok((report, summary))
            };
            send_event(&sender, &repaint, GuiEvent::FilesAdded(result));
        });
    }

    /// Route a drop from any tab. `.mrpack` always becomes an instance, a
    /// `.zip` on the Instances tab is an instance archive, and everything else
    /// is content for the selected instance.
    fn handle_drop(&mut self, paths: Vec<PathBuf>, context: &egui::Context) {
        let mut dropped = expand_dropped(paths);
        if dropped.is_empty() {
            self.set_status(
                StatusKind::Warning,
                "Drop a mod, pack, image, or account file.",
            );
            return;
        }
        // A dropped account list is an identity import, not content. The
        // parser refuses anything that does not look like a player record, so
        // an instance JSON still falls through to the paths below.
        let mut dropped_accounts: Vec<ImportedAccount> = Vec::new();
        dropped.retain(|path| {
            if !has_extension(path, "json") {
                return true;
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                return true;
            };
            match account_import::import_text(&text) {
                Ok(accounts) => {
                    dropped_accounts.extend(accounts);
                    false
                }
                Err(_) => true,
            }
        });
        if !dropped_accounts.is_empty() {
            let (added, refreshed) = self.merge_imported_accounts(dropped_accounts);
            // Re-dropping a file that is already imported must not claim a
            // second import: the report counts what the saved list gained.
            let text = match (added, refreshed) {
                (0, 0) => "Those accounts are already saved.".to_owned(),
                (added, refreshed) => format!(
                    "{} account(s) imported and {refreshed} refreshed from the dropped file.",
                    added
                ),
            };
            self.set_status(StatusKind::Success, text);
            self.current_tab = Tab::Account;
        }
        if dropped.is_empty() {
            return;
        }
        // A picture is artwork, not content: it is normalised into the instance
        // folder and never copied into mods.
        let (images, files): (Vec<PathBuf>, Vec<PathBuf>) =
            dropped.into_iter().partition(|path| is_image_path(path));
        if !images.is_empty() {
            self.adopt_dropped_artwork(images, context);
        }
        if files.is_empty() {
            return;
        }
        // A pack archive builds a whole instance, so it outranks the content
        // files that arrived with it. Extra packs are named instead of being
        // dropped on the floor, because silently ignoring a drag is the one
        // thing a drop handler must never do.
        let packs = files
            .iter()
            .filter(|path| has_extension(path, "mrpack"))
            .count();
        if let Some(pack) = files.iter().find(|path| has_extension(path, "mrpack")) {
            self.start_local_modpack_install(context.clone(), pack.clone());
            if packs > 1 {
                self.set_status(
                    StatusKind::Info,
                    format!(
                        "Importing {} · {} other pack(s) skipped.",
                        short_name(pack),
                        packs - 1
                    ),
                );
            }
            return;
        }
        // A zip means two different things: an instance archive dropped from
        // another launcher, or a resource pack. Probe the archive instead of
        // guessing from the tab: a Prism or CurseForge pack is an instance
        // wherever it lands, while a loose game-folder zip follows the tab.
        if let Some(position) = files.iter().position(|path| has_extension(path, "zip"))
            && (self.current_tab == Tab::Instances
                || kaelis_api::foreign::probe_archive(&files[position])
                    .map(|probe| probe.kind != kaelis_api::foreign::ForeignKind::Plain)
                    .unwrap_or(false))
        {
            let mut rest = files.clone();
            let archive = rest.remove(position);
            self.import_instance_archive(archive, context);
            if !rest.is_empty() {
                self.queue_dropped_content(rest, context);
            }
            return;
        }
        self.add_local_files(files, context.clone());
    }

    /// Take the first dropped picture as instance artwork. With the editor open
    /// it stages the file for Save; otherwise it updates the selected instance
    /// right away.
    fn adopt_dropped_artwork(&mut self, images: Vec<PathBuf>, context: &egui::Context) {
        let Some(first) = images.first() else {
            return;
        };
        let bytes = match std::fs::read(first) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.set_status(
                    StatusKind::Error,
                    format!("Could not read the image: {error}"),
                );
                return;
            }
        };
        self.adopt_artwork_bytes(&bytes, context);
    }

    /// Take one picture from the system clipboard. Works for a screenshot, a
    /// browser "copy image", or a picture copied from an editor, with no file
    /// dialog and no temporary file.
    fn paste_clipboard_artwork(&mut self, context: &egui::Context) {
        let bytes = match clipboard_picture() {
            Ok(bytes) => bytes,
            // A file copied in Explorer is a file list, not a bitmap: read the
            // picture from disk instead of telling the user it failed.
            Err(message) => match clipboard_artwork_file() {
                Some(path) => match std::fs::read(&path) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        self.set_status(
                            StatusKind::Error,
                            format!("Could not read the copied image: {error}"),
                        );
                        return;
                    }
                },
                None => {
                    self.set_status(StatusKind::Warning, message);
                    return;
                }
            },
        };
        self.adopt_artwork_bytes(&bytes, context);
    }

    /// Native picture picker for instance artwork, so nobody has to type a
    /// path or know where the file lives.
    fn browse_artwork(&mut self, context: &egui::Context) {
        let sender = self.sender.clone();
        let repaint = context.clone();
        self.runtime.spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .set_title("Choose instance artwork")
                .add_filter(
                    "Images",
                    &["png", "jpg", "jpeg", "webp", "gif", "bmp", "ico", "svg"],
                )
                .pick_file()
                .await;
            if let Some(file) = picked {
                send_event(
                    &sender,
                    &repaint,
                    GuiEvent::ArtworkPicked(file.path().to_path_buf()),
                );
            }
        });
    }

    /// Native picture picker for one instance card, so setting a card icon
    /// never means opening the editor, typing a path, or knowing where the
    /// picture lives.
    fn browse_artwork_for(&mut self, id: String, context: &egui::Context) {
        let sender = self.sender.clone();
        let repaint = context.clone();
        self.runtime.spawn(async move {
            let picked = rfd::AsyncFileDialog::new()
                .set_title("Choose instance artwork")
                .add_filter(
                    "Images",
                    &["png", "jpg", "jpeg", "webp", "gif", "bmp", "ico", "svg"],
                )
                .pick_file()
                .await;
            if let Some(file) = picked {
                send_event(
                    &sender,
                    &repaint,
                    GuiEvent::InstanceArtworkPicked {
                        id,
                        path: file.path().to_path_buf(),
                    },
                );
            }
        });
    }

    /// Ctrl+V fallback for every text field we own. egui delivers a paste event
    /// for text, but a file copied in Explorer is a Windows file list, not
    /// text, so nothing would arrive and the field would look broken.
    fn handle_paste_fallback(&mut self, context: &egui::Context) {
        let (pressed, delivered) = context.input(|input| {
            (
                input.modifiers.command && input.key_pressed(egui::Key::V),
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Paste(_))),
            )
        });
        if !pressed {
            return;
        }
        let focused = context.memory(|memory| memory.focused());
        // A picture anywhere on the clipboard becomes artwork: people copy a
        // screenshot or an icon file and press Ctrl+V, not "open the editor
        // and find the file". Text pastes still belong to the focused field.
        if focused != Some(egui::Id::new(ARTWORK_FIELD_ID)) {
            let has_picture = clipboard_picture().is_ok();
            let has_picture_file = clipboard_artwork_file().is_some()
                && (self.editing.is_some() || self.current_tab == Tab::Instances);
            if has_picture || has_picture_file {
                self.paste_clipboard_artwork(context);
                return;
            }
        }
        if delivered {
            return;
        }
        if focused == Some(egui::Id::new(ARTWORK_FIELD_ID)) {
            self.paste_clipboard_artwork(context);
            return;
        }
        let Some(text) = clipboard_query_text() else {
            return;
        };
        let buffers: [(&str, &mut String); 4] = [
            (MOD_SEARCH_ID, &mut self.mod_query),
            (MOD_REFINE_ID, &mut self.mod_refine),
            (INSTALLED_FILTER_ID, &mut self.installed_filter),
            (INSTANCE_FILTER_ID, &mut self.instance_filter),
        ];
        for (id, buffer) in buffers {
            if focused == Some(egui::Id::new(id)) {
                buffer.push_str(&text);
                break;
            }
        }
    }

    /// One path for every artwork source: drop, paste, path, data URL, base64.
    fn adopt_artwork_bytes(&mut self, bytes: &[u8], context: &egui::Context) {
        if let Some(edit) = self.editing.as_ref() {
            let root = edit.root.clone();
            match store_instance_artwork(&root, bytes) {
                Ok(path) => {
                    self.edit_icon = path.display().to_string();
                    self.set_status(StatusKind::Success, "Artwork ready. Save to apply.");
                }
                Err(error) => self.set_status(StatusKind::Error, error),
            }
            return;
        }
        let Some(instance) = self.selected_instance().cloned() else {
            self.set_status(
                StatusKind::Warning,
                "Select an instance first, or open one to set its artwork.",
            );
            return;
        };
        self.apply_instance_artwork(&instance.id, bytes, context);
    }

    /// Store artwork for one instance and refresh its card immediately. Used by
    /// the avatar picker, which works without opening the editor.
    fn apply_instance_artwork(&mut self, id: &str, bytes: &[u8], context: &egui::Context) {
        let Some(instance) = self
            .instances
            .iter()
            .find(|candidate| candidate.id == id)
            .cloned()
        else {
            self.set_status(StatusKind::Warning, "That instance is no longer open.");
            return;
        };
        match store_instance_artwork(&instance.root, bytes) {
            Ok(path) => {
                let mut updated = instance.clone();
                updated.icon = Some(path);
                match updated.save() {
                    Ok(_) => {
                        if let Some(slot) = self
                            .instances
                            .iter_mut()
                            .find(|candidate| candidate.id == updated.id)
                        {
                            *slot = updated.clone();
                        }
                        self.refresh_instance_cards();
                        self.instance_icons.remove(&updated.id);
                        if let Some(path) = updated.icon.clone() {
                            self.request_instance_icon(context, updated.id.clone(), path);
                        }
                        self.set_status(
                            StatusKind::Success,
                            format!("Artwork set for {}.", updated.name),
                        );
                    }
                    Err(error) => self.set_status(
                        StatusKind::Error,
                        format!("Could not save the artwork: {error}"),
                    ),
                }
            }
            Err(error) => self.set_status(StatusKind::Error, error),
        }
    }

    /// Turn whatever the artwork field holds (a path, a data URL, or base64)
    /// into one stored 256 px PNG before the instance record is written.
    fn prepare_editor_artwork(&mut self) -> bool {
        let Some(edit) = self.editing.as_ref() else {
            return true;
        };
        let root = edit.root.clone();
        let text = self.edit_icon.trim().to_owned();
        if text.is_empty() {
            return true;
        }
        let managed = root.join(".kaelis").join("artwork.png");
        if Path::new(&text) == managed {
            return true;
        }
        let bytes = if let Some(payload) = decode_image_payload(&text) {
            payload
        } else {
            let path = PathBuf::from(&text);
            if !path.is_file() || !is_image_path(&path) {
                // The normal save validation reports a missing or unsupported
                // file with its own message.
                return true;
            }
            match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    self.set_status(
                        StatusKind::Error,
                        format!("Could not read the image: {error}"),
                    );
                    return false;
                }
            }
        };
        match store_instance_artwork(&root, &bytes) {
            Ok(path) => {
                self.edit_icon = path.display().to_string();
                true
            }
            Err(error) => {
                self.set_status(StatusKind::Error, error);
                false
            }
        }
    }

    /// Content dropped beside an instance archive still belongs to the open
    /// instance; hand it to the normal installer instead of ignoring it.
    fn queue_dropped_content(&mut self, files: Vec<PathBuf>, context: &egui::Context) {
        let Some(instance) = self.selected_instance().cloned() else {
            return;
        };
        if self.manual_busy || self.portable_busy {
            return;
        }
        let mut jobs = Vec::with_capacity(files.len());
        for path in files {
            if let Some(kind) = Self::classify_local_file(&path, self.content_kind) {
                jobs.push((kind, path));
            }
        }
        if jobs.is_empty() {
            return;
        }
        let count = jobs.len();
        let status = format!("Adding {count} file(s) to {}…", instance.name);
        self.spawn_local_install(instance, jobs, Vec::new(), context, status);
    }

    /// A drop target that states its destination before the mouse button is
    /// released, so a drag is never a guess about which tab or instance it
    /// will land in.
    fn show_drop_overlay(&self, context: &egui::Context) {
        let mut hovering = context.input(|input| input.raw.hovered_files.len());
        // Development-only fixture: a real OS drag cannot be scripted, so a
        // capture run can ask for the overlay on a fixed frame.
        if hovering == 0
            && self.screenshot_path.is_some()
            && std::env::var_os("KAELIS_SCREENSHOT_DROP").is_some()
        {
            hovering = 2;
        }
        if hovering == 0 {
            return;
        }
        let (headline, detail) = match self.current_tab {
            Tab::Instances => (
                "Release to import".to_owned(),
                "Instance .zip or modpack .mrpack".to_owned(),
            ),
            _ => (
                "Release to add".to_owned(),
                match self.selected_instance() {
                    Some(instance) => {
                        format!("Mods, packs, and shaders → {}", instance.name)
                    }
                    None => "Mods, packs, and shaders · pick an instance first".to_owned(),
                },
            ),
        };
        let count = match hovering {
            1 => "1 item".to_owned(),
            other => format!("{other} items"),
        };
        egui::Area::new(egui::Id::new("kaelis-drop-target"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .interactable(false)
            .show(context, |ui| {
                egui::Frame::none()
                    .fill(egui::Color32::from_rgba_premultiplied(10, 12, 17, 240))
                    .stroke(egui::Stroke::new(1.0_f32, accent().gamma_multiply(0.55)))
                    .rounding(egui::Rounding::same(18.0))
                    .inner_margin(egui::Margin::symmetric(30.0, 22.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 14.0;
                            // A painted dot, not a glyph: every bundled font
                            // renders it, including on a bare Windows install.
                            let (mark, _) = ui
                                .allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                            ui.painter()
                                .circle_filled(mark.center(), 5.0, accent_soft());
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 3.0;
                                ui.label(
                                    egui::RichText::new(&headline)
                                        .size(18.0)
                                        .strong()
                                        .color(TEXT),
                                );
                                ui.label(egui::RichText::new(&detail).size(12.0).color(MUTED));
                            });
                        });
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new(count)
                                .size(10.5)
                                .color(accent_soft().gamma_multiply(0.9)),
                        );
                    });
            });
    }

    /// Import an instance archive chosen in the file manager.
    fn pick_instance_archive(&mut self, context: &egui::Context) {
        if self.portable_busy {
            self.set_status(
                StatusKind::Warning,
                "An instance archive is already being written.",
            );
            return;
        }
        let Some(archive) = rfd::FileDialog::new()
            .set_title("Import an instance")
            .add_filter("Instance or modpack", &["zip", "mrpack"])
            .add_filter("Kaelis instance", &["kaelis.zip"])
            .add_filter("All files", &["*"])
            .pick_file()
        else {
            return;
        };
        self.import_instance_archive(archive, context);
    }

    /// Import anything that looks like an instance: Kaelis archives, Modrinth
    /// packs, Prism/MultiMC exports, CurseForge packs, and loose game folders.
    /// The archive's own metadata decides which importer runs.
    fn import_instance_archive(&mut self, archive: PathBuf, context: &egui::Context) {
        if self.portable_busy || self.modpack_installing.is_some() {
            self.set_status(
                StatusKind::Warning,
                "An instance archive is already being imported.",
            );
            return;
        }
        let probe = match kaelis_api::foreign::probe_archive(&archive) {
            Ok(probe) => probe,
            Err(error) => {
                self.set_status(
                    StatusKind::Error,
                    format!("Could not read {}: {error}", short_name(&archive)),
                );
                return;
            }
        };
        // Modrinth packs need their download URLs resolved, so they travel the
        // same transactional installer as a pack picked from the browser.
        if probe.kind == kaelis_api::foreign::ForeignKind::Modrinth {
            let title = if probe.name.trim().is_empty() {
                archive
                    .file_stem()
                    .map(|stem| stem.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Modpack".to_owned())
            } else {
                probe.name.clone()
            };
            let id = self.unique_instance_id(&slugify(&title));
            let version = self.browse_game_version().unwrap_or_default();
            self.begin_modpack_install(
                context.clone(),
                title,
                id,
                version,
                ModpackSource::Local(archive),
            );
            return;
        }
        let sender = self.sender.clone();
        let repaint = context.clone();
        self.portable_busy = true;
        self.set_status(
            StatusKind::Busy,
            format!(
                "Importing {} as a {}…",
                short_name(&archive),
                probe.kind.label()
            ),
        );
        let fallback_version = if probe.game_version.trim().is_empty() {
            let typed = self.new_instance_version.trim();
            if typed.is_empty() {
                self.browse_game_version().unwrap_or_default()
            } else {
                typed.to_owned()
            }
        } else {
            String::new()
        };
        let kind = probe.kind;
        self.runtime.spawn(async move {
            let target = Instance::instances_dir();
            let result = tokio::task::spawn_blocking(move || {
                if kind == kaelis_api::foreign::ForeignKind::Kaelis {
                    return portable::import_instance(&archive, &target)
                        .map(|report| report.summary())
                        .map_err(|error| error.to_string());
                }
                kaelis_api::foreign::import_foreign_archive(
                    &archive,
                    &probe,
                    &target,
                    &fallback_version,
                )
                .map(|report| report.summary())
                .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result);
            send_event(
                &sender,
                &repaint,
                GuiEvent::Portable(result.map(|message| (message, true))),
            );
        });
    }

    /// Write one instance and its files into a portable archive.
    fn export_instance_archive(&mut self, instance: Instance, context: &egui::Context) {
        if self.portable_busy {
            self.set_status(
                StatusKind::Warning,
                "An instance archive is already being written.",
            );
            return;
        }
        let mut dialog = rfd::FileDialog::new()
            .set_title("Export an instance archive")
            .set_file_name(format!("{}.kaelis.zip", instance.id))
            .add_filter("Kaelis instance", &["zip"]);
        if let Some(dir) = &self.last_export_dir {
            dialog = dialog.set_directory(dir);
        }
        let Some(destination) = dialog.save_file() else {
            return;
        };
        if let Some(dir) = destination.parent() {
            self.last_export_dir = Some(dir.to_path_buf());
        }
        let sender = self.sender.clone();
        let repaint = context.clone();
        self.portable_busy = true;
        self.set_status(StatusKind::Busy, format!("Exporting {}…", instance.name));
        self.runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                portable::export_instance(&instance, &destination)
                    .map(|report| format!("{} → {}", report.summary(), destination.display()))
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result.map_err(|error| error.to_string()));
            send_event(
                &sender,
                &repaint,
                GuiEvent::Portable(result.map(|message| (message, false))),
            );
        });
    }

    /// Re-read every instance record from disk, keeping the current selection
    /// when it still exists. Used after an archive import adds one.
    fn reload_instance_list(&mut self, context: &egui::Context) {
        let previous = self.selected_instance.clone();
        self.instances = Instance::load_all().unwrap_or_default();
        self.refresh_instance_cards();
        self.reload_instance_icons(context);
        self.selected_instance = previous
            .filter(|id| self.instances.iter().any(|instance| &instance.id == id))
            .or_else(|| self.instances.first().map(|instance| instance.id.clone()));
        self.refresh_installed();
    }

    /// Record the result of a manual or file-manager import in one place.
    fn finish_manual_install(
        &mut self,
        result: Result<InstallReport, String>,
        summary: Option<String>,
    ) {
        self.manual_busy = false;
        match result {
            Ok(report) => {
                let text = summary.unwrap_or_else(|| report.summary());
                let clean = report.warnings.is_empty();
                self.manual_status = text.clone();
                self.last_install = Some(report);
                self.refresh_installed();
                self.set_status(
                    if clean {
                        StatusKind::Success
                    } else {
                        StatusKind::Warning
                    },
                    text,
                );
            }
            Err(error) => {
                self.manual_status = format!("Import failed: {error}");
                self.set_status(StatusKind::Error, self.manual_status.clone());
            }
        }
    }

    /// Draw a cached premium head immediately (or Steve when the name has no
    /// premium skin yet), then look the name up in the background.
    fn apply_offline_head(&mut self, context: &egui::Context, name: &str) {
        let image = self
            .offline_skins
            .get(&name.to_ascii_lowercase())
            .cloned()
            .flatten()
            .unwrap_or_else(steve_head);
        self.avatar =
            Some(context.load_texture("kaelis-profile-head", image, egui::TextureOptions::NEAREST));
    }

    fn request_offline_skin(&mut self, context: &egui::Context, name: &str) {
        let key = name.trim().to_ascii_lowercase();
        if !valid_username(&key) {
            return;
        }
        let active = self
            .offline_account
            .as_ref()
            .is_some_and(|profile| profile.name.eq_ignore_ascii_case(&key))
            && self.microsoft_session.is_none();
        if self.offline_skins.contains_key(&key) {
            if active {
                self.apply_offline_head(context, &key);
            }
            self.refresh_offline_typing_avatar(context);
            return;
        }
        if self.offline_skin_pending.as_deref() == Some(key.as_str()) {
            return;
        }
        // Single flight per username: the active avatar, the typing preview,
        // and every account row share one lookup and one decode.
        if !self.offline_skin_inflight.insert(key.clone()) {
            return;
        }
        self.offline_skin_pending = Some(key.clone());
        let sender = self.sender.clone();
        let repaint = context.clone();
        let client = self.http.clone();
        self.runtime.spawn(async move {
            let (result, failed) = match fetch_head_for_username(&client, &key, 64).await {
                Ok(Some(bytes)) => {
                    let decoded = tokio::task::spawn_blocking(move || {
                        image::load_from_memory(&bytes)
                            .map(|image| image.to_rgba8())
                            .map(|image| {
                                egui::ColorImage::from_rgba_unmultiplied(
                                    [image.width() as usize, image.height() as usize],
                                    image.as_raw(),
                                )
                            })
                            .ok()
                    })
                    .await
                    .ok()
                    .flatten();
                    let failed = decoded.is_none();
                    (decoded, failed)
                }
                Ok(None) => (None, false),
                // A network hiccup keeps the current head and is retried the
                // next time the name settles.
                Err(_) => (None, true),
            };
            send_event(
                &sender,
                &repaint,
                GuiEvent::OfflineAvatar {
                    name: key,
                    result,
                    failed,
                },
            );
        });
    }

    fn refresh_offline_typing_avatar(&mut self, context: &egui::Context) {
        let key = self.offline_name.trim().to_ascii_lowercase();
        if key == self.offline_typing_key && self.offline_typing_avatar.is_some() {
            return;
        }
        let image = self
            .offline_skins
            .get(&key)
            .cloned()
            .flatten()
            .unwrap_or_else(steve_head);
        self.offline_typing_key = key;
        self.offline_typing_avatar = Some(context.load_texture(
            "kaelis-offline-typing-head",
            image,
            egui::TextureOptions::NEAREST,
        ));
    }

    fn request_avatar(&self, context: egui::Context, uuid: String) {
        let sender = self.sender.clone();
        let repaint = context;
        let client = self.http.clone();
        self.runtime.spawn(async move {
            let result = match fetch_crafatar_head(&client, &uuid, 64).await {
                Ok(bytes) => match tokio::task::spawn_blocking(move || {
                    let image = image::load_from_memory(&bytes)
                        .map_err(|error| error.to_string())?
                        .to_rgba8();
                    let size = [image.width() as usize, image.height() as usize];
                    Ok::<_, String>(egui::ColorImage::from_rgba_unmultiplied(
                        size,
                        image.as_raw(),
                    ))
                })
                .await
                {
                    Ok(result) => result,
                    Err(error) => Err(error.to_string()),
                },
                Err(error) => Err(error.to_string()),
            };
            send_event(&sender, &repaint, GuiEvent::Avatar(result));
        });
    }

    /// Game version the browser targets. A selected instance wins; without
    /// one (modpacks can be browsed before any instance exists) the version
    /// picker on the Instances tab supplies it.
    fn browse_game_version(&self) -> Option<String> {
        if let Some(instance) = self.selected_instance() {
            return Some(instance.mc_version.clone());
        }
        let version = self.new_instance_version.trim();
        (!version.is_empty()).then(|| version.to_owned())
    }

    /// Loader that actually applies to the selected instance. Installed jars
    /// win over the instance label, and the label wins over the browser filter,
    /// so a stale combo can never install incompatible metadata.
    fn resolved_mod_loader(&self) -> String {
        let instance_loader = self.selected_instance().and_then(|instance| {
            (!instance.loader.eq_ignore_ascii_case("vanilla")).then(|| instance.loader.clone())
        });
        match instance_loader {
            // Selecting an instance synchronises the combo with the jars (or
            // the declared loader), so a differing combo is a deliberate
            // override and must win over the instance label.
            Some(instance_loader) if !self.mod_loader.eq_ignore_ascii_case(&instance_loader) => {
                self.mod_loader.clone()
            }
            Some(instance_loader) => self.detected_loader.clone().unwrap_or(instance_loader),
            None => self
                .detected_loader
                .clone()
                .unwrap_or_else(|| self.mod_loader.clone()),
        }
    }

    /// Everything that changes the result set. Used by the debounce so typing,
    /// a loader change, a version change, and provider toggles all refresh the
    /// browser without a stale response ever winning.
    fn mod_signature(&self) -> String {
        let instance = self.selected_instance();
        format!(
            "{}|{}|{}|{}|{}|{}|{:?}",
            instance
                .map(|instance| instance.id.as_str())
                .unwrap_or_default(),
            self.browse_game_version().unwrap_or_default(),
            self.resolved_mod_loader(),
            self.mod_query.trim(),
            self.mod_show_modrinth,
            self.mod_show_curseforge,
            self.content_kind
        )
    }

    fn start_mod_search(&mut self, context: egui::Context) {
        self.start_mod_search_page(context, false);
    }

    /// Search the next page from both providers and append it. The catalogue is
    /// paged instead of truncated, so a broad query can be browsed to the end.
    fn start_mod_search_page(&mut self, context: egui::Context, append: bool) {
        if append && !self.mod_has_more {
            return;
        }
        let offset = if append {
            self.mod_pages.saturating_mul(MOD_SEARCH_LIMIT)
        } else {
            0
        };
        self.mod_pages = if append {
            self.mod_pages.saturating_add(1)
        } else {
            1
        };
        let Some(game_version) = self.browse_game_version() else {
            self.set_status(
                StatusKind::Warning,
                "Pick a Minecraft version before searching.",
            );
            return;
        };
        if !self.mod_show_modrinth && !self.mod_show_curseforge {
            self.set_status(
                StatusKind::Warning,
                "Enable Modrinth, CurseForge, or both before searching.",
            );
            return;
        }
        let signature = self.mod_signature();
        self.mod_last_request = signature.clone();
        self.mod_pending_signature = signature;
        self.mod_signature_changed_at = None;
        self.mod_search_generation = self.mod_search_generation.wrapping_add(1);
        let generation = self.mod_search_generation;
        self.search_busy = true;
        self.mod_icon_generation = self.mod_icon_epoch.fetch_add(1, Ordering::Relaxed) + 1;
        let query = self.mod_query.trim().to_owned();
        let loader = self.resolved_mod_loader();
        let kind = self.content_kind;
        let platforms = self.platforms.clone();
        let include_modrinth = self.mod_show_modrinth;
        let include_curseforge = self.mod_show_curseforge;
        let sender = self.sender.clone();
        let repaint = context;
        if kind == ContentKind::Modpack && !include_modrinth {
            self.set_status(
                StatusKind::Warning,
                "Modpacks are installed from Modrinth; enable it to browse.",
            );
        }
        self.set_status(
            StatusKind::Busy,
            if append {
                format!(
                    "Loading more {} from Modrinth and CurseForge…",
                    kind.label().to_ascii_lowercase()
                )
            } else if query.is_empty() {
                if kind.uses_loader() {
                    format!(
                        "Loading popular {loader} {} for Minecraft {game_version}…",
                        kind.label().to_ascii_lowercase()
                    )
                } else {
                    format!(
                        "Loading popular {} for Minecraft {game_version}…",
                        kind.label().to_ascii_lowercase()
                    )
                }
            } else {
                format!(
                    "Searching {} for \"{query}\" · Minecraft {game_version}…",
                    kind.label().to_ascii_lowercase()
                )
            },
        );
        self.runtime.spawn(async move {
            let results = platforms
                .search_scoped_kind(
                    &query,
                    &game_version,
                    &loader,
                    kaelis_api::mods::SearchOptions::channel(kind, MOD_SEARCH_LIMIT)
                        .page(offset)
                        .providers(include_modrinth, include_curseforge),
                )
                .await;
            send_event(
                &sender,
                &repaint,
                GuiEvent::ModSearch {
                    generation,
                    query,
                    results,
                },
            );
        });
    }

    /// Queue an install instead of rejecting it while another one runs. Rows
    /// stay bound to their instance, and the queue drains in order.
    fn queue_mod_install(&mut self, context: egui::Context, item: ModSearchItem) {
        if item.kind == ContentKind::Modpack {
            self.start_modpack_install(context, item);
            return;
        }
        let Some(instance) = self.selected_instance().cloned() else {
            self.set_status(
                StatusKind::Warning,
                "Select an instance before installing a mod.",
            );
            return;
        };
        let key = mod_item_key(&item);
        let state_key = install_state_key(&instance.id, &key);
        let already_queued = matches!(
            self.install_states.get(&state_key),
            Some(ModInstallState::Queued | ModInstallState::Running)
        ) || self
            .installing_job
            .as_ref()
            .is_some_and(|job| job.instance_id == instance.id && job.key == key)
            || self
                .install_queue
                .iter()
                .any(|job| job.instance_id == instance.id && job.key == key);
        if already_queued {
            self.set_status(
                StatusKind::Info,
                format!("{} is already queued.", item.title),
            );
            return;
        }
        self.install_states
            .insert(state_key, ModInstallState::Queued);
        if self.install_states.len() > 256 {
            // Finished rows from long-past sessions are not worth unbounded
            // memory; active jobs must survive the trim.
            self.install_states.retain(|_, state| {
                matches!(state, ModInstallState::Queued | ModInstallState::Running)
            });
        }
        self.install_queue.push_back(InstallJob {
            key,
            instance_id: instance.id.clone(),
            item: item.clone(),
        });
        if self.installing_job.is_some() {
            let position = self.install_queue.len();
            self.set_status(
                StatusKind::Info,
                format!("Queued {} (#{position} in line).", item.title),
            );
        }
        self.pump_install_queue(context);
    }

    /// Install a modpack as its own instance. Packs carry a game version, a
    /// loader, and their whole mod list, so importing one into an existing
    /// instance would mix two environments; the pack always gets a fresh
    /// instance that is named after it.
    fn start_modpack_install(&mut self, context: egui::Context, item: ModSearchItem) {
        let Some(game_version) = self.browse_game_version() else {
            self.set_status(
                StatusKind::Warning,
                "Pick a Minecraft version before installing a modpack.",
            );
            return;
        };
        let base_id = slugify(&item.title);
        if base_id.is_empty() {
            self.set_status(StatusKind::Warning, "That modpack name cannot be used.");
            return;
        }
        let id = self.unique_instance_id(&base_id);
        self.begin_modpack_install(
            context,
            item.title.clone(),
            id,
            game_version,
            ModpackSource::Remote(Box::new(item)),
        );
    }

    /// Import a `.mrpack` from disk. Dropping one onto the Content tab is a
    /// complete install: the pack's own metadata decides the version, loader,
    /// and mod list.
    fn start_local_modpack_install(&mut self, context: egui::Context, path: PathBuf) {
        if path
            .extension()
            .is_none_or(|extension| !extension.to_string_lossy().eq_ignore_ascii_case("mrpack"))
        {
            self.set_status(
                StatusKind::Warning,
                "Drop a .mrpack file, or pick one from the Import panel.",
            );
            return;
        }
        let title = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().replace(['-', '_'], " "))
            .unwrap_or_else(|| "Modpack".to_owned());
        let base_id = slugify(&title);
        if base_id.is_empty() {
            self.set_status(StatusKind::Warning, "That pack filename cannot be used.");
            return;
        }
        let id = self.unique_instance_id(&base_id);
        let game_version = self.browse_game_version().unwrap_or_default();
        self.begin_modpack_install(context, title, id, game_version, ModpackSource::Local(path));
    }

    fn unique_instance_id(&self, base: &str) -> String {
        let mut candidate = base.to_owned();
        let mut suffix = 2_u32;
        while self
            .instances
            .iter()
            .any(|instance| instance.id == candidate)
            || Instance::instances_dir()
                .join(format!("{candidate}.json"))
                .exists()
        {
            candidate = format!("{base}-{suffix}");
            suffix += 1;
        }
        candidate
    }

    fn begin_modpack_install(
        &mut self,
        context: egui::Context,
        title: String,
        id: String,
        game_version: String,
        source: ModpackSource,
    ) {
        if self.modpack_installing.is_some() {
            self.set_status(StatusKind::Warning, "A modpack is already installing.");
            return;
        }
        self.modpack_generation = self.modpack_generation.wrapping_add(1);
        let generation = self.modpack_generation;
        self.modpack_installing = Some(title.clone());
        self.modpack_detail = "Reading the pack…".to_owned();
        self.set_status(StatusKind::Busy, format!("Installing modpack {title}…"));
        let platforms = self.platforms.clone();
        let http = self.http.clone();
        let sender = self.sender.clone();
        let repaint = context;
        let staging = kaelis_core::sweeper::temp_root().join("modpacks");
        let mut icon_url = match &source {
            ModpackSource::Remote(item) => item.icon_url.clone(),
            _ => None,
        };
        let task = self.runtime.spawn(async move {
            let downloaded = !matches!(source, ModpackSource::Local(_));
            let result = async {
                let pack_path = match source {
                    ModpackSource::Remote(item) => platforms
                        .download_modpack(&item, &game_version, &staging)
                        .await
                        .map_err(|error| error.to_string())?,
                    ModpackSource::Link(link) => {
                        let item = platforms
                            .resolve_modpack(&link)
                            .await
                            .map_err(|error| error.to_string())?;
                        icon_url = item.icon_url.clone();
                        platforms
                            .download_modpack(&item, &game_version, &staging)
                            .await
                            .map_err(|error| error.to_string())?
                    }
                    ModpackSource::Url(url) => {
                        std::fs::create_dir_all(&staging).map_err(|error| error.to_string())?;
                        let name = url
                            .rsplit('/')
                            .next()
                            .filter(|name| !name.is_empty())
                            .unwrap_or("pack.mrpack");
                        let destination = staging.join(format!("{}-{name}", std::process::id()));
                        // A pack archive is verified when it is imported; the
                        // link itself has no published digest to check.
                        let task = kaelis_api::downloader::DownloadTask::new(&url, destination, "");
                        kaelis_api::downloader::download_all(&http, vec![task], 1)
                            .await
                            .map_err(|error| error.to_string())?
                            .pop()
                            .ok_or_else(|| "the pack download produced no file".to_owned())?
                    }
                    ModpackSource::Local(path) => path,
                };
                let mut staged_cleanup =
                    downloaded.then(|| PackArchiveCleanup(Some(pack_path.clone())));
                let mut pack = kaelis_api::mrpack::read_pack_summary(&pack_path)
                    .map_err(|error| error.to_string())?;
                let root = Instance::instances_dir().join(&id);
                let mut instance = Instance::new(
                    &id,
                    if pack.name.trim().is_empty() {
                        &title
                    } else {
                        &pack.name
                    },
                    &pack.game_version,
                    &pack.loader,
                    root,
                );
                std::fs::create_dir_all(&instance.root).map_err(|error| error.to_string())?;
                let mut cleanup = ModpackCleanup(Some(instance.clone()));
                let mut pending_icon: Option<tokio::task::JoinHandle<Option<Vec<u8>>>> = None;
                // A picture inside the pack costs nothing, so it lands before
                // the record is saved. A remote project picture is fetched
                // alongside the mod downloads instead of delaying them; the
                // artwork is cosmetic, so a failed second save never turns a
                // finished import into an error.
                match pack.icon.take() {
                    Some(bytes) => {
                        apply_pack_artwork(&mut instance, &bytes);
                    }
                    None => {
                        if let Some(url) = icon_url.clone() {
                            let http = http.clone();
                            pending_icon =
                                Some(tokio::spawn(
                                    async move { fetch_pack_icon(&http, &url).await },
                                ));
                        }
                    }
                }
                instance.save().map_err(|error| error.to_string())?;
                let progress_sender = sender.clone();
                let progress_repaint = repaint.clone();
                let imported = kaelis_api::mrpack::install_mrpack_with_progress(
                    &http,
                    &pack_path,
                    &instance.root,
                    move |detail| {
                        send_event(
                            &progress_sender,
                            &progress_repaint,
                            GuiEvent::ModpackProgress {
                                generation,
                                detail: detail.to_owned(),
                            },
                        );
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
                if let Some(handle) = pending_icon
                    && let Ok(Some(bytes)) = handle.await
                    && apply_pack_artwork(&mut instance, &bytes)
                {
                    let _ = instance.save();
                }
                if let Some(cleanup) = &mut staged_cleanup {
                    // Staged downloads are ours to remove; a local .mrpack the
                    // user dropped belongs to them.
                    cleanup.0 = None;
                    let _ = std::fs::remove_file(&pack_path);
                }
                // The instance is complete; dropping the guard now would
                // delete it.
                cleanup.0 = None;
                Ok(ModpackOutcome {
                    instance,
                    imported,
                    pack,
                })
            }
            .await;
            send_event(
                &sender,
                &repaint,
                GuiEvent::ModpackInstalled {
                    generation,
                    result: Box::new(result),
                },
            );
        });
        self.modpack_task = Some(task);
    }

    /// Delete logs, crash reports, and staging leftovers from every instance.
    /// Saves, mods, configs, and options are outside the cleaner by design.
    /// Sweep one instance once its game has exited, so the session's logs,
    /// caches, and crash dumps never outlive the process that wrote them.
    fn clean_finished_instance(&self, instance_id: &str, context: &egui::Context) {
        let Some(root) = self
            .instances
            .iter()
            .find(|instance| instance.id == instance_id)
            .map(|instance| instance.root.clone())
        else {
            return;
        };
        // The shared staging root is only safe to empty once no other game is
        // extracting natives into it.
        let sweep_temp = self.running_instances.is_empty();
        let sender = self.sender.clone();
        let repaint = context.clone();
        self.runtime.spawn_blocking(move || {
            let report = kaelis_core::junk::clean_instance(&root).unwrap_or_default();
            if sweep_temp {
                let _ = kaelis_core::sweeper::sweep_stale(0);
            }
            // Say what the cleanup actually reclaimed: a silent delete is
            // indistinguishable from one that never ran.
            if !report.is_empty() {
                send_event(
                    &sender,
                    &repaint,
                    GuiEvent::Notice(format!("Session cleanup · {}", report.summary())),
                );
            }
        });
    }

    fn clean_junk_now(&mut self, context: egui::Context) {
        let sender = self.sender.clone();
        let repaint = context;
        let instances = Instance::instances_dir();
        self.runtime.spawn_blocking(move || {
            let message = match kaelis_core::junk::clean_instances(&instances) {
                Ok(report) => report.summary(),
                Err(error) => format!("Cleanup skipped: {error}"),
            };
            // Older instances keep private copies of libraries, the client jar,
            // and the Java runtime until one pass links them into the shared
            // store. A per-instance marker makes every later start a no-op.
            let mut shared = kaelis_api::content::ShareReport::default();
            for instance in Instance::load_all().unwrap_or_default() {
                shared.add(kaelis_api::content::seed_instance(&instance.root));
            }
            let pruned = kaelis_api::content::prune_unreferenced();
            let message = if pruned.is_empty() {
                message
            } else {
                format!(
                    "{message} Pruned {:.1} MiB of unreferenced cache.",
                    pruned.bytes as f64 / (1024.0 * 1024.0)
                )
            };
            let message = if shared.is_empty() {
                message
            } else {
                format!("{message} {}", shared.summary())
            };
            send_event(&sender, &repaint, GuiEvent::Notice(message));
        });
    }

    fn pump_install_queue(&mut self, context: egui::Context) {
        if self.installing_job.is_some() {
            return;
        }
        loop {
            let Some(job) = self.install_queue.pop_front() else {
                return;
            };
            let state_key = install_state_key(&job.instance_id, &job.key);
            let Some(instance) = self
                .instances
                .iter()
                .find(|instance| instance.id == job.instance_id)
                .cloned()
            else {
                self.install_states.insert(
                    state_key,
                    ModInstallState::Failed("The target instance no longer exists.".to_owned()),
                );
                continue;
            };
            let include_dependencies = self.install_dependencies;
            let loader = if self
                .selected_instance()
                .map(|selected| selected.id.as_str())
                == Some(job.instance_id.as_str())
                && self.detected_loader.is_some()
            {
                self.resolved_mod_loader()
            } else if !instance.loader.eq_ignore_ascii_case("vanilla") {
                instance.loader.clone()
            } else {
                self.mod_loader.clone()
            };
            self.install_states
                .insert(state_key, ModInstallState::Running);
            self.installing_job = Some(job.clone());
            self.install_detail = Some("Resolving requirements…".to_owned());
            self.install_started_at = Some(std::time::Instant::now());
            self.set_status(
                StatusKind::Busy,
                format!("Installing {} · {}", job.item.title, instance.name),
            );
            let platforms = self.platforms.clone();
            let sender = self.sender.clone();
            let repaint = context.clone();
            let progress_sender = sender.clone();
            let progress_context = context.clone();
            let game_version = instance.mc_version.clone();
            let instance_root = instance.root.clone();
            let task = self.runtime.spawn(async move {
                let result = platforms
                    .install_content_reported(
                        &job.item,
                        &game_version,
                        &loader,
                        &instance_root,
                        include_dependencies,
                        move |event| {
                            let detail = match event {
                                InstallEvent::Resolving { label } => {
                                    format!("Resolving {label}…")
                                }
                                InstallEvent::FileInstalled {
                                    installed,
                                    dependencies,
                                } => {
                                    if dependencies > 0 {
                                        format!(
                                            "Downloaded {installed} file(s) · {dependencies} dependencies"
                                        )
                                    } else {
                                        format!("Downloaded {installed} file(s)…")
                                    }
                                }
                                InstallEvent::Finished {
                                    installed,
                                    dependencies,
                                } => format!(
                                    "Verified {installed} file(s) · {dependencies} dependencies"
                                ),
                            };
                            send_event(
                                &progress_sender,
                                &progress_context,
                                GuiEvent::ModInstallProgress(detail),
                            );
                        },
                    )
                    .await
                    .map_err(|error| error.to_string());
                send_event(&sender, &repaint, GuiEvent::ModInstalled(result));
            });
            self.install_task = Some(task);
            return;
        }
    }

    /// Stop the running content install now. The banner disappears on this
    /// frame; the worker is aborted at its next await, and a short watcher
    /// starts the next queued job only after it has actually stopped.
    fn cancel_mod_install(&mut self, context: egui::Context) {
        let Some(job) = self.installing_job.take() else {
            return;
        };
        self.install_states.insert(
            install_state_key(&job.instance_id, &job.key),
            ModInstallState::Cancelled,
        );
        self.install_detail = None;
        self.install_started_at = None;
        self.set_status(StatusKind::Info, format!("Stopped {}.", job.item.title));
        let Some(task) = self.install_task.take() else {
            self.pump_install_queue(context);
            return;
        };
        task.abort();
        let sender = self.sender.clone();
        let repaint = context;
        self.runtime.spawn(async move {
            let _ = task.await;
            send_event(&sender, &repaint, GuiEvent::InstallStopped);
        });
    }

    /// Stop a modpack import. Aborting drops the worker's cleanup guard, which
    /// removes the half-created instance; the generation bump ignores any
    /// progress that was already in the channel.
    fn cancel_modpack_install(&mut self, context: egui::Context) {
        let Some(title) = self.modpack_installing.take() else {
            return;
        };
        self.modpack_generation = self.modpack_generation.wrapping_add(1);
        self.modpack_detail.clear();
        self.set_status(StatusKind::Info, format!("Stopped importing {title}."));
        let Some(task) = self.modpack_task.take() else {
            return;
        };
        task.abort();
        let sender = self.sender.clone();
        let repaint = context;
        self.runtime.spawn(async move {
            // Abort is immediate for the UI, but the future's cleanup guards
            // only run when the runtime drops it. Wait for that before the
            // instance list is re-read, or a cancelled pack briefly reappears.
            let _ = task.await;
            send_event(&sender, &repaint, GuiEvent::ModpackStopped);
        });
    }

    fn clear_install_queue(&mut self) {
        for job in self.install_queue.drain(..) {
            self.install_states
                .remove(&install_state_key(&job.instance_id, &job.key));
        }
        self.set_status(StatusKind::Info, "Install queue cleared.");
    }

    fn request_mod_icon(
        &mut self,
        context: &egui::Context,
        generation: u64,
        key: String,
        url: String,
    ) {
        if self.mod_icons.contains_key(&key) {
            return;
        }
        let Some(url) = https_icon_url(&url) else {
            self.mod_icons.insert(key, ModIconState::Failed);
            return;
        };
        self.mod_icons.insert(key.clone(), ModIconState::Loading);
        let client = self.http.clone();
        let sender = self.sender.clone();
        let repaint = context.clone();
        let slots = Arc::clone(&self.mod_icon_slots);
        let epoch = Arc::clone(&self.mod_icon_epoch);
        self.runtime.spawn(async move {
            // A newer search invalidates queued work before it touches the
            // network, keeping icon traffic bounded to the visible result set.
            if epoch.load(Ordering::Relaxed) != generation {
                return;
            }
            let Ok(_permit) = slots.acquire_owned().await else {
                return;
            };
            if epoch.load(Ordering::Relaxed) != generation {
                return;
            }
            let result = fetch_mod_icon(&client, url).await;
            if epoch.load(Ordering::Relaxed) != generation {
                return;
            }
            send_event(
                &sender,
                &repaint,
                GuiEvent::ModIcon {
                    generation,
                    key,
                    result,
                },
            );
        });
    }

    /// Read and decode one instance artwork file off the UI thread. Failures
    /// fall back to the variant monogram and are retried by `Recheck`.
    fn request_instance_icon(&mut self, context: &egui::Context, id: String, path: PathBuf) {
        if self.instance_icons.contains_key(&id) {
            return;
        }
        if !path.is_file() {
            self.instance_icon_paths.insert(id.clone(), path);
            self.instance_icons.insert(id, ModIconState::Failed);
            return;
        }
        self.instance_icon_paths.insert(id.clone(), path.clone());
        self.instance_icons
            .insert(id.clone(), ModIconState::Loading);
        let sender = self.sender.clone();
        let repaint = context.clone();
        self.runtime.spawn_blocking(move || {
            let result = (|| {
                let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
                if metadata.len() > INSTANCE_ICON_BYTES {
                    return Err("instance icon exceeds the 8 MiB limit".to_owned());
                }
                let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
                decode_instance_icon(&bytes)
            })();
            send_event(&sender, &repaint, GuiEvent::InstanceIcon { id, result });
        });
    }

    fn reload_instance_icons(&mut self, context: &egui::Context) {
        let live: HashSet<String> = self
            .instances
            .iter()
            .map(|instance| instance.id.clone())
            .collect();
        self.instance_icons.retain(|id, _| live.contains(id));
        self.instance_icon_paths.retain(|id, _| live.contains(id));
        let requests: Vec<(String, PathBuf)> = self
            .instances
            .iter()
            .filter_map(|instance| {
                instance
                    .icon
                    .clone()
                    .map(|path| (instance.id.clone(), path))
            })
            .collect();
        for (id, path) in requests {
            let unchanged = self.instance_icon_paths.get(&id) == Some(&path)
                && matches!(
                    self.instance_icons.get(&id),
                    Some(ModIconState::Ready(_) | ModIconState::Loading)
                );
            if unchanged {
                continue;
            }
            self.instance_icons.remove(&id);
            self.request_instance_icon(context, id, path);
        }
    }

    fn begin_edit(&mut self, instance: Instance) {
        self.edit_icon = instance
            .icon
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        self.edit_java = instance
            .java_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        self.edit_jvm_args = instance.jvm_args.join("\n");
        self.edit_game_args = instance.game_args.join("\n");
        self.edit_env = instance.env.join("\n");
        self.edit_min_ram = instance.min_ram_mb.unwrap_or(512);
        self.edit_custom_min_ram = instance.min_ram_mb.is_some();
        self.edit_custom_window =
            instance.window_width.is_some() || instance.window_height.is_some();
        self.edit_window_width = instance.window_width.unwrap_or(1280);
        self.edit_window_height = instance.window_height.unwrap_or(720);
        self.edit_notes = instance.notes.clone();
        self.edit_group = instance.group.clone();
        self.edit_quick_join = instance.quick_join.clone().unwrap_or_default();
        self.edit_pre_launch = instance.pre_launch_command.clone().unwrap_or_default();
        self.edit_post_exit = instance.post_exit_command.clone().unwrap_or_default();
        self.editing = Some(instance);
    }

    fn close_editor(&mut self) {
        self.editing = None;
        self.edit_icon.clear();
        self.edit_java.clear();
        self.edit_jvm_args.clear();
        self.edit_game_args.clear();
        self.edit_env.clear();
        self.edit_notes.clear();
        self.edit_group.clear();
        self.edit_quick_join.clear();
        self.edit_pre_launch.clear();
        self.edit_post_exit.clear();
        self.confirm_mod_delete = None;
    }

    fn save_instance_edit(&mut self, context: &egui::Context) {
        if !self.prepare_editor_artwork() {
            return;
        }
        let saved = {
            let Some(edit) = self.editing.as_mut() else {
                return;
            };
            let name = edit.name.trim().to_owned();
            let version = edit.mc_version.trim().to_owned();
            let loader = edit.loader.trim().to_owned();
            let group = self.edit_group.trim().to_owned();
            let notes = self.edit_notes.trim().to_owned();
            let quick_join = self.edit_quick_join.trim().to_owned();
            let pre_launch = self.edit_pre_launch.trim().to_owned();
            let post_exit = self.edit_post_exit.trim().to_owned();
            if name.is_empty() {
                Err("Instance name cannot be empty.".to_owned())
            } else if name.chars().count() > 64 {
                Err("Instance name must be 64 characters or fewer.".to_owned())
            } else if self
                .instances
                .iter()
                .any(|item| item.id != edit.id && item.name.eq_ignore_ascii_case(&name))
            {
                Err(format!(
                    "An instance named \"{name}\" already exists. Pick a unique name."
                ))
            } else if !valid_instance_version(&version) {
                Err("Minecraft version must use letters, digits, '.', '_' or '-'.".to_owned())
            } else if loader.is_empty() {
                Err("Loader cannot be empty; use 'vanilla' for an unmodded instance.".to_owned())
            } else if group.chars().count() > 48 {
                Err("Group must be 48 characters or fewer.".to_owned())
            } else if notes.chars().count() > 4000 {
                Err("Notes must be 4,000 characters or fewer.".to_owned())
            } else if !quick_join.is_empty() && !valid_quick_join(&quick_join) {
                Err("Quick join must be a host or host:port without spaces or paths.".to_owned())
            } else if pre_launch.chars().count() > 4096 || post_exit.chars().count() > 4096 {
                Err("Launch commands must be 4,096 characters or fewer.".to_owned())
            } else {
                let icon_text = self.edit_icon.trim();
                let icon = if icon_text.is_empty() {
                    Some(None)
                } else {
                    let path = PathBuf::from(icon_text);
                    path.is_file().then_some(Some(path))
                };
                let java_text = self.edit_java.trim();
                let java = if java_text.is_empty() {
                    Some(None)
                } else {
                    let path = PathBuf::from(java_text);
                    path.is_file().then_some(Some(path))
                };
                if icon.is_none() {
                    Err(
                        "Artwork not found. Use a PNG, JPEG, WebP, GIF, BMP, ICO, or SVG \
                         file, a data URL, or base64."
                            .to_owned(),
                    )
                } else if java.is_none() {
                    Err(
                        "Java executable not found. Clear the field to use the managed runtime."
                            .to_owned(),
                    )
                } else {
                    edit.name = name;
                    edit.mc_version = version;
                    edit.loader = loader;
                    edit.icon = icon.unwrap_or_default();
                    edit.java_path = java.unwrap_or_default();
                    edit.jvm_args = parse_jvm_args(&self.edit_jvm_args);
                    edit.game_args = parse_jvm_args(&self.edit_game_args);
                    edit.env = parse_jvm_args(&self.edit_env);
                    edit.ram_mb = edit.ram_mb.clamp(512, 65_536);
                    let max_ram = edit.ram_mb;
                    edit.min_ram_mb = self
                        .edit_custom_min_ram
                        .then(|| self.edit_min_ram.clamp(512, max_ram));
                    edit.group = group;
                    edit.notes = notes;
                    edit.quick_join = (!quick_join.is_empty()).then_some(quick_join);
                    edit.pre_launch_command = (!pre_launch.is_empty()).then_some(pre_launch);
                    edit.post_exit_command = (!post_exit.is_empty()).then_some(post_exit);
                    if self.edit_custom_window {
                        edit.window_width = Some(self.edit_window_width.clamp(320, 16_384));
                        edit.window_height = Some(self.edit_window_height.clamp(240, 16_384));
                    } else {
                        edit.window_width = None;
                        edit.window_height = None;
                    }
                    edit.save()
                        .map(|_| edit.clone())
                        .map_err(|error| format!("Could not save instance properties: {error}"))
                }
            }
        };
        let saved = match saved {
            Ok(saved) => saved,
            Err(error) => {
                self.set_status(StatusKind::Error, error);
                return;
            }
        };
        if let Some(slot) = self
            .instances
            .iter_mut()
            .find(|instance| instance.id == saved.id)
        {
            *slot = saved.clone();
        }
        self.settings_ram_mb = saved.ram_mb;
        self.settings_jvm_args = saved.jvm_args.join("\n");
        self.edit_java = saved
            .java_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        self.edit_jvm_args = saved.jvm_args.join("\n");
        self.edit_game_args = saved.game_args.join("\n");
        self.edit_env = saved.env.join("\n");
        self.refresh_instance_cards();
        self.instance_icons.remove(&saved.id);
        if let Some(path) = saved.icon.clone() {
            self.request_instance_icon(context, saved.id.clone(), path);
        }
        let running = self.running_instances.contains(&saved.id);
        self.editing = Some(saved);
        self.set_status(
            StatusKind::Success,
            if running {
                "Instance properties saved. Restart the running game to apply launch changes."
            } else {
                "Instance properties saved."
            },
        );
    }

    /// Everything persisted outside instance JSON. One construction point so a
    /// new field can never be dropped by one of the six save paths.
    /// The switches this launcher remembers between runs.
    ///
    /// Gathered into one comparable value so the frame loop can notice a
    /// checkbox change once, instead of every checkbox remembering to save.
    fn ui_switches(&self) -> UiSwitches {
        UiSwitches {
            show_snapshots: self.show_snapshots,
            show_alpha: self.show_alpha,
            show_beta: self.show_beta,
            install_dependencies: self.install_dependencies,
            search_modrinth: self.mod_show_modrinth,
            search_curseforge: self.mod_show_curseforge,
            search_as_typed: self.mod_auto_search,
        }
    }

    /// Write settings once when a remembered switch changed.
    fn persist_switches_if_changed(&mut self) {
        let current = self.ui_switches();
        if current != self.ui_switches_saved {
            self.ui_switches_saved = current;
            self.persist_settings();
        }
    }

    /// Follow the window's own size and write it once the drag settles.
    ///
    /// A maximized window is skipped on purpose: recording the monitor size
    /// would reopen the launcher pinned to a full screen, so the last windowed
    /// size is what survives.
    fn track_window_size(&mut self, context: &egui::Context) {
        const SETTLE: std::time::Duration = std::time::Duration::from_millis(500);
        let viewport = context.input(|input| input.viewport().clone());
        if viewport.maximized == Some(true) {
            self.window_size_due = None;
        } else if let Some(rect) = viewport.inner_rect {
            let size = [rect.width(), rect.height()];
            if (size[0] - self.window_size[0]).abs() > 0.5
                || (size[1] - self.window_size[1]).abs() > 0.5
            {
                self.window_size = size;
                self.window_size_due = Some(std::time::Instant::now() + SETTLE);
            }
        }
        if let Some(due) = self.window_size_due {
            let now = std::time::Instant::now();
            if now >= due {
                self.window_size_due = None;
                self.persist_settings();
            } else {
                context.request_repaint_after(due - now);
            }
        }
    }

    fn settings_snapshot(&self) -> LauncherSettings {
        LauncherSettings {
            last_account: self.last_account.clone(),
            account_mode: Some(self.account_mode),
            auto_repair: self.auto_repair_before_launch,
            auto_resolve_duplicates: self.auto_resolve_duplicates,
            launch_at_startup: self.launch_at_startup,
            content_kind: self.content_kind,
            offline_account: self
                .offline_account
                .as_ref()
                .map(|profile| profile.name.clone())
                .unwrap_or_default(),
            accounts: self.accounts.clone(),
            ui_scale: self.ui_scale,
            window_size: self.window_size,
            show_snapshots: self.show_snapshots,
            show_alpha: self.show_alpha,
            show_beta: self.show_beta,
            install_dependencies: self.install_dependencies,
            search_modrinth: self.mod_show_modrinth,
            search_curseforge: self.mod_show_curseforge,
            search_as_typed: self.mod_auto_search,
        }
    }

    fn persist_settings(&self) {
        let _ = save_settings(&self.settings_snapshot());
    }

    /// Follow the display in automatic mode, or follow Ctrl +/- in manual mode.
    ///
    /// The basis is the window height in native-DPI points: screen height
    /// times the current zoom. Changing the zoom shrinks the logical canvas by
    /// exactly that factor, so the basis stays put and a resolved step cannot
    /// feed itself back into the next frame. A resize or a move to another
    /// monitor changes the basis, which is the only thing that re-resolves.
    fn apply_interface_scale(&mut self, context: &egui::Context) {
        let zoom = context.zoom_factor();
        if self.ui_scale > 0.0 {
            // Manual choice. egui's Ctrl +/- lands here as a real change.
            if (zoom - self.applied_zoom).abs() > 0.001 {
                self.applied_zoom = zoom;
                self.ui_scale = zoom;
                self.persist_settings();
            }
            return;
        }
        // A zoom change while Auto is selected is the user's Ctrl +/-; keep
        // it and make it the saved manual step rather than snapping back.
        if (zoom - self.applied_zoom).abs() > 0.001 && !self.auto_scale_pending {
            self.applied_zoom = zoom;
            self.ui_scale = zoom;
            self.persist_settings();
            return;
        }
        let screen = context.screen_rect().size();
        // The first frame can report a surface that is not sized yet, so wait
        // rather than latching a scale for a 0×0 window.
        if screen.y < 200.0 || screen.x < 320.0 {
            return;
        }
        let basis = screen.y * zoom;
        if !self.auto_scale_pending && (basis - self.auto_basis).abs() < 8.0 {
            return;
        }
        let target = if self.auto_scale_pending {
            auto_scale_for(basis)
        } else {
            // A dead zone around each boundary stops a window resized onto a
            // threshold from flipping between two steps every frame.
            auto_scale_for_settled(basis, self.applied_zoom)
        };
        self.auto_basis = basis;
        self.auto_scale_pending = false;
        if (target - zoom).abs() > 0.001 {
            context.set_zoom_factor(target);
            self.applied_zoom = target;
            context.request_repaint();
        } else {
            self.applied_zoom = zoom;
        }
    }

    /// A Tab press turns the focus ring on; a pointer press turns it off again,
    /// so the ring never lingers after a mouse-driven focus. No chord is
    /// consumed here: the launcher advertises no shortcuts, so nothing is taken
    /// away from the widgets before they see it.
    fn handle_keyboard_navigation(&mut self, context: &egui::Context) {
        if context.input(|input| input.key_pressed(egui::Key::Tab)) {
            self.keyboard_nav = true;
        }
        if context.input(|input| input.pointer.any_pressed()) {
            self.keyboard_nav = false;
        }
    }

    /// Switch to automatic scale now, and resolve it against the current
    /// display on the next frame.
    fn use_auto_scale(&mut self, context: &egui::Context) {
        context.set_zoom_factor(1.0);
        self.applied_zoom = 1.0;
        self.ui_scale = 0.0;
        self.auto_basis = 0.0;
        self.auto_scale_pending = true;
        self.persist_settings();
    }

    fn set_ui_scale(&mut self, context: &egui::Context, scale: f32) {
        let scale = scale.clamp(0.85, 1.5);
        context.set_zoom_factor(scale);
        self.applied_zoom = scale;
        self.ui_scale = scale;
        self.auto_scale_pending = false;
        self.persist_settings();
    }

    fn install_vanilla_instance(&mut self, context: egui::Context, instance: Instance) {
        if self.installing_instance.is_some() || self.installing_runtime.is_some() {
            self.set_status(
                StatusKind::Warning,
                "Wait for the current installation to finish.",
            );
            return;
        }
        let Some(version) = self
            .versions
            .iter()
            .find(|version| version.id == instance.mc_version)
            .cloned()
        else {
            self.set_status(
                StatusKind::Error,
                format!(
                    "Could not find Mojang metadata for Minecraft {}. Refresh versions and retry.",
                    instance.mc_version
                ),
            );
            return;
        };
        let instance_id = instance.id.clone();
        self.installing_instance = Some(instance_id.clone());
        self.set_status(
            StatusKind::Busy,
            format!("Preparing vanilla Minecraft {}…", instance.mc_version),
        );
        let client = self.http.clone();
        let root = instance.root;
        let sender = self.sender.clone();
        let repaint = context;
        let progress_sender = sender.clone();
        let progress_context = repaint.clone();
        let progress_instance_id = instance_id.clone();
        self.runtime.spawn(async move {
            let result =
                kaelis_api::vanilla::install_vanilla(&client, version, root, move |progress| {
                    send_event(
                        &progress_sender,
                        &progress_context,
                        GuiEvent::VanillaProgress {
                            instance_id: progress_instance_id.clone(),
                            progress,
                        },
                    );
                })
                .await
                .map_err(|error| error.to_string());
            send_event(
                &sender,
                &repaint,
                GuiEvent::VanillaInstalled {
                    instance_id,
                    result,
                },
            );
        });
    }

    fn create_instance(&mut self, context: egui::Context) {
        if self.installing_instance.is_some() || self.installing_runtime.is_some() {
            self.set_status(
                StatusKind::Warning,
                "Wait for the current installation to finish.",
            );
            return;
        }
        let name = self.new_instance_name.trim();
        let version = self.new_instance_version.trim();
        if name.is_empty() || version.is_empty() {
            self.set_status(
                StatusKind::Warning,
                "Enter an instance name and a Minecraft version.",
            );
            return;
        }
        if let Some(existing) = instance_name_conflict(&self.instances, name) {
            self.set_status(StatusKind::Warning, name_conflict_text(name, existing));
            return;
        }
        let id = slugify(name);
        let root = Instance::instances_dir().join(&id);
        let loader = self.new_instance_loader.trim();
        let instance = Instance::new(
            &id,
            name,
            version,
            if loader.is_empty() { "vanilla" } else { loader },
            root,
        );
        // A leftover record with no live instance above it is dead metadata
        // from an interrupted install; `save` replaces it instead of refusing
        // the name, which used to leave the user with no visible instance and
        // no way to reuse its own name.
        let result = std::fs::create_dir_all(&instance.root)
            .map_err(|error| error.to_string())
            .and_then(|()| instance.save().map_err(|error| error.to_string()));
        match result {
            Ok(_) => {
                self.select_instance(&id);
                self.instances.insert(0, instance.clone());
                self.refresh_instance_cards();
                self.install_vanilla_instance(context, instance);
            }
            Err(error) => self.set_status(
                StatusKind::Error,
                format!("Could not create instance: {error}"),
            ),
        }
    }

    fn install_runtime_for_instance(&mut self, context: egui::Context, instance: Instance) {
        if self.installing_instance.is_some() || self.installing_runtime.is_some() {
            self.set_status(
                StatusKind::Warning,
                "Wait for the current installation to finish.",
            );
            return;
        }
        let instance_id = instance.id.clone();
        self.installing_runtime = Some(instance_id.clone());
        self.set_status(StatusKind::Busy, "Resolving the compatible Java runtime…");
        let client = self.http.clone();
        let sender = self.sender.clone();
        let repaint = context;
        let progress_sender = sender.clone();
        let progress_context = repaint.clone();
        let progress_instance_id = instance_id.clone();
        self.runtime.spawn(async move {
            let result =
                kaelis_api::runtime::install_for_instance(&client, &instance, move |progress| {
                    send_event(
                        &progress_sender,
                        &progress_context,
                        GuiEvent::RuntimeProgress {
                            instance_id: progress_instance_id.clone(),
                            progress,
                        },
                    );
                })
                .await
                .map_err(|error| error.to_string());
            send_event(
                &sender,
                &repaint,
                GuiEvent::RuntimeInstalled {
                    instance_id,
                    result,
                },
            );
        });
    }

    /// Download the loader libraries for an instance that just finished its
    /// vanilla install, so Fabric/Quilt mods can actually run.
    fn provision_loader(&mut self, context: egui::Context, instance_id: String) {
        let Some(instance) = self
            .instances
            .iter()
            .find(|instance| instance.id == instance_id)
            .cloned()
        else {
            return;
        };
        let loader = instance.loader.to_ascii_lowercase();
        if !matches!(loader.as_str(), "fabric" | "quilt")
            || kaelis_api::loader::load_for(&instance.root, &instance.mc_version, &loader).is_some()
        {
            return;
        }
        let client = self.http.clone();
        let sender = self.sender.clone();
        let repaint = context;
        let game_version = instance.mc_version.clone();
        let label = if loader == "quilt" { "Quilt" } else { "Fabric" };
        self.runtime.spawn(async move {
            let message = match kaelis_api::loader::provision(
                &client,
                &instance.root,
                &game_version,
                &loader,
            )
            .await
            {
                Ok(Some(install)) => format!(
                    "{label} loader {} installed for {} ({} libraries).",
                    install.loader_version, install.game_version, install.libraries
                ),
                Ok(None) => return,
                Err(error) => format!("{label} loader install failed: {error}"),
            };
            send_event(&sender, &repaint, GuiEvent::Notice(message));
        });
    }

    fn sign_in_offline(&mut self, context: egui::Context) {
        let name = self.offline_name.trim();
        if !valid_username(name) {
            self.auth_status = "Offline names use 3–16 letters, digits, or underscores.".to_owned();
            return;
        }
        if let Some(cancel) = self.auth_cancel.take() {
            let _ = cancel.send(true);
        }
        self.auth_busy = false;
        self.auth_device = None;
        self.microsoft_session = None;
        let profile = offline_profile(name);
        self.auth_status = format!("Offline profile ready: {}", profile.name);
        self.offline_account = Some(profile.clone());
        self.microsoft_session = None;
        self.apply_offline_head(&context, &profile.name);
        self.request_offline_skin(&context, &profile.name);
        self.upsert_account(AccountKind::Offline, &profile.name, &profile.uuid);
    }

    /// Read the account files Prism, MultiMC, PolyMC, and the official
    /// launcher leave on this machine. Disk work stays off the UI thread: a
    /// network drive or a large launcher data folder must not freeze a frame.
    fn scan_for_accounts(&mut self, context: &egui::Context, announce: bool) {
        let sender = self.sender.clone();
        let repaint = context.clone();
        if announce {
            self.auth_status = "Looking for account files…".to_owned();
        }
        self.runtime.spawn_blocking(move || {
            let scan = account_import::scan_known_files();
            if scan.accounts.is_empty() {
                // A silent first-run scan must not put a warning in the status
                // bar just because this computer has no other launcher.
                if announce {
                    let result = Err("no account file from another launcher was found".to_owned());
                    send_event(&sender, &repaint, GuiEvent::AccountsImported(result));
                }
                return;
            }
            send_event(&sender, &repaint, GuiEvent::AccountsImported(Ok(scan)));
        });
    }

    /// Import an account list the user picks. Any JSON with player records is
    /// accepted, not only a filename the launcher recognises.
    fn pick_account_file(&mut self, context: &egui::Context) {
        let sender = self.sender.clone();
        let repaint = context.clone();
        self.runtime.spawn(async move {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title("Import accounts")
                .add_filter("Account files", &["json", "txt"])
                .pick_file()
                .await
            else {
                return;
            };
            let path = file.path().to_path_buf();
            let read = tokio::task::spawn_blocking({
                let path = path.clone();
                move || {
                    account_import::import_file(&path)
                        .map(|accounts| ImportScan {
                            accounts,
                            files: vec![path.display().to_string()],
                        })
                        .map_err(|error| error.to_string())
                }
            })
            .await;
            let result = match read {
                Ok(result) => result,
                Err(error) => Err(error.to_string()),
            };
            send_event(&sender, &repaint, GuiEvent::AccountsImported(result));
        });
    }

    /// Fold imported identities into the saved list without changing which
    /// account is active. Existing rows keep their place; a late UUID fills an
    /// empty one instead of replacing the row.
    fn merge_imported_accounts(&mut self, accounts: Vec<ImportedAccount>) -> (usize, usize) {
        let mut added = 0;
        let mut refreshed = 0;
        for imported in accounts {
            let saved = imported.saved();
            if let Some(existing) = self.accounts.iter_mut().find(|account| {
                account.kind == saved.kind && account.name.eq_ignore_ascii_case(&saved.name)
            }) {
                if existing.uuid.is_empty() && !saved.uuid.is_empty() {
                    existing.uuid = saved.uuid;
                    refreshed += 1;
                }
                continue;
            }
            self.accounts.push(saved);
            added += 1;
        }
        if added > 0 || refreshed > 0 {
            self.persist_settings();
        }
        (added, refreshed)
    }

    fn active_identity_matches(&self, account: &SavedAccount) -> bool {
        match account.kind {
            AccountKind::Microsoft => self
                .microsoft_session
                .as_ref()
                .is_some_and(|session| session.profile.name.eq_ignore_ascii_case(&account.name)),
            AccountKind::Offline => self
                .offline_account
                .as_ref()
                .is_some_and(|profile| profile.name.eq_ignore_ascii_case(&account.name)),
        }
    }

    /// Make a finished Microsoft session the current identity: its row enters
    /// Accounts, the mode and preferred name follow it, and the whole choice is
    /// written to disk in the same step. Every path that ends in a live session
    /// — the device flow, the keychain restore, and the Account tab's heal —
    /// funnels through here, so a signed-in account cannot be missing from the
    /// list. Returns the settings write so the caller can say so.
    fn adopt_microsoft_session(
        &mut self,
        session: AuthSession,
        context: &egui::Context,
    ) -> Result<(), String> {
        let name = session.profile.name.clone();
        let uuid = session.profile.id.clone();
        self.offline_account = None;
        self.avatar = None;
        self.account_mode = AccountMode::Microsoft;
        self.microsoft_session = Some(session);
        self.upsert_account(AccountKind::Microsoft, &name, &uuid);
        if !uuid.is_empty() {
            self.request_avatar(context.clone(), uuid);
        }
        save_settings(&self.settings_snapshot()).map_err(|error| error.to_string())
    }

    /// Insert or refresh one identity. The active row moves to the end so the
    /// list reads newest-last without a separate timestamp sort.
    fn upsert_account(&mut self, kind: AccountKind, name: &str, uuid: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if let Some(index) = self
            .accounts
            .iter()
            .position(|account| account.kind == kind && account.name.eq_ignore_ascii_case(name))
        {
            let mut account = self.accounts.remove(index);
            if !uuid.is_empty() {
                account.uuid = uuid.to_owned();
            }
            self.accounts.push(account);
        } else {
            self.accounts.push(SavedAccount::new(kind, name, uuid));
        }
        self.account_mode = match kind {
            AccountKind::Microsoft => AccountMode::Microsoft,
            AccountKind::Offline => AccountMode::Offline,
        };
        if kind == AccountKind::Microsoft {
            self.last_account = name.to_owned();
        }
        self.persist_settings();
    }

    /// Switch identity. Offline rows are instant; a Microsoft row restores its
    /// saved token in the background, so the window never blocks on a keychain
    /// read or a network round trip.
    fn activate_account(&mut self, index: usize, context: &egui::Context) {
        let Some(account) = self.accounts.get(index).cloned() else {
            return;
        };
        match account.kind {
            AccountKind::Offline => {
                let profile = offline_profile(&account.name);
                self.microsoft_session = None;
                self.offline_account = Some(profile.clone());
                self.offline_name = profile.name.clone();
                self.auth_status = format!("Offline profile ready: {}", profile.name);
                self.apply_offline_head(context, &profile.name);
                self.request_offline_skin(context, &profile.name);
                self.upsert_account(AccountKind::Offline, &profile.name, &profile.uuid);
            }
            AccountKind::Microsoft => {
                self.account_mode = AccountMode::Microsoft;
                self.offline_account = None;
                self.avatar = None;
                self.auth_busy = true;
                self.auth_status = format!("Restoring {}…", account.name);
                let sender = self.sender.clone();
                let repaint = context.clone();
                let name = account.name.clone();
                self.runtime.spawn(async move {
                    let token = tokio::task::spawn_blocking({
                        let name = name.clone();
                        move || kaelis_api::auth::load_refresh_token(&name)
                    })
                    .await;
                    let result = match token {
                        Ok(Ok(_)) => match MicrosoftAuth::new() {
                            Ok(auth) => auth
                                .login_with_refresh_token(&name)
                                .await
                                .map_err(|error| error.to_string()),
                            Err(error) => Err(error.to_string()),
                        },
                        Ok(Err(error)) => Err(format!(
                            "{name} needs a fresh sign-in before it can play online: {error}"
                        )),
                        Err(error) => Err(error.to_string()),
                    };
                    send_event(&sender, &repaint, GuiEvent::Restored(result));
                });
            }
        }
    }

    /// Drop one saved identity. Removing a Microsoft row also deletes its
    /// keychain token; no other account is touched.
    fn remove_account(&mut self, index: usize, context: &egui::Context) {
        if index >= self.accounts.len() {
            return;
        }
        let account = self.accounts.remove(index);
        self.account_heads.remove(&account.key());
        self.account_head_pending.remove(&account.key());
        self.account_head_retry.remove(&account.key());
        if self.active_identity_matches(&account) {
            self.microsoft_session = None;
            self.offline_account = None;
            self.avatar = None;
            self.auth_status = format!("Removed {}. No identity is active.", account.name);
        }
        if self.last_account.eq_ignore_ascii_case(&account.name) {
            self.last_account.clear();
        }
        self.persist_settings();
        if account.kind == AccountKind::Microsoft {
            let name = account.name.clone();
            let sender = self.sender.clone();
            let repaint = context.clone();
            self.runtime.spawn_blocking(move || {
                let message = match kaelis_api::auth::delete_refresh_token(&name) {
                    Ok(()) => format!("Removed {name} and its saved token."),
                    Err(error) => format!("Removed {name}; token cleanup failed: {error}"),
                };
                send_event(&sender, &repaint, GuiEvent::Notice(message));
            });
        }
    }

    /// Sign out of the active identity and delete its row.
    fn forget_account(&mut self, context: egui::Context) {
        if let Some(index) = self.active_account_index() {
            self.remove_account(index, &context);
            return;
        }
        self.microsoft_session = None;
        self.offline_account = None;
        self.avatar = None;
        self.persist_settings();
        self.auth_status = "Signed out.".to_owned();
    }

    fn ensure_steve_texture(&mut self, context: &egui::Context) {
        if self.steve_head_texture.is_none() {
            self.steve_head_texture = Some(context.load_texture(
                "kaelis-steve-head",
                steve_head(),
                egui::TextureOptions::NEAREST,
            ));
        }
    }

    fn active_account_index(&self) -> Option<usize> {
        self.accounts
            .iter()
            .position(|account| self.active_identity_matches(account))
    }

    /// Fetch a head for every visible row once. Results are cached per row,
    /// including "no premium skin", so the list never re-requests a name.
    fn ensure_account_heads(&mut self, context: &egui::Context) {
        const HEAD_CONCURRENCY: usize = 3;
        if self.account_head_pending.len() >= HEAD_CONCURRENCY {
            return;
        }
        for index in 0..self.accounts.len() {
            if self.account_head_pending.len() >= HEAD_CONCURRENCY {
                break;
            }
            let account = self.accounts[index].clone();
            let key = account.key();
            if self.account_heads.contains_key(&key) || self.account_head_pending.contains(&key) {
                continue;
            }
            if let Some(at) = self.account_head_retry.get(&key) {
                if at.elapsed() < std::time::Duration::from_secs(60) {
                    continue;
                }
                self.account_head_retry.remove(&key);
            }
            let uuid = match account.kind {
                AccountKind::Offline => {
                    if !valid_username(account.name.trim()) {
                        continue;
                    }
                    // A resolved name is reused, including "owns no skin".
                    if let Some(cached) = self
                        .offline_skins
                        .get(&account.name.to_ascii_lowercase())
                        .cloned()
                    {
                        let texture = cached.map(|image| {
                            context.load_texture(
                                format!("kaelis-account-head-{key}"),
                                image,
                                egui::TextureOptions::NEAREST,
                            )
                        });
                        self.account_heads.insert(key, texture);
                        continue;
                    }
                    // Route through the shared offline fetch: it is debounced,
                    // single-flight, and primes this row when it lands.
                    self.request_offline_skin(context, &account.name);
                    continue;
                }
                AccountKind::Microsoft => {
                    if account.uuid.is_empty() {
                        continue;
                    }
                    account.uuid.clone()
                }
            };
            self.account_head_pending.insert(key.clone());
            let sender = self.sender.clone();
            let repaint = context.clone();
            let client = self.http.clone();
            let name = account.name.clone();
            self.runtime.spawn(async move {
                // A transport failure and "this name owns no skin" are different
                // answers: only the second one is worth caching.
                let fetched = if uuid.is_empty() {
                    fetch_head_for_username(&client, &name, 64).await.ok()
                } else {
                    fetch_crafatar_head(&client, &uuid, 64).await.ok().map(Some)
                };
                match fetched {
                    Some(bytes) => send_event(
                        &sender,
                        &repaint,
                        GuiEvent::AccountHead {
                            key,
                            result: bytes.as_deref().and_then(decode_head),
                            failed: false,
                        },
                    ),
                    None => send_event(
                        &sender,
                        &repaint,
                        GuiEvent::AccountHead {
                            key,
                            result: None,
                            failed: true,
                        },
                    ),
                }
            });
        }
    }

    /// Recover games that outlived the launcher window.
    ///
    /// A record is written when the JVM spawns and is checked against the live
    /// process table here. A watcher thread turns the eventual exit into the
    /// same event an in-window launch sends, so the card, the session cleanup,
    /// and the tray all agree — and Quit works on a game this window never
    /// spawned itself.
    fn adopt_running_games(&mut self, context: &egui::Context) {
        self.running_instances.clear();
        let mut adopted = 0_usize;
        for (id, game) in kaelis_core::running::live() {
            if !self.instances.iter().any(|instance| instance.id == id) {
                // The instance was erased while its game ran; do not keep a
                // record for a folder that no longer exists.
                kaelis_core::running::clear(&id);
                continue;
            }
            self.running_instances.insert(id.clone());
            adopted += 1;
            let sender = self.sender.clone();
            let repaint = context.clone();
            let _ = std::thread::Builder::new()
                .name("kaelis-adopted-game".to_owned())
                .spawn(move || {
                    kaelis_core::running::wait_until_gone(&game);
                    kaelis_core::running::clear(&id);
                    send_event(
                        &sender,
                        &repaint,
                        GuiEvent::GameExited {
                            instance_id: id,
                            message: "Minecraft exited.".to_owned(),
                        },
                    );
                });
        }
        if adopted > 0 {
            self.set_status(
                StatusKind::Info,
                format!(
                    "{adopted} game{} still running after the launcher closed.",
                    if adopted == 1 { " is" } else { "s are" }
                ),
            );
        }
    }

    fn instance_running(&self, id: &str) -> bool {
        self.running_instances.contains(id)
    }

    /// End one game process now.
    ///
    /// The launch task owns the child, so Quit travels through the same watch
    /// channel it already selects on: `start_kill` runs there, finishes the
    /// exit event, and the card returns to Play without a `taskkill` spawn or
    /// a UI stall. A signal that arrives while Java is still being prepared is
    /// kept and applied the instant the process exists.
    fn stop_instance(&mut self, id: &str, context: &egui::Context) {
        let Some(stop) = self.stop_signals.get(id) else {
            // The game predates this window: the launcher was closed and
            // reopened while Minecraft kept playing. Its process record is the
            // authority, so Quit still ends it.
            let Some(game) = kaelis_core::running::find(id) else {
                return;
            };
            let name = self
                .instances
                .iter()
                .find(|instance| instance.id == id)
                .map(|instance| instance.name.clone())
                .unwrap_or_else(|| id.to_owned());
            if kaelis_core::running::terminate(&game) {
                self.set_status(StatusKind::Busy, format!("Quitting {name}…"));
            } else {
                self.set_status(StatusKind::Warning, format!("{name} has already exited."));
            }
            context.request_repaint();
            return;
        };
        if stop.send(true).is_err() {
            self.stop_signals.remove(id);
            return;
        }
        let name = self
            .instances
            .iter()
            .find(|instance| instance.id == id)
            .map(|instance| instance.name.clone())
            .unwrap_or_else(|| id.to_owned());
        self.set_status(StatusKind::Busy, format!("Quitting {name}…"));
        context.request_repaint();
    }

    /// End the launcher process on the spot.
    ///
    /// Every setting is written when it changes and the game log lives in RAM,
    /// so there is nothing to flush. The tray icon is dropped first because
    /// Windows leaves a dead application's icon in the notification area until
    /// it is hovered; the toolkit close path would queue a frame on a hidden
    /// window and unwind every editor, worker, and runtime handle before the
    /// process ended.
    fn quit_now(&mut self) -> ! {
        // A resize made less than half a second before Quit is still the size
        // the user wants back, so the final geometry is written here as well as
        // by the settle timer.
        self.persist_settings();
        self._tray = None;
        std::process::exit(0)
    }

    fn launch_selected(&mut self, context: egui::Context) {
        let Some(instance) = self.selected_instance().cloned() else {
            self.set_status(
                StatusKind::Warning,
                "Create or select an instance before launching.",
            );
            return;
        };
        if self.instance_running(&instance.id) {
            self.set_status(
                StatusKind::Warning,
                format!("{} is already running.", instance.name),
            );
            return;
        }
        let identity = if let Some(session) = &self.microsoft_session {
            LaunchIdentity::microsoft(
                session.profile.name.clone(),
                session.profile.id.clone(),
                session.access_token().to_owned(),
            )
        } else if let Some(profile) = &self.offline_account {
            LaunchIdentity::offline(profile.name.clone(), profile.uuid.clone())
        } else {
            self.set_status(
                StatusKind::Warning,
                "Sign in or select an offline profile before launching.",
            );
            self.current_tab = Tab::Account;
            return;
        };

        self.continue_launch(instance, identity, context);
    }

    /// The last gate before Java: make the instance whole, settle mod
    /// dependencies, then start the game.
    fn continue_launch(
        &mut self,
        instance: Instance,
        identity: LaunchIdentity,
        context: egui::Context,
    ) {
        // A JVM, the client files, and the loader are what a launch cannot do
        // without. They are repaired from Mojang's metadata and the launch
        // continues, so Play is never a dead end on a fresh or damaged
        // instance.
        let readiness = kaelis_api::health::check(&instance);
        if !readiness.ready() {
            self.pending_launch = Some((instance.clone(), identity));
            self.set_status(StatusKind::Busy, readiness.summary());
            self.begin_instance_repair(context, instance);
            return;
        }
        // A launch is the moment missing dependencies actually matter, so
        // resolve them first instead of failing inside the game's log.
        let missing = self
            .installed
            .as_ref()
            .map(|scan| !scan.missing.is_empty())
            .unwrap_or(false);
        let loader_missing = matches!(
            instance.loader.to_ascii_lowercase().as_str(),
            "fabric" | "quilt"
        ) && kaelis_api::loader::load_for(
            &instance.root,
            &instance.mc_version,
            &instance.loader,
        )
        .is_none();
        if (missing || loader_missing) && self.auto_repair_before_launch {
            self.pending_launch = Some((instance, identity));
            self.repair_mod_environment(context);
            return;
        }
        self.start_launch(instance, identity, context);
    }

    /// Fill an instance's missing pieces off the UI thread, reporting each step
    /// to the status bar, then continue the launch that asked for it.
    fn begin_instance_repair(&mut self, context: egui::Context, mut instance: Instance) {
        let instance_id = instance.id.clone();
        let client = self.http.clone();
        let sender = self.sender.clone();
        let repaint = context;
        let progress_sender = sender.clone();
        let progress_context = repaint.clone();
        let progress_instance_id = instance_id.clone();
        self.runtime.spawn(async move {
            let result = kaelis_api::health::repair(&client, &mut instance, move |update| {
                let (message, progress) = match update {
                    kaelis_api::health::RepairProgress::Message(message) => (message, None),
                    kaelis_api::health::RepairProgress::Download { completed, total } => (
                        format!("Downloading ({completed}/{total})…"),
                        Some((completed, total)),
                    ),
                };
                send_event(
                    &progress_sender,
                    &progress_context,
                    GuiEvent::InstanceRepairProgress {
                        instance_id: progress_instance_id.clone(),
                        message,
                        progress,
                    },
                );
            })
            .await
            .map(|report| report.summary());
            send_event(
                &sender,
                &repaint,
                GuiEvent::InstanceRepaired {
                    instance_id,
                    result,
                },
            );
        });
    }

    fn start_launch(
        &mut self,
        instance: Instance,
        identity: LaunchIdentity,
        context: egui::Context,
    ) {
        let instance = self.with_launch_automation(instance);
        // Claim the slot before the worker runs so a double click cannot queue
        // two JVMs for the same instance. The OS guard in `run_instance` is the
        // final authority; this keeps the UI honest in the meantime.
        self.running_instances.insert(instance.id.clone());
        // The quit path is a channel, not a process lookup: the launch task
        // already owns the child and selects on this flag while it waits.
        let (stop, stop_requested) = tokio::sync::watch::channel(false);
        self.stop_signals.insert(instance.id.clone(), stop);
        let live_game = LiveGame::start(self.live_games.clone());
        self.set_status(StatusKind::Busy, format!("Starting {}…", instance.name));
        let sender = self.sender.clone();
        let repaint = context.clone();
        let started_sender = sender.clone();
        let started_context = repaint.clone();
        let started_id = instance.id.clone();
        let finished_id = instance.id.clone();
        let log = EphemeralLog::new(20_000);
        self.runtime.spawn(async move {
            // Owns the count until this future ends, however it ends.
            let _live_game = live_game;
            let result = run_instance(
                instance,
                identity,
                log.clone(),
                move |_pid| {
                    send_event(
                        &started_sender,
                        &started_context,
                        GuiEvent::GameStarted(started_id.clone()),
                    );
                },
                Some(stop_requested),
            )
            .await;
            log.clear();
            // `_live_game` restores the window when this is the last game; a
            // hidden viewport paints no frames, so the event handler alone
            // cannot be trusted to bring it back.
            match result {
                Ok(status) => send_event(
                    &sender,
                    &repaint,
                    GuiEvent::GameExited {
                        instance_id: finished_id,
                        message: format!("Minecraft exited with {status}."),
                    },
                ),
                Err(error) => send_event(
                    &sender,
                    &repaint,
                    GuiEvent::GameFailed {
                        instance_id: finished_id,
                        error: format!("Could not launch Minecraft: {error}"),
                    },
                ),
            }
        });
    }

    /// Fold enabled user commands into this launch only. The instance file is
    /// untouched; the copy handed to the launch worker carries the chain, so a
    /// "before launch" rule always finishes before Java starts and an "after
    /// exit" rule always runs after the process ends.
    fn with_launch_automation(&self, mut instance: Instance) -> Instance {
        instance.pre_launch_command = merge_launch_command(
            instance.pre_launch_command.take(),
            self.automation.launch_chain(Trigger::PreLaunch),
        );
        instance.post_exit_command = merge_launch_command(
            instance.post_exit_command.take(),
            self.automation.launch_chain(Trigger::PostExit),
        );
        instance
    }

    /// Fire startup rules once, then any interval rule whose window has
    /// elapsed. The nearest deadline becomes a scheduled repaint, so an idle
    /// window wakes exactly when a rule is due instead of polling.
    fn run_automation_tick(&mut self, context: &egui::Context) {
        let now = unix_now();
        let mut due: Vec<Rule> = Vec::new();
        if !self.automation_started {
            self.automation_started = true;
            due.extend(
                self.automation
                    .commands_for(Trigger::Startup)
                    .into_iter()
                    .cloned(),
            );
        }
        let timed = self.automation.take_due(now);
        if !timed.is_empty() {
            // Persist the schedule so a restart does not re-run a rule early.
            let _ = self.automation.save();
            due.extend(timed);
        }
        for rule in due {
            self.spawn_automation(rule, context);
        }
        // Wake for the next interval even when no event arrives.
        let wait = self
            .automation
            .rules()
            .iter()
            .filter(|rule| rule.enabled)
            .filter_map(|rule| match rule.trigger {
                Trigger::Every { secs } => {
                    let due_at = rule.last_run_unix.saturating_add(secs as i64);
                    Some(due_at.saturating_sub(now).max(1) as u64)
                }
                _ => None,
            })
            .min();
        if let Some(seconds) = wait {
            context.request_repaint_after(std::time::Duration::from_secs(seconds));
        }
    }

    /// Run one rule beside the UI thread. The working folder is the selected
    /// instance so a command can use relative paths the same way a hook does.
    fn spawn_automation(&mut self, rule: Rule, context: &egui::Context) {
        let selected = self.selected_instance().cloned();
        let cwd = selected
            .as_ref()
            .map(|instance| instance.root.clone())
            .unwrap_or_else(std::env::temp_dir);
        let env = automation::rule_environment(selected.as_ref());
        let log = EphemeralLog::new(400);
        let sender = self.sender.clone();
        let repaint = context.clone();
        self.set_status(StatusKind::Busy, format!("Running {}…", rule.title()));
        self.runtime.spawn(async move {
            let run = automation::run_rule(&rule, &cwd, &env, &log).await;
            send_event(&sender, &repaint, GuiEvent::Automation(run));
        });
    }

    /// The avatar texture is released while the game runs (the window is
    /// hidden); rebuild it from the skin cache as soon as the launcher returns.
    fn restore_avatar(&mut self, context: &egui::Context) {
        if let Some(profile) = self.offline_account.clone() {
            self.apply_offline_head(context, &profile.name);
            self.request_offline_skin(context, &profile.name);
        } else if let Some(session) = &self.microsoft_session {
            self.request_avatar(context.clone(), session.profile.id.clone());
        }
    }

    fn selected_instance(&self) -> Option<&Instance> {
        self.instances
            .iter()
            .find(|instance| Some(instance.id.as_str()) == self.selected_instance.as_deref())
    }

    fn handle_event(&mut self, context: &egui::Context, event: GuiEvent) {
        match event {
            GuiEvent::ModpackInstalled { generation, result } => {
                if generation != self.modpack_generation {
                    return;
                }
                self.modpack_task = None;
                self.modpack_installing = None;
                self.modpack_detail.clear();
                match *result {
                    Ok(outcome) => {
                        let name = outcome.instance.name.clone();
                        let game_version = outcome.instance.mc_version.clone();
                        let loader = outcome.pack.loader.clone();
                        let id = outcome.instance.id.clone();
                        let imported = outcome.imported;
                        let mods = outcome.pack.mod_files;
                        // Fabric, Quilt, and vanilla can be provisioned end to
                        // end today; anything else imports the files and says so.
                        let launchable = matches!(
                            loader.to_ascii_lowercase().as_str(),
                            "fabric" | "quilt" | "vanilla"
                        );
                        self.instances.insert(0, outcome.instance.clone());
                        self.refresh_instance_cards();
                        if let Some(path) = outcome.instance.icon.clone() {
                            self.request_instance_icon(context, id.clone(), path);
                        }
                        self.select_instance(&id);
                        let text = if launchable {
                            format!(
                                "{name} installed · {imported} files · {mods} mods · MC {game_version} · {loader}"
                            )
                        } else {
                            format!(
                                "{name} imported · {imported} files · MC {game_version} · {loader}; add the loader to play."
                            )
                        };
                        self.set_status(
                            if launchable {
                                StatusKind::Success
                            } else {
                                StatusKind::Warning
                            },
                            text,
                        );
                        self.install_vanilla_instance(context.clone(), outcome.instance);
                    }
                    Err(error) => {
                        self.set_status(
                            StatusKind::Error,
                            format!("Modpack install failed: {error}"),
                        );
                    }
                }
            }
            GuiEvent::DeviceCode(Ok(device)) => {
                // The code is on screen for a few seconds before anyone can
                // retype it, so it is also placed on the clipboard: the player
                // opens the page, pastes, and is done.
                context.copy_text(device.user_code.clone());
                self.auth_code_copied = true;
                self.auth_device_until = Some(
                    std::time::Instant::now()
                        + std::time::Duration::from_secs(device.expires_in.max(1)),
                );
                self.auth_status =
                    "Code copied. Paste it on the Microsoft page; this window finishes on its own."
                        .to_owned();
                // The user just asked to sign in, so opening the verification
                // page is expected and removes a step from the device flow.
                let url = device
                    .verification_uri_complete
                    .clone()
                    .unwrap_or_else(|| device.verification_uri.clone());
                context.open_url(egui::OpenUrl::new_tab(url));
                self.auth_device = Some(device);
            }
            GuiEvent::DeviceCode(Err(error)) => {
                self.auth_busy = false;
                self.auth_cancel = None;
                self.auth_status = error;
            }
            GuiEvent::SignedIn(Ok(session)) => {
                self.auth_busy = false;
                self.auth_cancel = None;
                self.auth_device = None;
                let name = session.profile.name.clone();
                let saved = session.token_saved;
                let persisted = self.adopt_microsoft_session(session, context);
                self.auth_status = match (saved, persisted) {
                    (true, Ok(())) => format!(
                        "Signed in as {name}. Added to Accounts and saved for the next start."
                    ),
                    (false, Ok(())) => format!(
                        "Signed in as {name}. Added to Accounts; this session lasts until Kaelis closes."
                    ),
                    (_, Err(error)) => format!(
                        "Signed in as {name} and added to Accounts, but the list could not be written: {error}"
                    ),
                };
            }
            GuiEvent::Restored(Ok(session)) => {
                self.auth_busy = false;
                let name = session.profile.name.clone();
                match self.adopt_microsoft_session(session, context) {
                    Ok(()) => self.auth_status = format!("Welcome back, {name}."),
                    Err(error) => {
                        self.auth_status = format!(
                            "Welcome back, {name}. The account list could not be written: {error}"
                        );
                    }
                }
            }
            GuiEvent::Restored(Err(error)) => {
                self.auth_busy = false;
                self.auth_status =
                    format!("Saved Microsoft session could not be restored: {error}");
            }
            GuiEvent::SignedIn(Err(error)) => {
                if !self.auth_busy && error == "device authorization was cancelled" {
                    return;
                }
                self.auth_busy = false;
                self.auth_cancel = None;
                self.auth_device = None;
                // A refusal has to be impossible to miss: the reason names the
                // exact step that stopped, so a child account or a profile with
                // no Java Edition is never mistaken for a broken button.
                let cancelled = error == "device authorization was cancelled";
                self.auth_status = error.clone();
                if !cancelled {
                    self.set_status(
                        StatusKind::Error,
                        format!("Microsoft sign-in failed: {error}"),
                    );
                }
            }
            GuiEvent::Avatar(Ok(image)) => {
                self.avatar = Some(context.load_texture(
                    "kaelis-profile-head",
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
            GuiEvent::Avatar(Err(_error)) => {
                self.avatar = None;
            }
            GuiEvent::AccountHead {
                key,
                result,
                failed,
            } => {
                self.account_head_pending.remove(&key);
                if failed {
                    self.account_head_retry
                        .insert(key, std::time::Instant::now());
                    // Wake up once the cooldown expires instead of polling.
                    context.request_repaint_after(std::time::Duration::from_secs(60));
                } else {
                    self.account_head_retry.remove(&key);
                    let texture = result.map(|image| {
                        context.load_texture(
                            format!("kaelis-account-head-{key}"),
                            image,
                            egui::TextureOptions::NEAREST,
                        )
                    });
                    self.account_heads.insert(key, texture);
                }
            }
            GuiEvent::OfflineAvatar {
                name,
                result,
                failed,
            } => {
                if self.offline_skin_pending.as_deref() == Some(name.as_str()) {
                    self.offline_skin_pending = None;
                }
                self.offline_skin_inflight.remove(&name);
                let active = self
                    .offline_account
                    .as_ref()
                    .is_some_and(|profile| profile.name.eq_ignore_ascii_case(&name))
                    && self.microsoft_session.is_none();
                // The account list draws the same face; reuse the decoded
                // pixels instead of requesting the name twice.
                let keys = self
                    .accounts
                    .iter()
                    .filter(|account| {
                        account.kind == AccountKind::Offline
                            && account.name.eq_ignore_ascii_case(&name)
                    })
                    .map(SavedAccount::key)
                    .collect::<Vec<_>>();
                if failed {
                    // Back off instead of re-asking every frame.
                    for key in keys {
                        self.account_head_retry
                            .insert(key, std::time::Instant::now());
                    }
                    context.request_repaint_after(std::time::Duration::from_secs(60));
                } else {
                    self.offline_skins.insert(name.clone(), result.clone());
                    for key in keys {
                        self.account_head_pending.remove(&key);
                        self.account_head_retry.remove(&key);
                        let texture = result.clone().map(|image| {
                            context.load_texture(
                                format!("kaelis-account-head-{key}"),
                                image,
                                egui::TextureOptions::NEAREST,
                            )
                        });
                        self.account_heads.insert(key, texture);
                    }
                }
                if active && !failed {
                    match result {
                        Some(image) => {
                            self.avatar = Some(context.load_texture(
                                "kaelis-profile-head",
                                image,
                                egui::TextureOptions::NEAREST,
                            ));
                        }
                        None => {
                            self.avatar = Some(context.load_texture(
                                "kaelis-profile-head",
                                steve_head(),
                                egui::TextureOptions::NEAREST,
                            ));
                        }
                    }
                }
                if self.offline_name.trim().eq_ignore_ascii_case(&name) {
                    // Force the small typing preview to pick up the result.
                    self.offline_typing_key.clear();
                    self.refresh_offline_typing_avatar(context);
                }
            }
            GuiEvent::Versions(Ok(manifest)) => {
                if self.new_instance_version.is_empty() {
                    self.new_instance_version = manifest.latest.release.clone();
                }
                self.versions = manifest.versions;
                if self.installing_job.is_none() && !self.repairing_mods {
                    self.set_status(StatusKind::Success, "Minecraft versions refreshed.");
                }
            }
            GuiEvent::LauncherRelease(release) => {
                self.launcher_release = release;
            }
            #[cfg(windows)]
            GuiEvent::UpdatePrepared(Ok(update)) => {
                self.install_prepared_update(update, context);
            }
            #[cfg(windows)]
            GuiEvent::UpdatePrepared(Err(error)) => {
                self.update_busy = false;
                self.set_status(
                    StatusKind::Error,
                    format!("Could not download the update: {error}"),
                );
            }
            GuiEvent::Versions(Err(error)) => {
                self.set_status(
                    StatusKind::Error,
                    format!("Could not load Minecraft versions: {error}"),
                );
            }
            GuiEvent::ModSearch {
                generation,
                query,
                results,
            } => {
                // A newer search may already be in flight; its results are the
                // only ones allowed to replace the visible list.
                if generation != self.mod_search_generation {
                    return;
                }
                self.search_busy = false;
                let received = results.items.len() as u32;
                let total = results.total;
                if results.offset == 0 {
                    self.mod_results = results;
                    self.mod_results_query = query;
                    self.mod_icons.clear();
                    self.preview_key = None;
                } else {
                    // A "load more" page appends. Deduplicate across page
                    // boundaries because provider ranking can shift between
                    // requests while the user browses.
                    self.mod_results.total = self.mod_results.total.max(total);
                    for item in results.items {
                        let duplicate = self.mod_results.items.iter().any(|existing| {
                            existing.provider == item.provider && existing.id == item.id
                        });
                        if !duplicate {
                            self.mod_results.items.push(item);
                        }
                    }
                    for failure in results.failures {
                        if !self
                            .mod_results
                            .failures
                            .iter()
                            .any(|existing| existing.provider == failure.provider)
                        {
                            self.mod_results.failures.push(failure);
                        }
                    }
                }
                let loaded = self.mod_results.items.len() as u32;
                self.mod_has_more = received > 0
                    && if total > 0 {
                        loaded < total
                    } else {
                        received >= MOD_SEARCH_LIMIT
                    };
                // Build the keys, project links, and download labels once per
                // search, so scrolling does not re-format them every frame.
                self.mod_rows = build_mod_rows(&self.mod_results.items, &self.mod_results_query);
                let installed_count = self
                    .mod_results
                    .items
                    .iter()
                    .zip(&self.mod_rows)
                    .filter(|(item, row)| {
                        if item.kind == ContentKind::Mod {
                            self.installed.as_ref().is_some_and(|scan| {
                                scan.match_file(&item.slug, &row.match_tokens).is_some()
                            })
                        } else {
                            self.content_library
                                .match_entry(item.kind, &item.slug, &row.match_tokens)
                                .is_some()
                        }
                    })
                    .count();
                let total = self.mod_results.items.len();
                let kind_label = self.content_kind.label().to_ascii_lowercase();
                let (kind, text) = if total == 0 && self.mod_results.failures.is_empty() {
                    (
                        StatusKind::Info,
                        if self.mod_results_query.is_empty() {
                            format!("No {kind_label} are available for this version.")
                        } else {
                            format!("No {kind_label} matched \"{}\".", self.mod_results_query)
                        },
                    )
                } else {
                    (
                        StatusKind::Success,
                        format!(
                            "{total} matches · {installed_count} already installed in this instance"
                        ),
                    )
                };
                // A background search must never overwrite an install or repair
                // activity line: that is the one status the user is waiting on.
                if self.installing_job.is_none() && !self.repairing_mods {
                    self.set_status(kind, text);
                }
                let generation = self.mod_icon_generation;
                let requests: Vec<(String, String)> = self
                    .mod_results
                    .items
                    .iter()
                    .zip(&self.mod_rows)
                    .filter_map(|(item, row)| {
                        item.icon_url
                            .as_ref()
                            .map(|url| (row.key.clone(), url.clone()))
                    })
                    .collect();
                for (key, url) in requests {
                    self.request_mod_icon(context, generation, key, url);
                }
            }
            GuiEvent::ModIcon {
                generation,
                key,
                result,
            } => {
                if generation != self.mod_icon_generation {
                    return;
                }
                match result {
                    Ok(image) => {
                        let texture = context.load_texture(
                            format!("mod-icon-{key}"),
                            image,
                            egui::TextureOptions::LINEAR,
                        );
                        self.mod_icons.insert(key, ModIconState::Ready(texture));
                    }
                    Err(_) => {
                        self.mod_icons.insert(key, ModIconState::Failed);
                    }
                }
            }
            GuiEvent::InstanceIcon { id, result } => match result {
                Ok(image) => {
                    let texture = context.load_texture(
                        format!("instance-icon-{id}"),
                        image,
                        egui::TextureOptions::LINEAR,
                    );
                    self.instance_icons.insert(id, ModIconState::Ready(texture));
                }
                Err(error) => {
                    self.instance_icons.insert(id, ModIconState::Failed);
                    self.set_status(
                        StatusKind::Warning,
                        format!("Could not use the instance icon: {error}"),
                    );
                }
            },
            GuiEvent::ModpackProgress { generation, detail } => {
                if generation == self.modpack_generation
                    && let Some(title) = self.modpack_installing.clone()
                {
                    self.modpack_detail = detail.clone();
                    self.set_status(StatusKind::Busy, format!("Importing {title} · {detail}"));
                }
            }
            GuiEvent::InstalledScan {
                id,
                generation,
                scan,
                library,
            } => {
                if generation != self.installed_scan_generation
                    || self.selected_instance.as_deref() != Some(id.as_str())
                {
                    return;
                }
                self.detected_loader = scan.detected_loader.map(str::to_owned);
                self.installed = Some(scan);
                self.content_library = library;
            }
            GuiEvent::InstallStopped => {
                if self.installing_job.is_none() {
                    self.pump_install_queue(context.clone());
                }
            }
            GuiEvent::ModpackStopped => self.reload_instance_list(context),
            GuiEvent::ModInstallProgress(detail) => {
                // A progress line can be queued just before Cancel aborts the
                // worker; ignore it rather than resurrecting the banner.
                if self.installing_job.is_none() {
                    return;
                }
                self.install_detail = Some(detail.clone());
                if let Some(job) = &self.installing_job {
                    self.set_status(
                        StatusKind::Busy,
                        format!("Installing {} · {detail}", job.item.title),
                    );
                }
            }
            GuiEvent::ManualInstalled(result) => self.finish_manual_install(result, None),
            GuiEvent::FilesAdded(result) => match result {
                Ok((report, summary)) => self.finish_manual_install(Ok(report), Some(summary)),
                Err(error) => self.finish_manual_install(Err(error), None),
            },
            GuiEvent::Portable(result) => {
                self.portable_busy = false;
                match result {
                    Ok((message, imported)) => {
                        if imported {
                            self.reload_instance_list(context);
                        }
                        self.set_status(StatusKind::Success, message);
                    }
                    Err(error) => self.set_status(StatusKind::Error, error),
                }
            }
            GuiEvent::ModInstalled(result) => {
                self.install_task = None;
                let Some(job) = self.installing_job.take() else {
                    // A late event from a job the user replaced; keep the UI
                    // consistent rather than resurrecting a stale row.
                    self.install_detail = None;
                    return;
                };
                self.install_detail = None;
                self.install_started_at = None;
                let state_key = install_state_key(&job.instance_id, &job.key);
                let on_target = self
                    .selected_instance()
                    .map(|instance| instance.id.as_str())
                    == Some(job.instance_id.as_str());
                match result {
                    Ok(report) => {
                        let summary = report.summary();
                        self.install_states.insert(
                            state_key,
                            ModInstallState::Done {
                                summary: summary.clone(),
                                warnings: report.warnings.len(),
                            },
                        );
                        if on_target {
                            self.refresh_installed();
                        }
                        let kind = if report.warnings.is_empty() {
                            StatusKind::Success
                        } else {
                            StatusKind::Warning
                        };
                        self.last_install = Some(report);
                        self.set_status(kind, format!("{summary} · {}", job.item.title));
                    }
                    Err(error) => {
                        self.install_states
                            .insert(state_key, ModInstallState::Failed(error.clone()));
                        if on_target {
                            self.refresh_installed();
                        }
                        self.last_install = None;
                        self.set_status(
                            StatusKind::Error,
                            format!("Could not install {}: {error}", job.item.title),
                        );
                    }
                }
                self.pump_install_queue(context.clone());
                // Installs are the moment duplicate ids and undeclared
                // requirements appear. Resolve duplicates first, then repair
                // the dependency graph, without ever stacking workers.
                if self.installing_job.is_none()
                    && self.install_queue.is_empty()
                    && !self.repairing_mods
                    && on_target
                {
                    if self.auto_resolve_duplicates
                        && self
                            .installed
                            .as_ref()
                            .is_some_and(|scan| !scan.conflicts.is_empty())
                    {
                        self.resolve_duplicate_mods(context.clone());
                    } else if self.install_dependencies
                        && self.auto_repair_before_launch
                        && self
                            .installed
                            .as_ref()
                            .is_some_and(|scan| !scan.missing.is_empty())
                    {
                        self.repair_mod_environment(context.clone());
                    }
                }
            }
            GuiEvent::Updates {
                instance_id,
                result,
            } => {
                self.updates_busy = false;
                match result {
                    Ok(report) => {
                        let summary = report.summary();
                        let count = report.updates.len();
                        let on_target = self
                            .selected_instance()
                            .map(|instance| instance.id.as_str())
                            == Some(instance_id.as_str());
                        self.update_report = Some((instance_id, report));
                        if on_target {
                            self.set_status(
                                if count == 0 {
                                    StatusKind::Success
                                } else {
                                    StatusKind::Info
                                },
                                summary,
                            );
                        }
                    }
                    Err(error) => {
                        self.update_report = None;
                        self.set_status(
                            StatusKind::Error,
                            format!("Could not check mod updates: {error}"),
                        );
                    }
                }
            }
            GuiEvent::ModUpdated {
                project_id,
                title,
                result,
            } => {
                self.updating_mods.remove(&project_id);
                match result {
                    Ok(name) => {
                        if let Some((_, report)) = &mut self.update_report {
                            report
                                .updates
                                .retain(|update| update.project_id != project_id);
                        }
                        self.refresh_installed();
                        self.set_status(StatusKind::Success, format!("Updated {title} to {name}."));
                    }
                    Err(error) => self.set_status(
                        StatusKind::Error,
                        format!("Could not update {title}: {error}"),
                    ),
                }
            }
            GuiEvent::EnvironmentRepaired(Ok(report)) => {
                self.repairing_mods = false;
                self.refresh_installed();
                let kind = if report.unresolved.is_empty() && report.warnings.is_empty() {
                    StatusKind::Success
                } else {
                    StatusKind::Warning
                };
                let summary = report.summary();
                self.set_status(kind, summary);
                if let Some((instance, identity)) = self.pending_launch.take() {
                    self.start_launch(instance, identity, context.clone());
                }
            }
            GuiEvent::EnvironmentRepaired(Err(error)) => {
                self.repairing_mods = false;
                self.refresh_installed();
                self.set_status(
                    StatusKind::Warning,
                    format!("Environment repair failed: {error}"),
                );
                // A broken repair must not strand a launch the user asked for.
                if let Some((instance, identity)) = self.pending_launch.take() {
                    self.start_launch(instance, identity, context.clone());
                }
            }
            GuiEvent::DuplicatesResolved(Ok(report)) => {
                self.refresh_installed();
                self.set_status(
                    if report.disabled.is_empty() {
                        StatusKind::Info
                    } else {
                        StatusKind::Success
                    },
                    if report.disabled.is_empty() && report.warnings.is_empty() {
                        "No duplicate mod ids need cleanup.".to_owned()
                    } else {
                        report.summary()
                    },
                );
                if self.installing_job.is_none()
                    && self.install_queue.is_empty()
                    && self.auto_repair_before_launch
                    && !self.repairing_mods
                    && self
                        .installed
                        .as_ref()
                        .is_some_and(|scan| !scan.missing.is_empty())
                {
                    self.repair_mod_environment(context.clone());
                }
            }
            GuiEvent::DuplicatesResolved(Err(error)) => self.set_status(
                StatusKind::Error,
                format!("Could not resolve duplicate mods: {error}"),
            ),
            GuiEvent::VanillaProgress {
                instance_id,
                progress,
            } => {
                if self.installing_instance.as_deref() == Some(instance_id.as_str()) {
                    self.install_progress = match progress {
                        InstallProgress::DownloadingLibraries { completed, total }
                        | InstallProgress::DownloadingAssets { completed, total } => {
                            Some((completed, total))
                        }
                        _ => None,
                    };
                    self.set_status(StatusKind::Busy, progress.message());
                }
            }
            GuiEvent::VanillaInstalled {
                instance_id,
                result,
            } => {
                if self.installing_instance.as_deref() == Some(instance_id.as_str()) {
                    self.installing_instance = None;
                    self.install_progress = None;
                    let succeeded = result.is_ok();
                    let (kind, text) = match result {
                        Ok(()) => (
                            StatusKind::Success,
                            format!(
                                "Vanilla Minecraft {} is installed and ready for Java.",
                                self.instances
                                    .iter()
                                    .find(|instance| instance.id == instance_id)
                                    .map(|instance| instance.mc_version.as_str())
                                    .unwrap_or("version")
                            ),
                        ),
                        Err(error) => (
                            StatusKind::Error,
                            format!(
                                "Vanilla install failed: {error}. Select Install / Repair to retry."
                            ),
                        ),
                    };
                    self.set_status(kind, text);
                    self.refresh_instance_cards();
                    if succeeded {
                        self.provision_loader(context.clone(), instance_id.clone());
                    }
                }
            }
            GuiEvent::RuntimeProgress {
                instance_id,
                progress,
            } => {
                if self.installing_runtime.as_deref() == Some(instance_id.as_str()) {
                    self.install_progress = match progress {
                        RuntimeInstallProgress::Downloading { completed, total } => {
                            Some((completed, total))
                        }
                        _ => None,
                    };
                    self.set_status(StatusKind::Busy, progress.message());
                }
            }
            GuiEvent::RuntimeInstalled {
                instance_id,
                result,
            } => {
                if self.installing_runtime.as_deref() == Some(instance_id.as_str()) {
                    self.installing_runtime = None;
                    self.install_progress = None;
                    let (kind, text) = match result {
                        Ok(path) => {
                            let save_result = self
                                .instances
                                .iter_mut()
                                .find(|instance| instance.id == instance_id)
                                .map(|instance| {
                                    instance.java_path = Some(path.clone());
                                    instance.save()
                                });
                            match save_result {
                                Some(Ok(_)) => (
                                    StatusKind::Success,
                                    format!("Java runtime installed at {}.", path.display()),
                                ),
                                Some(Err(error)) => (
                                    StatusKind::Warning,
                                    format!(
                                        "Java runtime installed, but instance settings could not be saved: {error}"
                                    ),
                                ),
                                None => (
                                    StatusKind::Warning,
                                    "Java runtime installed, but the instance is missing."
                                        .to_owned(),
                                ),
                            }
                        }
                        Err(error) => (
                            StatusKind::Error,
                            format!(
                                "Java runtime install failed: {error}. Select Install Java to retry."
                            ),
                        ),
                    };
                    self.set_status(kind, text);
                    self.refresh_instance_cards();
                }
            }
            GuiEvent::InstanceRepairProgress {
                instance_id,
                message,
                progress,
            } => {
                // Only the instance waiting on a repair owns the status line;
                // a late event from an erased instance is dropped.
                if self
                    .pending_launch
                    .as_ref()
                    .is_some_and(|(item, _)| item.id == instance_id)
                {
                    self.install_progress = progress;
                    self.set_status(StatusKind::Busy, message);
                }
            }
            GuiEvent::InstanceRepaired {
                instance_id,
                result,
            } => {
                self.install_progress = None;
                match result {
                    Ok(summary) => self.set_status(StatusKind::Success, summary),
                    Err(error) => self.set_status(
                        StatusKind::Error,
                        format!("Could not repair the instance: {error}"),
                    ),
                }
                let pending = self
                    .pending_launch
                    .take_if(|(item, _)| item.id == instance_id);
                if let Some((pending, identity)) = pending {
                    // Only a pass that actually filled every gap continues;
                    // anything else is reported instead of looping.
                    let readiness = kaelis_api::health::check(&pending);
                    if readiness.ready() {
                        self.continue_launch(pending, identity, context.clone());
                    } else {
                        self.set_status(
                            StatusKind::Warning,
                            format!("Still not launchable: {}", readiness.issues().join(", ")),
                        );
                    }
                }
            }
            GuiEvent::GameStarted(instance_id) => {
                self.running_instances.insert(instance_id.clone());
                let count = self.running_instances.len();
                let name = self
                    .instances
                    .iter()
                    .find(|instance| instance.id == instance_id)
                    .map(|instance| instance.name.clone())
                    .unwrap_or_else(|| instance_id.clone());
                let running = if count == 1 {
                    format!("{name} is running. Kaelis is sleeping in the tray.")
                } else {
                    format!("{name} is running ({count} instances running).")
                };
                self.set_status(StatusKind::Success, running);
                self.avatar = None;
                if let Some(sweeper) = self.sweeper.take() {
                    sweeper.abort();
                }
                if let Some(cancel) = self.auth_cancel.take() {
                    let _ = cancel.send(true);
                }
                // A second launch happens from a window the user restored to
                // the tray; let it stay visible rather than fighting them.
                if count == 1 {
                    self.hidden_for_game = true;
                    platform::hide_for_game(context);
                    // The launcher now has no visible frame: give its pages
                    // back while the game owns the screen. They fault back in
                    // on the next restore; the tray and the game are untouched.
                    platform::trim_working_set();
                }
            }
            GuiEvent::GameExited {
                instance_id,
                message,
            } => {
                self.running_instances.remove(&instance_id);
                self.stop_signals.remove(&instance_id);
                self.clean_finished_instance(&instance_id, context);
                self.set_status(
                    StatusKind::Info,
                    if self.running_instances.is_empty() {
                        message
                    } else {
                        format!("{message} {} still running.", self.running_instances.len())
                    },
                );
                if self.running_instances.is_empty() {
                    self.restart_sweeper();
                    self.restore_avatar(context);
                    // Only a window that hid itself for the game returns by
                    // itself. A launcher the user hid, or started at sign-in
                    // in the tray, stays hidden when the game ends.
                    if self.hidden_for_game {
                        self.hidden_for_game = false;
                        show_window(context);
                    }
                }
            }
            GuiEvent::GameFailed { instance_id, error } => {
                self.running_instances.remove(&instance_id);
                self.stop_signals.remove(&instance_id);
                self.clean_finished_instance(&instance_id, context);
                self.set_status(StatusKind::Error, error);
                if self.running_instances.is_empty() {
                    self.restart_sweeper();
                    self.restore_avatar(context);
                    if self.hidden_for_game {
                        self.hidden_for_game = false;
                        show_window(context);
                    }
                }
            }
            GuiEvent::InstanceDuplicated(result) => match *result {
                Ok((duplicate, summary)) => {
                    if let Some(path) = duplicate.icon.clone() {
                        self.request_instance_icon(context, duplicate.id.clone(), path);
                    }
                    self.instances.push(duplicate.clone());
                    self.instances.sort_by(|left, right| {
                        right
                            .last_played_unix
                            .cmp(&left.last_played_unix)
                            .then_with(|| left.name.cmp(&right.name))
                    });
                    self.refresh_instance_cards();
                    self.select_instance(&duplicate.id);
                    self.set_status(StatusKind::Success, summary);
                }
                Err(error) => {
                    self.set_status(StatusKind::Error, format!("Could not duplicate: {error}"))
                }
            },
            GuiEvent::Notice(message) => self.set_status(StatusKind::Info, message),
            GuiEvent::AccountsImported(result) => {
                let (kind, text) = match result {
                    Ok(scan) => {
                        let files = scan.files.len();
                        let (added, refreshed) = self.merge_imported_accounts(scan.accounts);
                        let text = match (added, refreshed) {
                            (0, 0) => "No new accounts found.".to_owned(),
                            (added, 0) => {
                                format!("Imported {added} account(s) from {files} file(s).")
                            }
                            (0, refreshed) => {
                                format!("Refreshed {refreshed} saved account(s).")
                            }
                            (added, refreshed) => format!(
                                "Imported {added} account(s) and refreshed {refreshed} from {files} file(s)."
                            ),
                        };
                        (StatusKind::Success, text)
                    }
                    Err(error) => (
                        StatusKind::Warning,
                        format!("No accounts imported: {error}"),
                    ),
                };
                self.auth_status = text.clone();
                self.set_status(kind, text);
                context.request_repaint();
            }
            GuiEvent::ArtworkPicked(path) => match std::fs::read(&path) {
                Ok(bytes) => self.adopt_artwork_bytes(&bytes, context),
                Err(error) => self.set_status(
                    StatusKind::Error,
                    format!("Could not read the image: {error}"),
                ),
            },
            GuiEvent::InstanceArtworkPicked { id, path } => match std::fs::read(&path) {
                Ok(bytes) => {
                    self.select_instance(&id);
                    self.apply_instance_artwork(&id, &bytes, context);
                }
                Err(error) => self.set_status(
                    StatusKind::Error,
                    format!("Could not read the image: {error}"),
                ),
            },
            GuiEvent::Automation(run) => {
                // Keep the last few results in the panel: a rule that fails
                // every launch must stay visible after the status bar moves on.
                let detail = run
                    .lines
                    .last()
                    .filter(|_| !run.ok)
                    .map(|line| format!("{}: {line}", run.summary))
                    .unwrap_or_else(|| run.summary.clone());
                self.automation_log.push(detail);
                if self.automation_log.len() > 5 {
                    self.automation_log.remove(0);
                }
                self.set_status(
                    if run.ok {
                        StatusKind::Success
                    } else {
                        StatusKind::Error
                    },
                    run.summary,
                );
            }
            GuiEvent::Handoff(arguments) => self.apply_handoff(context, &arguments),
            GuiEvent::Tray(action) => match action {
                tray::TrayAction::Show => {
                    self.hidden_for_game = false;
                    show_window(context);
                }
                tray::TrayAction::Hide => {
                    // A deliberate hide is not a game hand-off, so a later
                    // game exit must not pop the window back up.
                    self.hidden_for_game = false;
                    platform::hide_to_tray(context);
                }
                tray::TrayAction::LaunchLast => self.launch_selected(context.clone()),
                tray::TrayAction::Quit => self.quit_now(),
            },
        }
    }

    fn apply_handoff(&mut self, context: &egui::Context, arguments: &[String]) {
        match parse_handoff(arguments) {
            HandoffRequest::Activate => {
                self.hidden_for_game = false;
                self.set_status(
                    StatusKind::Info,
                    "Kaelis is already running; window restored.",
                );
            }
            HandoffRequest::Play {
                instance_id,
                offline_name,
            } => {
                let Some(instance) = self
                    .instances
                    .iter()
                    .find(|instance| instance.id == instance_id)
                    .cloned()
                else {
                    self.set_status(
                        StatusKind::Error,
                        format!("Another launch requested unknown instance {instance_id}."),
                    );
                    return;
                };
                self.select_instance(&instance_id);
                self.current_tab = Tab::Instances;
                if self.instance_running(&instance_id) {
                    self.set_status(
                        StatusKind::Warning,
                        format!("{} is already running.", instance.name),
                    );
                    return;
                }
                if let Some(name) = offline_name {
                    // A forwarded `--offline` is an explicit identity choice,
                    // so it overrides a signed-in Microsoft session for this
                    // launch without discarding that session.
                    let profile = offline_profile(&name);
                    let identity =
                        LaunchIdentity::offline(profile.name.clone(), profile.uuid.clone());
                    self.offline_account = Some(profile.clone());
                    self.apply_offline_head(context, &profile.name);
                    self.request_offline_skin(context, &profile.name);
                    self.auth_status = format!("Offline profile ready: {}", profile.name);
                    // A forwarded launch gets the same repair gate as the Play
                    // button: the instance is made whole before Java starts.
                    self.continue_launch(instance, identity, context.clone());
                    return;
                }
                self.launch_selected(context.clone());
            }
            HandoffRequest::Unsupported => {
                self.set_status(
                    StatusKind::Info,
                    "Another Kaelis launch was forwarded; no launcher action was needed.",
                );
            }
        }
    }

    fn restart_sweeper(&mut self) {
        if self.sweeper.is_none() {
            // 15-minute ticks over an hour of grace: the sweep almost always
            // has nothing to remove, so it should not wake more often than the
            // lifetime of the files it collects.
            self.sweeper = Some(kaelis_core::sweeper::spawn_background_sweeper(
                &self.runtime,
                900,
                3600,
            ));
        }
    }

    fn glass_frame() -> egui::Frame {
        egui::Frame::none()
            .fill(SURFACE)
            .stroke(egui::Stroke::new(1.0_f32, BORDER))
            .rounding(egui::Rounding::same(CORNER_LG))
            .inner_margin(egui::Margin::symmetric(12.0, 9.0))
    }

    /// Tighter sibling of [`Self::glass_frame`] for repeated list rows.
    fn card_frame() -> egui::Frame {
        egui::Frame::none()
            .fill(SURFACE_ALT)
            .stroke(egui::Stroke::new(1.0_f32, BORDER))
            .rounding(egui::Rounding::same(CORNER_MD))
            .inner_margin(egui::Margin::symmetric(10.0, 7.0))
    }

    /// Search-result rows: the card surface with tighter gutters, so more
    /// results fit on one screen without changing the visual language.
    fn list_frame() -> egui::Frame {
        egui::Frame::none()
            .fill(SURFACE_ALT)
            .stroke(egui::Stroke::new(1.0_f32, BORDER))
            .rounding(egui::Rounding::same(CORNER_MD))
            .inner_margin(egui::Margin::symmetric(9.0, 5.0))
    }

    fn paint_backdrop(context: &egui::Context) {
        let rect = context.screen_rect();
        let painter = context.layer_painter(egui::LayerId::background());
        let mut mesh = egui::Mesh::default();
        // Two corners are pure black: on an OLED panel those pixels switch
        // off entirely instead of glowing a faint grey in a dark room.
        mesh.colored_vertex(rect.left_top(), egui::Color32::from_rgb(4, 5, 7));
        mesh.colored_vertex(rect.right_top(), egui::Color32::from_rgb(6, 8, 11));
        mesh.colored_vertex(rect.right_bottom(), egui::Color32::BLACK);
        mesh.colored_vertex(rect.left_bottom(), egui::Color32::BLACK);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(mesh));
    }

    fn show_sidebar(&mut self, context: &egui::Context) {
        // Development-only fixture: focus injection from a capture run is not
        // reliable on every host, so the first row can ask for keyboard focus
        // itself and still paint through the real focus-ring path.
        let focus_first =
            self.screenshot_path.is_some() && std::env::var_os("KAELIS_SCREENSHOT_FOCUS").is_some();
        // The rail is a layout decision, not a font-size one: below the
        // breakpoint labels move into tooltips so the workspace keeps its
        // width instead of squeezing into a strip.
        let rail = context.screen_rect().width() < RAIL_WIDTH;
        let width = if rail {
            RAIL_SIDEBAR_WIDTH
        } else {
            SIDEBAR_WIDTH
        };
        let margin = if rail { 9.0 } else { 12.0 };
        egui::SidePanel::left("kaelis-navigation")
            .resizable(false)
            .exact_width(width)
            .frame(
                egui::Frame::none()
                    .fill(SURFACE)
                    .inner_margin(egui::Margin::symmetric(margin, 12.0)),
            )
            .show(context, |ui| {
                if rail {
                    ui.vertical_centered(|ui| {
                        draw_brand_mark(ui, 24.0);
                    });
                } else {
                    ui.horizontal(|ui| {
                        draw_brand_mark(ui, 24.0);
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("KAELIS").strong().size(13.0));
                    });
                }
                // Hairline under the brand block: one rule, no card, so the
                // rail reads as chrome rather than a stack of panels.
                ui.add_space(10.0);
                let rule_y = ui.cursor().min.y;
                ui.painter().hline(
                    ui.max_rect().x_range(),
                    rule_y,
                    egui::Stroke::new(1.0_f32, BORDER),
                );
                ui.add_space(9.0);
                for (index, tab) in Tab::ALL.into_iter().enumerate() {
                    let selected = self.current_tab == tab;
                    // Painted by hand: a capsule row with a short hover fade,
                    // so selection glides in instead of snapping.
                    let (rect, response) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), 27.0),
                        egui::Sense::click(),
                    );
                    // The tooltip is the label: in rail mode the row is an
                    // icon only, and no chord is advertised anywhere now.
                    let _ = index;
                    let response = response
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(tab.title());
                    response.widget_info(|| {
                        // A selectable, not a plain button: screen readers get
                        // the active tab as state instead of only a label.
                        egui::WidgetInfo::selected(
                            egui::WidgetType::SelectableLabel,
                            true,
                            selected,
                            tab.title(),
                        )
                    });
                    if focus_first && index == 0 {
                        ui.memory_mut(|memory| memory.request_focus(response.id));
                        self.keyboard_nav = true;
                    }
                    let hover = ui.ctx().animate_bool_with_time(
                        response.id.with("hover"),
                        response.hovered(),
                        0.12,
                    );
                    let fill_alpha = if selected { 10 } else { (hover * 8.0) as u8 };
                    if fill_alpha > 0 {
                        ui.painter().rect_filled(
                            rect,
                            egui::Rounding::same(CORNER_MD),
                            egui::Color32::from_rgba_unmultiplied(255, 255, 255, fill_alpha),
                        );
                    }
                    if selected {
                        // A 2 px accent rail anchors the active row without
                        // boxing it in; the fill alone is too quiet to find.
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                rect.left_top() + egui::vec2(0.0, 7.0),
                                egui::vec2(2.0, rect.height() - 14.0),
                            ),
                            egui::Rounding::same(1.0),
                            accent(),
                        );
                    }
                    if !rail && ui.is_rect_visible(rect) {
                        ui.painter().text(
                            rect.left_center() + egui::vec2(32.0, 0.0),
                            egui::Align2::LEFT_CENTER,
                            tab.title(),
                            egui::FontId::proportional(11.0),
                            if selected { TEXT } else { MUTED },
                        );
                    }
                    let icon_at = if rail {
                        rect.center()
                    } else {
                        rect.left_center() + egui::vec2(15.0, 0.0)
                    };
                    paint_tab_icon(
                        ui.painter(),
                        icon_at,
                        tab,
                        if selected { accent() } else { MUTED },
                    );
                    if self.keyboard_nav && response.has_focus() {
                        ui.painter().rect_stroke(
                            rect.expand(0.5),
                            egui::Rounding::same(CORNER_MD),
                            egui::Stroke::new(1.4_f32, accent_soft()),
                        );
                    }
                    if response.clicked() {
                        self.current_tab = tab;
                    }
                    ui.add_space(1.0);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    // The identity block is chosen from what the tab list left
                    // behind, not from a window height: a short window then
                    // keeps every tab reachable instead of letting the card
                    // paint over Settings. The rail always takes the compact
                    // form because its labels do not fit at all.
                    const FULL_IDENTITY_HEIGHT: f32 = 88.0;
                    let room = ui.available_height();
                    if room < 30.0 {
                        // Not even one button fits; the Account tab above is
                        // the same destination.
                    } else if rail || room < FULL_IDENTITY_HEIGHT {
                        // One 30 px identity button: the same click target as
                        // the full card, without a label that cannot fit.
                        let (rect, response) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 32.0),
                            egui::Sense::click(),
                        );
                        if let Some(avatar) = &self.avatar {
                            ui.painter().image(
                                avatar.id(),
                                rect.shrink(1.0),
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                egui::Color32::WHITE,
                            );
                        } else {
                            ui.painter().circle_filled(rect.center(), 13.0, GLASS_LIGHT);
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                self.account_initial(),
                                egui::FontId::proportional(13.0),
                                accent(),
                            );
                        }
                        let response = response
                            .on_hover_text(format!(
                                "{} · {}",
                                self.account_name(),
                                self.account_kind()
                            ))
                            .on_hover_cursor(egui::CursorIcon::PointingHand);
                        if response.clicked() {
                            self.current_tab = Tab::Account;
                        }
                        ui.add_space(6.0);
                    } else {
                        ui.label(
                            egui::RichText::new(format!("{} · Rust", kaelis_core::brand::COMPANY))
                                .size(8.5)
                                .color(MUTED),
                        );
                        ui.horizontal(|ui| {
                            ui.hyperlink_to(
                                egui::RichText::new("Community ↗").size(9.0).color(accent()),
                                kaelis_core::brand::DISCORD_HUB,
                            );
                            ui.hyperlink_to(
                                egui::RichText::new("GitHub ↗").size(9.0).color(MUTED),
                                kaelis_core::brand::GITHUB_REPOSITORY,
                            );
                        });
                        ui.add_space(7.0);
                        let card = Self::glass_frame().show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                if let Some(avatar) = &self.avatar {
                                    ui.add(
                                        egui::Image::new((avatar.id(), egui::vec2(30.0, 30.0)))
                                            .rounding(egui::Rounding::same(CORNER_SM)),
                                    );
                                } else {
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(30.0, 30.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().circle_filled(rect.center(), 14.0, GLASS_LIGHT);
                                    ui.painter().text(
                                        rect.center(),
                                        egui::Align2::CENTER_CENTER,
                                        self.account_initial(),
                                        egui::FontId::proportional(13.5),
                                        accent(),
                                    );
                                }
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 1.0;
                                    ui.label(
                                        egui::RichText::new(self.account_name())
                                            .strong()
                                            .size(12.0),
                                    );
                                    ui.label(
                                        egui::RichText::new(self.account_kind())
                                            .size(9.0)
                                            .color(MUTED),
                                    );
                                });
                            });
                        });
                        // The card itself is the target: one clickable surface
                        // with no half-clipped button hanging off its edge.
                        if card
                            .response
                            .interact(egui::Sense::click())
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .on_hover_text("Open the account manager")
                            .clicked()
                        {
                            self.current_tab = Tab::Account;
                        }
                        ui.add_space(6.0);
                    }
                });
            });
    }

    fn account_name(&self) -> String {
        self.microsoft_session
            .as_ref()
            .map(|session| session.profile.name.clone())
            .or_else(|| {
                self.offline_account
                    .as_ref()
                    .map(|profile| profile.name.clone())
            })
            .unwrap_or_else(|| "No account".to_owned())
    }

    /// Short identity line for the sidebar card. The saved count makes a
    /// multi-account setup visible without opening the tab.
    /// One letter for the avatar disc. Signed out there is no identity to
    /// initial, and the first letter of "No account" read as an N, which looks
    /// like a real profile; the brand mark is the honest placeholder.
    fn account_initial(&self) -> char {
        let signed_in = self.microsoft_session.is_some() || self.offline_account.is_some();
        if signed_in {
            self.account_name().chars().next().unwrap_or('K')
        } else {
            'K'
        }
    }

    fn account_kind(&self) -> String {
        let base = if self.microsoft_session.is_some() {
            "Microsoft · online"
        } else if self.offline_account.is_some() {
            "Offline · local"
        } else {
            "Not signed in"
        };
        match self.accounts.len() {
            0 | 1 => base.to_owned(),
            count => format!("{base} · {count} saved"),
        }
    }

    fn set_status(&mut self, kind: StatusKind, text: impl Into<String>) {
        self.status_kind = kind;
        self.status = text.into();
    }

    /// Re-read the selected instance's jars and content folders on a worker.
    /// The scan opens every jar, so doing it inline froze the window on large
    /// packs; the generation drops a result the user has already navigated
    /// past, and clearing the old scan prevents another instance's mods from
    /// flashing on the new card.
    fn refresh_installed(&mut self) {
        self.confirm_mod_delete = None;
        self.installed_scan_generation = self.installed_scan_generation.wrapping_add(1);
        let generation = self.installed_scan_generation;
        let Some(instance) = self.selected_instance().cloned() else {
            self.installed = None;
            self.content_library = ContentLibrary::default();
            self.detected_loader = None;
            return;
        };
        self.installed = None;
        self.content_library = ContentLibrary::default();
        self.detected_loader = None;
        let id = instance.id.clone();
        let sender = self.sender.clone();
        let repaint = self.egui_context.clone();
        self.runtime.spawn_blocking(move || {
            let scan = scan_installed(&instance);
            let library = ContentLibrary::scan(&instance);
            send_event(
                &sender,
                &repaint,
                GuiEvent::InstalledScan {
                    id,
                    generation,
                    scan,
                    library,
                },
            );
        });
    }

    /// Iris (Fabric/NeoForge) and Oculus (Forge) are the shader loaders; a pack
    /// installed without one is silently ignored by Minecraft.
    fn has_shader_loader(&self) -> bool {
        self.installed.as_ref().is_some_and(|scan| {
            scan.files.iter().any(|file| {
                !file.disabled
                    && file
                        .ids
                        .iter()
                        .any(|id| id.contains("iris") || id.contains("oculus"))
            })
        })
    }

    /// Compare the selected instance's jars with Modrinth, once per click.
    fn start_update_check(&mut self, context: egui::Context) {
        if self.updates_busy {
            return;
        }
        let Some(instance) = self.selected_instance().cloned() else {
            self.set_status(
                StatusKind::Warning,
                "Select an instance before checking for mod updates.",
            );
            return;
        };
        if instance.loader.eq_ignore_ascii_case("vanilla") {
            self.set_status(StatusKind::Warning, "This instance has no mod loader.");
            return;
        }
        let mut instance = instance;
        if let Some(loader) = self.detected_loader.clone() {
            instance.loader = loader;
        }
        let instance_id = instance.id.clone();
        self.updates_busy = true;
        self.update_report = None;
        self.set_status(StatusKind::Busy, "Checking installed mods for updates…");
        let http = self.http.clone();
        let sender = self.sender.clone();
        let repaint = context;
        self.runtime.spawn(async move {
            let result = check_updates(&http, &instance)
                .await
                .map_err(|error| error.to_string());
            send_event(
                &sender,
                &repaint,
                GuiEvent::Updates {
                    instance_id,
                    result,
                },
            );
        });
    }

    /// Replace one installed jar with the newest compatible version.
    fn apply_mod_update(&mut self, update: ModUpdate, context: egui::Context) {
        if self.updating_mods.contains(&update.project_id) {
            return;
        }
        let Some(instance) = self.selected_instance().cloned() else {
            return;
        };
        let mods_dir = instance.root.join("mods");
        let project_id = update.project_id.clone();
        let title = update.title.clone();
        self.updating_mods.insert(project_id.clone());
        self.set_status(StatusKind::Busy, format!("Updating {}…", update.title));
        let http = self.http.clone();
        let sender = self.sender.clone();
        let repaint = context;
        self.runtime.spawn(async move {
            let result = apply_update(&http, &mods_dir, &update)
                .await
                .map(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default()
                })
                .map_err(|error| error.to_string());
            send_event(
                &sender,
                &repaint,
                GuiEvent::ModUpdated {
                    project_id,
                    title,
                    result,
                },
            );
        });
    }

    /// Install every missing required mod for the selected instance.
    fn repair_mod_environment(&mut self, context: egui::Context) {
        if self.repairing_mods || self.installing_job.is_some() {
            self.set_status(
                StatusKind::Warning,
                "Wait for the current mod operation to finish.",
            );
            return;
        }
        let Some(instance) = self.selected_instance().cloned() else {
            self.set_status(
                StatusKind::Warning,
                "Select an instance before repairing its mods.",
            );
            return;
        };
        self.repairing_mods = true;
        let loader = self
            .detected_loader
            .clone()
            .or_else(|| {
                (!instance.loader.eq_ignore_ascii_case("vanilla")).then(|| instance.loader.clone())
            })
            .unwrap_or_else(|| self.mod_loader.clone());
        let platforms = self.platforms.clone();
        let game_version = instance.mc_version.clone();
        let mods_dir = instance.root.join("mods");
        let sender = self.sender.clone();
        let repaint = context;
        self.set_status(
            StatusKind::Busy,
            "Repairing the mod environment and resolving required dependencies…",
        );
        self.runtime.spawn(async move {
            let result = repair_environment(&platforms, &game_version, &loader, &mods_dir).await;
            send_event(&sender, &repaint, GuiEvent::EnvironmentRepaired(Ok(result)));
        });
    }

    /// Disable older jars that duplicate an id a newer jar already provides.
    fn resolve_duplicate_mods(&mut self, context: egui::Context) {
        let Some(instance) = self.selected_instance().cloned() else {
            self.set_status(
                StatusKind::Warning,
                "Select an instance before resolving duplicates.",
            );
            return;
        };
        let mods_dir = instance.root.join("mods");
        let sender = self.sender.clone();
        let repaint = context;
        self.set_status(StatusKind::Busy, "Checking for duplicate mod ids…");
        self.runtime.spawn_blocking(move || {
            let result = disable_duplicate_mods(&mods_dir).map_err(|error| error.to_string());
            send_event(&sender, &repaint, GuiEvent::DuplicatesResolved(result));
        });
    }

    fn apply_mod_file_action(&mut self, action: ModFileAction, path: &std::path::Path) {
        let result: Result<String, String> = match action {
            ModFileAction::Toggle { disabled } => toggle_mod_file(path, disabled).map(|_| {
                format!(
                    "Content is now {}.",
                    if disabled { "enabled" } else { "disabled" }
                )
            }),
            ModFileAction::Delete => std::fs::remove_file(path)
                .map(|()| "Content deleted.".to_owned())
                .map_err(|error| error.to_string()),
        };
        match result {
            Ok(message) => self.set_status(StatusKind::Success, message),
            Err(error) => self.set_status(
                StatusKind::Error,
                format!("Could not change installed content: {error}"),
            ),
        }
        self.confirm_mod_delete = None;
        self.refresh_installed();
    }

    /// Remove one project from the selected instance in a single click. Only
    /// files the current scan associates with the project are touched, and a
    /// running game is refused because Windows keeps open jars locked.
    fn erase_installed_content(&mut self, target: &EraseTarget) {
        let Some(instance) = self.selected_instance().cloned() else {
            self.set_status(
                StatusKind::Warning,
                "Select an instance before erasing content.",
            );
            return;
        };
        if self.instance_running(&instance.id) {
            self.set_status(
                StatusKind::Warning,
                format!("Close {} before erasing content.", instance.name),
            );
            return;
        }
        let paths: Vec<PathBuf> = match target.kind {
            ContentKind::Mod => self
                .installed
                .as_ref()
                .map(|scan| {
                    scan.files_matching(&target.slug, &target.tokens)
                        .into_iter()
                        .map(|file| file.path.clone())
                        .collect()
                })
                .unwrap_or_default(),
            kind => self
                .content_library
                .match_entry(kind, &target.slug, &target.tokens)
                .map(|entry| vec![entry.path.clone()])
                .unwrap_or_default(),
        };
        if paths.is_empty() {
            self.set_status(
                StatusKind::Warning,
                format!("{} is no longer installed in this instance.", target.title),
            );
            self.refresh_installed();
            return;
        }
        let mut removed = 0_usize;
        let mut bytes = 0_u64;
        let mut failure = None;
        for path in &paths {
            let size = std::fs::metadata(path).map_or(0, |metadata| metadata.len());
            let result = if path.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
            match result {
                Ok(()) => {
                    removed += 1;
                    bytes = bytes.saturating_add(size);
                }
                Err(error) if failure.is_none() => failure = Some(error.to_string()),
                Err(_) => {}
            }
        }
        self.install_states
            .remove(&install_state_key(&instance.id, &target.key));
        self.refresh_installed();
        match failure {
            None => self.set_status(
                StatusKind::Success,
                format!(
                    "Erased {}. {removed} file(s), {} removed.",
                    target.title,
                    format_bytes(bytes)
                ),
            ),
            Some(error) if removed == 0 => self.set_status(
                StatusKind::Error,
                format!("Could not erase {}: {error}", target.title),
            ),
            Some(error) => self.set_status(
                StatusKind::Warning,
                format!(
                    "Erased {removed} file(s) of {}. One file failed: {error}",
                    target.title
                ),
            ),
        }
    }

    fn open_mods_folder(&mut self) {
        let Some(instance) = self.selected_instance() else {
            self.set_status(StatusKind::Warning, "Select an instance first.");
            return;
        };
        let mods = instance.root.join("mods");
        let result = std::fs::create_dir_all(&mods)
            .map_err(|error| error.to_string())
            .and_then(|()| open_instance_path(&mods, false));
        if let Err(error) = result {
            self.set_status(
                StatusKind::Error,
                format!("Could not open the mods folder: {error}"),
            );
        }
    }

    /// Copy an instance on a worker thread: a large modpack can be several
    /// gigabytes, so the window never waits on the disk. The core crate picks
    /// the new id and name, skips runtime output, and refuses links.
    fn duplicate_instance(
        &mut self,
        instance: Instance,
        with_saves: bool,
        context: &egui::Context,
    ) {
        self.set_status(StatusKind::Busy, format!("Copying {}…", instance.name));
        let sender = self.sender.clone();
        let repaint = context.clone();
        self.runtime.spawn_blocking(move || {
            let options = DuplicateOptions {
                content: true,
                saves: with_saves,
                settings: true,
            };
            let result = instance
                .duplicate(options)
                .map(|report| {
                    let summary = report.summary();
                    (report.instance, summary)
                })
                .map_err(|error| error.to_string());
            send_event(
                &sender,
                &repaint,
                GuiEvent::InstanceDuplicated(Box::new(result)),
            );
        });
    }

    /// Wipe one instance completely in a single step: its whole directory tree,
    /// its JSON record, any temp record from an interrupted save, install jobs
    /// still queued for it, and every cached view of it.
    ///
    /// Files that the game or an installer is currently holding open cannot be
    /// deleted on Windows, so those two cases are refused up front instead of
    /// leaving a half-erased instance behind.
    fn erase_instance(&mut self, id: &str) {
        let Some(instance) = self
            .instances
            .iter()
            .find(|instance| instance.id == id)
            .cloned()
        else {
            return;
        };
        if self.instance_running(&instance.id) {
            self.set_status(
                StatusKind::Warning,
                format!("Close {} before erasing it.", instance.name),
            );
            return;
        }
        let install_busy = self
            .installing_job
            .as_ref()
            .is_some_and(|job| job.instance_id == id)
            || self.installing_instance.as_deref() == Some(id)
            || self.installing_runtime.as_deref() == Some(id);
        if install_busy {
            self.set_status(
                StatusKind::Warning,
                format!(
                    "Wait for the current install to finish before erasing {}.",
                    instance.name
                ),
            );
            return;
        }
        match instance.erase() {
            Ok(report) => {
                // Jobs queued for this instance would recreate folders and then
                // fail against a missing record; drop them with the records.
                self.install_queue.retain(|job| job.instance_id != id);
                let state_prefix = format!("{id}\u{1f}");
                self.install_states
                    .retain(|key, _| !key.starts_with(&state_prefix));
                if self
                    .pending_launch
                    .as_ref()
                    .is_some_and(|(item, _)| item.id == id)
                {
                    self.pending_launch = None;
                }
                self.instances.retain(|item| item.id != id);
                self.instance_cards.remove(id);
                self.instance_icons.remove(id);
                if self.editing.as_ref().is_some_and(|edit| edit.id == id) {
                    self.close_editor();
                }
                if self.selected_instance.as_deref() == Some(id) {
                    self.selected_instance = None;
                    match self.instances.first().map(|item| item.id.clone()) {
                        Some(next) => self.select_instance(&next),
                        None => {
                            self.reset_browse();
                            self.refresh_installed();
                        }
                    }
                } else {
                    self.refresh_installed();
                }
                self.refresh_instance_cards();
                let mut detail = format!(
                    "{} files, {} removed",
                    report.files,
                    format_bytes(report.bytes)
                );
                if !report.metadata {
                    detail.push_str(" · no metadata record remained");
                }
                self.set_status(
                    StatusKind::Success,
                    format!("Erased {}. {detail}.", instance.name),
                );
            }
            Err(error) => {
                self.refresh_instance_cards();
                self.set_status(
                    StatusKind::Error,
                    format!("Could not erase {}: {error}", instance.name),
                );
            }
        }
    }

    fn refresh_instance_cards(&mut self) {
        self.instance_cards.clear();
        self.instance_cards
            .extend(self.instances.iter().map(|instance| {
                (
                    instance.id.clone(),
                    InstanceCard {
                        detail: format!(
                            "Minecraft {} · {} · {} MiB",
                            instance.mc_version, instance.loader, instance.ram_mb
                        ),
                        root: instance.root.display().to_string(),
                        content: instance_content_summary(&instance.root),
                        last_played: relative_last_played(instance.last_played_unix),
                        notes: instance.notes.trim().to_owned(),
                        notes_preview: note_preview(&instance.notes),
                        health: InstanceHealth::check(instance),
                    },
                )
            }));
    }

    fn select_instance(&mut self, id: &str) {
        let changed = self.selected_instance.as_deref() != Some(id);
        self.selected_instance = Some(id.to_owned());
        if let Some(instance) = self.instances.iter().find(|instance| instance.id == id) {
            self.settings_ram_mb = instance.ram_mb;
            self.settings_jvm_args = instance.jvm_args.join("\n");
        }
        if changed {
            // Old results belong to another version/loader. Clearing them makes
            // the automatic search visibly reload rather than briefly showing
            // incompatible cards against the new target.
            self.reset_browse();
        }
        self.refresh_installed();
        // Keep the search filter aligned with the jars actually installed so a
        // stale instance label cannot hide compatible results.
        if let Some(loader) = self.detected_loader.clone() {
            self.mod_loader = loader;
        } else if let Some(instance) = self.instances.iter().find(|instance| instance.id == id)
            && !instance.loader.eq_ignore_ascii_case("vanilla")
        {
            self.mod_loader = instance.loader.clone();
        }
    }

    fn is_busy(&self) -> bool {
        self.search_busy
            || self.auth_busy
            || self.update_busy
            || self.installing_job.is_some()
            || self.installing_instance.is_some()
            || self.installing_runtime.is_some()
            || self.repairing_mods
            || self.portable_busy
    }

    fn fraction(progress: Option<(usize, usize)>) -> Option<f32> {
        progress
            .filter(|(_, total)| *total > 0)
            .map(|(completed, total)| (completed as f32 / total as f32).clamp(0.0, 1.0))
    }

    fn progress_fraction(&self) -> Option<f32> {
        Self::fraction(self.install_progress)
    }

    fn show_header(&mut self, context: &egui::Context) {
        egui::TopBottomPanel::top("kaelis-header")
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgba_premultiplied(7, 8, 11, 248))
                    // A 4 px strip above and below the title: the bar is a
                    // handle and a label, not a third content row.
                    .inner_margin(egui::Margin::symmetric(12.0, 4.0)),
            )
            .show(context, |ui| {
                // The window is an overlay with no native title bar, so this
                // strip is its handle: drag to move, double-click to maximise
                // or restore. Registered before the widgets so the buttons on
                // top keep their own clicks.
                let strip = ui.interact(
                    ui.max_rect(),
                    ui.id().with("kaelis-window-drag"),
                    egui::Sense::click_and_drag(),
                );
                if strip.drag_started() {
                    context.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
                if strip.double_clicked() {
                    let maximized = context
                        .input(|input| input.viewport().maximized)
                        .unwrap_or(false);
                    context.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                }
                ui.horizontal(|ui| {
                    // The bar is a drag handle and three window controls. The
                    // selected channel is already obvious from the rail and
                    // the page below it, so it is not repeated here.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if window_control(ui, WindowControl::Close, "Close").clicked() {
                            self.quit_now();
                        }
                        let maximized = context
                            .input(|input| input.viewport().maximized)
                            .unwrap_or(false);
                        let (control, tooltip) = if maximized {
                            (WindowControl::Restore, "Restore")
                        } else {
                            (WindowControl::Maximize, "Maximize")
                        };
                        if window_control(ui, control, tooltip).clicked() {
                            context.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
                        }
                        if window_control(ui, WindowControl::Minimize, "Minimize").clicked() {
                            platform::minimize_window(context);
                        }
                        // No status pill anywhere in the bar: a running game is
                        // already on its card and on Overview, and the strip
                        // stays a handle plus three window controls.
                        if self.is_busy() {
                            ui.add(egui::Spinner::new().size(12.0));
                        }
                    });
                });
                if let Some(fraction) = self.progress_fraction() {
                    ui.add_space(3.0);
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_height(3.0)
                            .text(""),
                    );
                }
            });
    }

    /// Edge and corner grips, because an overlay has no native resize border.
    ///
    /// A grip only exists while the pointer is over it, so every other click in
    /// the window still reaches the widget underneath.
    fn window_resize_grips(context: &egui::Context) {
        use egui::{CursorIcon, ResizeDirection, ViewportCommand};
        const GRIP: f32 = 5.0;
        let screen = context.screen_rect();
        let Some(pointer) = context.pointer_hover_pos() else {
            return;
        };
        let (min, max) = (screen.min, screen.max);
        let grips: [(ResizeDirection, egui::Rect, CursorIcon); 8] = [
            (
                ResizeDirection::North,
                egui::Rect::from_min_max(
                    egui::pos2(min.x + GRIP, min.y),
                    egui::pos2(max.x - GRIP, min.y + GRIP),
                ),
                CursorIcon::ResizeNorth,
            ),
            (
                ResizeDirection::South,
                egui::Rect::from_min_max(
                    egui::pos2(min.x + GRIP, max.y - GRIP),
                    egui::pos2(max.x - GRIP, max.y),
                ),
                CursorIcon::ResizeSouth,
            ),
            (
                ResizeDirection::West,
                egui::Rect::from_min_max(
                    egui::pos2(min.x, min.y + GRIP),
                    egui::pos2(min.x + GRIP, max.y - GRIP),
                ),
                CursorIcon::ResizeWest,
            ),
            (
                ResizeDirection::East,
                egui::Rect::from_min_max(
                    egui::pos2(max.x - GRIP, min.y + GRIP),
                    egui::pos2(max.x, max.y - GRIP),
                ),
                CursorIcon::ResizeEast,
            ),
            (
                ResizeDirection::NorthWest,
                egui::Rect::from_min_max(min, egui::pos2(min.x + GRIP, min.y + GRIP)),
                CursorIcon::ResizeNorthWest,
            ),
            (
                ResizeDirection::NorthEast,
                egui::Rect::from_min_max(
                    egui::pos2(max.x - GRIP, min.y),
                    egui::pos2(max.x, min.y + GRIP),
                ),
                CursorIcon::ResizeNorthEast,
            ),
            (
                ResizeDirection::SouthWest,
                egui::Rect::from_min_max(
                    egui::pos2(min.x, max.y - GRIP),
                    egui::pos2(min.x + GRIP, max.y),
                ),
                CursorIcon::ResizeSouthWest,
            ),
            (
                ResizeDirection::SouthEast,
                egui::Rect::from_min_max(
                    egui::pos2(max.x - GRIP, max.y - GRIP),
                    egui::pos2(max.x, max.y),
                ),
                CursorIcon::ResizeSouthEast,
            ),
        ];
        for (index, (direction, rect, cursor)) in grips.into_iter().enumerate() {
            if !rect.contains(pointer) {
                continue;
            }
            context.set_cursor_icon(cursor);
            let response = egui::Area::new(egui::Id::new(("kaelis-resize", index)))
                .order(egui::Order::Foreground)
                .fixed_pos(rect.min)
                .show(context, |ui| {
                    ui.allocate_response(rect.size(), egui::Sense::drag())
                })
                .inner;
            if response.drag_started() {
                context.send_viewport_cmd(ViewportCommand::BeginResize(direction));
            }
            break;
        }
    }

    fn show_footer(&mut self, context: &egui::Context) {
        egui::TopBottomPanel::bottom("kaelis-footer")
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgba_premultiplied(7, 8, 11, 248))
                    .inner_margin(egui::Margin::symmetric(14.0, 3.0)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    // The rail already carries Community and GitHub; repeating
                    // them here only added noise to a 12 px strip. When GitHub
                    // has a newer tag the version itself becomes the link, so
                    // the strip stays one line tall.
                    let version = env!("CARGO_PKG_VERSION");
                    let release = self.launcher_release.clone();
                    if let Some(release) = &release {
                        let text = egui::RichText::new(format!(
                            "{} {version} · {} available",
                            kaelis_core::brand::LAUNCHER_NAME,
                            release.version
                        ))
                        .size(8.5)
                        .color(accent_soft());
                        let response = ui.add(
                            egui::Label::new(text)
                                .sense(egui::Sense::click())
                                .selectable(false),
                        );
                        if response.clicked() {
                            ui.ctx()
                                .open_url(egui::OpenUrl::new_tab(release.url.clone()));
                        }
                        response.on_hover_text(format!(
                            "Open the Kaelis {} release page",
                            release.version
                        ));
                        // One click replaces this build: the download is
                        // checksum-verified first, then the launcher restarts
                        // itself into the new version. Installing in place is a
                        // Windows behavior; the macOS and Linux archives are
                        // unpacked deliberately, so those builds keep only the
                        // release link above instead of offering an action that
                        // could not replace them.
                        #[cfg(windows)]
                        {
                            ui.add_space(6.0);
                            if self.update_busy {
                                ui.add(egui::Spinner::new().size(9.0));
                            } else if ui
                                .add(
                                    egui::Label::new(
                                        egui::RichText::new("Update")
                                            .size(8.5)
                                            .strong()
                                            .color(accent_soft()),
                                    )
                                    .sense(egui::Sense::click())
                                    .selectable(false),
                                )
                                .on_hover_text(
                                    "Download the verified build, replace this copy, and restart",
                                )
                                .clicked()
                            {
                                self.start_self_update(context.clone());
                            }
                        }
                    } else {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} {version}",
                                kaelis_core::brand::LAUNCHER_NAME
                            ))
                            .size(8.5)
                            .color(MUTED),
                        );
                    }
                    // The status sentence moved here from the window bar: the
                    // bar keeps only the tab name and the window controls, and
                    // this strip already had a spare line. It truncates rather
                    // than pushing the FPS readout, and the full text is one
                    // hover away.
                    if !self.status.is_empty() {
                        ui.add_space(8.0);
                        let room = (ui.available_width() - 210.0).max(60.0);
                        ui.allocate_ui_with_layout(
                            egui::vec2(room, 12.0),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&self.status)
                                            .size(8.5)
                                            .color(self.status_kind.color()),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(&self.status);
                            },
                        );
                    }
                    // The right side is silent when the launcher is idle:
                    // "Idle · event-driven" described the scheduler to nobody.
                    if self.governor.target_fps() != 0 {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                egui::RichText::new("60 FPS while working")
                                    .size(8.5)
                                    .color(MUTED),
                            );
                        });
                    }
                });
            });
    }

    /// Tabs whose content can exceed the window get their own scroll area, so
    /// shrinking the window never hides settings or account controls.
    /// Instances and Content already scroll their own lists.
    fn show_tab(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        match self.current_tab {
            Tab::Instances => self.show_instances(ui, context),
            Tab::Content => self.show_content(ui, context),
            Tab::Overview | Tab::Account | Tab::Settings => {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        match self.current_tab {
                            Tab::Overview => self.show_overview(ui, context),
                            Tab::Account => self.show_account(ui, context),
                            _ => self.show_settings(ui),
                        }
                    });
            }
        }
    }

    fn show_overview(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        let selected = self.selected_instance().cloned();
        let selected_icon = selected.as_ref().and_then(|instance| {
            self.instance_icons
                .get(&instance.id)
                .and_then(|state| match state {
                    ModIconState::Ready(texture) => Some(texture.clone()),
                    _ => None,
                })
        });
        let counts = ContentKind::ALL.map(|kind| {
            (
                kind,
                self.installed
                    .as_ref()
                    .map(|scan| self.content_library.count(kind, scan))
                    .unwrap_or(0),
            )
        });
        ui.add_space(8.0);
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                draw_brand_mark(ui, 44.0);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(kaelis_core::brand::TAGLINE)
                            .size(19.0)
                            .strong(),
                    );
                    ui.horizontal_wrapped(|ui| {
                        let selected_running = selected
                            .as_ref()
                            .is_some_and(|instance| self.instance_running(&instance.id));
                        if selected_running {
                            if quit_button(ui, "Quit game", egui::vec2(112.0, CONTROL_HEIGHT))
                                .on_hover_text("End this Minecraft process now")
                                .clicked()
                                && let Some(instance) = &selected
                            {
                                self.stop_instance(&instance.id, context);
                            }
                        } else if primary_button(ui, "Launch selected").clicked() {
                            self.launch_selected(context.clone());
                        }
                        if ui
                            .button("Content")
                            .on_hover_text("Install and manage mods, packs, shaders, and worlds.")
                            .clicked()
                        {
                            self.current_tab = Tab::Content;
                        }
                        if ui.button("New instance").clicked() {
                            self.current_tab = Tab::Instances;
                        }
                    });
                });
            });
        });
        ui.add_space(7.0);
        // One row, the selected instance only: the caption above it said what
        // the card already shows, and the button beside it opened a tab the
        // sidebar has one click away.
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if let Some(instance) = &selected {
                    draw_instance_avatar(ui, selected_icon.as_ref(), instance);
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&instance.name).size(14.0).strong());
                        ui.label(
                            egui::RichText::new(format!(
                                "Minecraft {} · {} · {} MiB",
                                instance.mc_version, instance.loader, instance.ram_mb
                            ))
                            .size(10.0)
                            .color(MUTED),
                        );
                    });
                } else {
                    ui.label("No instance selected yet.");
                }
            });
        });
        ui.add_space(7.0);
        let mut open_kind = None;
        // Four cards need 720 px to keep their labels on one line; below that
        // they become a 2×2 board rather than shrinking to unreadable chips.
        if ui.available_width() >= 720.0 {
            ui.columns(4, |columns| {
                for (column, (kind, count)) in columns.iter_mut().zip(counts.iter().copied()) {
                    if content_stat_card(column, kind, count) {
                        open_kind = Some(kind);
                    }
                }
            });
        } else {
            for row in counts.chunks(2) {
                ui.columns(2, |columns| {
                    for (column, (kind, count)) in columns.iter_mut().zip(row.iter().copied()) {
                        if content_stat_card(column, kind, count) {
                            open_kind = Some(kind);
                        }
                    }
                });
                ui.add_space(5.0);
            }
        }
        if let Some(kind) = open_kind {
            self.content_kind = kind;
            self.current_tab = Tab::Content;
        }
        ui.add_space(7.0);
        let account_name = self.account_name();
        let account_kind = self.account_kind();
        let latest = self
            .versions
            .iter()
            .find(|version| version.kind == "release")
            .map(|version| version.id.clone())
            .unwrap_or_else(|| "—".to_owned());
        let mut manage_account = false;
        if ui.available_width() >= 620.0 {
            ui.columns(2, |columns| {
                manage_account =
                    account_summary_card(&mut columns[0], &account_name, &account_kind);
                release_summary_card(&mut columns[1], &latest);
            });
        } else {
            manage_account = account_summary_card(ui, &account_name, &account_kind);
            ui.add_space(5.0);
            release_summary_card(ui, &latest);
        }
        if manage_account {
            self.current_tab = Tab::Account;
        }
    }

    fn show_instance_editor(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        if self.editing.is_none() {
            return;
        }
        ui.add_space(10.0);
        let mut save = false;
        let mut cancel = false;
        let mut revert = false;
        let mut folder = None;
        let mut manage_content = None;
        let mut erase_request: Option<String> = None;
        let mut paste_artwork = false;
        let mut browse_artwork = false;
        let mut copy_json = false;
        let mut paste_json = false;
        let content_counts = ContentKind::ALL.map(|kind| {
            (
                kind,
                self.installed
                    .as_ref()
                    .map(|scan| self.content_library.count(kind, scan))
                    .unwrap_or(0),
            )
        });
        let preview_icon = self
            .editing
            .as_ref()
            .and_then(|edit| self.instance_icons.get(&edit.id))
            .and_then(|state| match state {
                ModIconState::Ready(texture) => Some(texture.clone()),
                _ => None,
            });
        let Some(edit) = self.editing.as_mut() else {
            return;
        };
        egui::Frame::none()
            .fill(SURFACE)
            .stroke(egui::Stroke::new(1.0_f32, BORDER))
            .rounding(egui::Rounding::same(CORNER_XL))
            .inner_margin(egui::Margin::same(16.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new("INSTANCE PROPERTIES")
                                .size(9.5)
                                .strong()
                                .color(accent()),
                        );
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&edit.name).size(19.0).strong());
                            chip(ui, &format!("MC {}", edit.mc_version), MUTED);
                            chip(ui, &edit.loader, accent());
                            chip(ui, &format!("{} MiB", edit.ram_mb), MUTED);
                        });
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            cancel = true;
                        }
                        ui.menu_button("Content", |ui| {
                            ui.set_min_width(180.0);
                            for kind in ContentKind::ALL {
                                if ui.button(kind.label()).clicked() {
                                    manage_content = Some(kind);
                                    ui.close_menu();
                                }
                            }
                        });
                        ui.menu_button("Copy / Paste", |ui| {
                            ui.set_min_width(190.0);
                            if ui.button("Copy instance JSON").clicked() {
                                copy_json = true;
                                ui.close_menu();
                            }
                            if ui.button("Paste instance from clipboard").clicked() {
                                paste_json = true;
                                ui.close_menu();
                            }
                        });
                    });
                });
                ui.add_space(10.0);
                ui.separator();
                ui.add_space(10.0);

                let body_height = (ui.available_height() - 66.0).max(200.0);
                egui::ScrollArea::vertical()
                    .id_source("kaelis-instance-editor-body")
                    .max_height(body_height)
                    .show(ui, |ui| {
                ui.columns(2, |columns| {
                    editor_section(
                        &mut columns[0],
                        "General",
                        "Identity, version, and notes",
                        |ui| {
                            editor_row(ui, "Name", |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut edit.name)
                                        .desired_width(210.0),
                                );
                            });
                            editor_row(ui, "Group", |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.edit_group)
                                        .hint_text("Optional, e.g. Favourites")
                                        .desired_width(210.0),
                                );
                            });
                            editor_row(ui, "Minecraft", |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut edit.mc_version)
                                        .desired_width(110.0),
                                );
                                ui.label(
                                    egui::RichText::new("Erase and re-install to change")
                                        .size(9.5)
                                        .color(MUTED),
                                );
                            });
                            editor_row(ui, "Loader", |ui| {
                                egui::ComboBox::from_id_source("kaelis-instance-loader")
                                    .selected_text(&edit.loader)
                                    .width(132.0)
                                    .show_ui(ui, |ui| {
                                        for loader in
                                            ["vanilla", "fabric", "forge", "neoforge", "quilt"]
                                        {
                                            ui.selectable_value(
                                                &mut edit.loader,
                                                loader.to_owned(),
                                                loader,
                                            );
                                        }
                                    });
                            });
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new("Notes").size(10.0).color(MUTED));
                            ui.add(
                                egui::TextEdit::multiline(&mut self.edit_notes)
                                    .desired_rows(3)
                                    .hint_text("Load order, pack notes, reminders…")
                                    .desired_width(210.0),
                            );
                        },
                    );
                    editor_section(
                        &mut columns[0],
                        "Appearance",
                        "Card artwork",
                        |ui| {
                            editor_row(ui, "Artwork", |ui| {
                                ui.vertical(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.edit_icon)
                                            .id(egui::Id::new(ARTWORK_FIELD_ID))
                                            .hint_text("Path, data URL, or base64")
                                            .desired_width(210.0),
                                    )
                                    .on_hover_text(
                                        "Drop a picture on the window, or pick one below. \
                                         PNG, JPEG, WebP, GIF, BMP, ICO, and SVG become one \
                                         256 px square.",
                                    );
                                    ui.horizontal(|ui| {
                                        if ui
                                            .button("Choose image…")
                                            .on_hover_text("Pick a PNG, JPEG, WebP, GIF, BMP, ICO, or SVG from this computer.")
                                            .clicked()
                                        {
                                            browse_artwork = true;
                                        }
                                        if ui
                                            .button("Paste")
                                            .on_hover_text("Use the picture on the clipboard.")
                                            .clicked()
                                        {
                                            paste_artwork = true;
                                        }
                                        if ui.button("Clear").clicked() {
                                            self.edit_icon.clear();
                                        }
                                        // Ctrl+V over this field means "the
                                        // picture I copied", not text.
                                        if ui.input(|input| {
                                            input.modifiers.command
                                                && input.key_pressed(egui::Key::V)
                                        }) && ui.memory(|memory| {
                                            memory.focused()
                                                == Some(egui::Id::new(ARTWORK_FIELD_ID))
                                        }) {
                                            paste_artwork = true;
                                        }
                                    });
                                });
                            });
                            ui.add_space(4.0);
                            ui.horizontal(|ui| {
                                draw_instance_avatar(ui, preview_icon.as_ref(), edit);
                                ui.add_space(6.0);
                                ui.vertical(|ui| {
                                    ui.label(
                                        egui::RichText::new("CARD PREVIEW")
                                            .size(9.0)
                                            .color(MUTED),
                                    );
                                    ui.label(
                                        egui::RichText::new(edit.root.display().to_string())
                                            .size(9.5)
                                            .color(MUTED),
                                    );
                                });
                            });
                        },
                    );

                    editor_section(
                        &mut columns[0],
                        "Content library",
                        "Every channel lands in its own instance folder",
                        |ui| {
                            for (kind, count) in content_counts.iter().copied() {
                                ui.horizontal(|ui| {
                                    let (mark, _) = ui.allocate_exact_size(
                                        egui::vec2(20.0, 20.0),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().rect_filled(mark, CORNER_SM, GLASS_LIGHT);
                                    ui.painter().text(
                                        mark.center(),
                                        egui::Align2::CENTER_CENTER,
                                        kind.label().chars().next().unwrap_or('•'),
                                        egui::FontId::proportional(11.0),
                                        kind_color(kind),
                                    );
                                    ui.label(
                                        egui::RichText::new(kind.label()).size(10.5).strong(),
                                    );
                                    chip(ui, &count.to_string(), kind_color(kind));
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui.small_button("Folder").clicked() {
                                                folder = Some(match kind {
                                                    ContentKind::Mod => InstanceFolder::Mods,
                                                    ContentKind::ResourcePack => {
                                                        InstanceFolder::ResourcePacks
                                                    }
                                                    ContentKind::Shader => {
                                                        InstanceFolder::ShaderPacks
                                                    }
                                                    ContentKind::Modpack => InstanceFolder::Root,
                                                });
                                            }
                                            if ui.small_button("Browse").clicked() {
                                                manage_content = Some(kind);
                                            }
                                        },
                                    );
                                });
                            }
                            ui.label(
                                egui::RichText::new(
                                    "Mods need the loader; packs, shaders, and worlds install directly.",
                                )
                                .size(9.5)
                                .color(MUTED),
                            );
                        },
                    );
                    editor_section(
                        &mut columns[1],
                        "Java & memory",
                        "Leave Java empty to use the managed runtime",
                        |ui| {
                            editor_row(ui, "Java", |ui| {
                                ui.add(
                                egui::TextEdit::singleline(&mut self.edit_java)
                                    .hint_text("Managed runtime or java.exe")
                                    .desired_width(210.0),
                                );
                                if ui.small_button("Clear").clicked() {
                                    self.edit_java.clear();
                                }
                            });
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new("Maximum heap")
                                    .size(10.0)
                                    .color(MUTED),
                            );
                            // A set track keeps the value box inside the column
                            // instead of drawing over the checkbox row below.
                            ui.spacing_mut().slider_width = 200.0;
                            ui.add(
                                egui::Slider::new(&mut edit.ram_mb, 512..=32_768)
                                    .step_by(512.0)
                                    .suffix(" MiB"),
                            );
                            ui.horizontal(|ui| {
                                ui.checkbox(
                                    &mut self.edit_custom_min_ram,
                                    "Custom initial heap",
                                )
                                .on_hover_text(
                                    "Sets -Xms. Leave off to let Kaelis use 512 MiB.",
                                );
                                ui.add_enabled_ui(self.edit_custom_min_ram, |ui| {
                                    let max = edit.ram_mb.max(512);
                                    ui.add(
                                        egui::DragValue::new(&mut self.edit_min_ram)
                                            .range(512..=max)
                                            .speed(128.0)
                                            .suffix(" MiB"),
                                    );
                                });
                            });
                        },
                    );
                    editor_section(
                        &mut columns[1],
                        "Window & server",
                        "Applied after Minecraft's own arguments",
                        |ui| {
                            ui.checkbox(&mut edit.fullscreen, "Start fullscreen");
                            ui.horizontal(|ui| {
                                ui.checkbox(
                                    &mut self.edit_custom_window,
                                    "Custom window size",
                                );
                                ui.add_enabled_ui(self.edit_custom_window, |ui| {
                                    ui.add(
                                        egui::DragValue::new(&mut self.edit_window_width)
                                            .range(320..=16_384)
                                            .speed(16.0)
                                            .prefix("W "),
                                    );
                                    ui.label(egui::RichText::new("×").color(MUTED));
                                    ui.add(
                                        egui::DragValue::new(&mut self.edit_window_height)
                                            .range(240..=16_384)
                                            .speed(16.0)
                                            .prefix("H "),
                                    );
                                });
                            });
                            editor_row(ui, "Quick join", |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.edit_quick_join)
                                        .hint_text("play.example.net:25565")
                                        .desired_width(210.0),
                                );
                            });
                            ui.label(
                                egui::RichText::new(
                                    "Uses quick-play on 1.20+ and the legacy server/port pair on older clients.",
                                )
                                .size(9.5)
                                .color(MUTED),
                            );
                        },
                    );
                });

                ui.add_space(2.0);
                egui::CollapsingHeader::new(
                    egui::RichText::new("Advanced launch options").size(12.0),
                )
                .default_open(false)
                .show(ui, |ui| {
                    ui.columns(2, |columns| {
                        editor_section(
                            &mut columns[0],
                            "JVM arguments",
                            "One per line, appended after Kaelis defaults",
                            |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.edit_jvm_args)
                                        .code_editor()
                                        .desired_rows(3)
                                        .hint_text("-XX:+AlwaysPreTouch")
                                        .desired_width(210.0),
                                );
                            },
                        );
                        editor_section(
                            &mut columns[0],
                            "Game arguments",
                            "Appended after Minecraft's arguments",
                            |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.edit_game_args)
                                        .code_editor()
                                        .desired_rows(3)
                                        .hint_text("--demo")
                                        .desired_width(210.0),
                                );
                            },
                        );
                        editor_section(
                            &mut columns[1],
                            "Environment",
                            "KEY=VALUE, one per line",
                            |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.edit_env)
                                        .code_editor()
                                        .desired_rows(3)
                                        .hint_text("JAVA_TOOL_OPTIONS=-Dfile.encoding=UTF-8")
                                        .desired_width(210.0),
                                );
                            },
                        );
                        editor_section(
                            &mut columns[1],
                            "Automation",
                            "Shell commands run in the instance folder",
                            |ui| {
                                ui.label(
                                    egui::RichText::new("Before launch")
                                        .size(10.0)
                                        .color(MUTED),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.edit_pre_launch)
                                        .hint_text("Optional command")
                                        .desired_width(210.0),
                                );
                                ui.label(
                                    egui::RichText::new("After exit")
                                        .size(10.0)
                                        .color(MUTED),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.edit_post_exit)
                                        .hint_text("Optional command")
                                        .desired_width(210.0),
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Hooks time out after 120 seconds and never block the launch.",
                                    )
                                    .size(9.5)
                                    .color(MUTED),
                                );
                            },
                        );
                    });
                });
                });

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if primary_button(ui, "Save changes")
                        .on_hover_text("Save instance properties")
                        .clicked()
                    {
                        save = true;
                    }
                    if ui.button("Revert").clicked() {
                        revert = true;
                    }
                    ui.menu_button("Open folder", |ui| {
                        for choice in [
                            InstanceFolder::Root,
                            InstanceFolder::Mods,
                            InstanceFolder::Saves,
                            InstanceFolder::ResourcePacks,
                            InstanceFolder::ShaderPacks,
                            InstanceFolder::Screenshots,
                            InstanceFolder::Logs,
                            InstanceFolder::Json,
                        ] {
                            if ui.button(choice.label()).clicked() {
                                folder = Some(choice);
                                ui.close_menu();
                            }
                        }
                    });
                    if ui.button("Copy path").clicked() {
                        ui.output_mut(|output| {
                            output.copied_text = edit.root.display().to_string();
                        });
                    }
                    ui.separator();
                    if danger_button(ui, "Erase instance")
                        .on_hover_text(
                            "Delete every file, log, and record of this instance immediately",
                        )
                        .clicked()
                    {
                        erase_request = Some(edit.id.clone());
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(
                                "Version or loader changes need Install / Repair",
                            )
                            .size(9.5)
                            .color(MUTED),
                        );
                    });
                });
            });
        if let Some(id) = erase_request {
            self.erase_instance(&id);
        }
        if browse_artwork {
            self.browse_artwork(context);
        }
        if paste_artwork {
            self.paste_clipboard_artwork(context);
        }
        if copy_json {
            self.copy_instance_json(context);
        }
        if paste_json {
            self.paste_instance_json();
        }
        if save {
            self.save_instance_edit(context);
        }
        if revert
            && let Some(original) = self
                .editing
                .as_ref()
                .and_then(|edit| self.instances.iter().find(|item| item.id == edit.id))
                .cloned()
        {
            self.begin_edit(original);
            self.set_status(StatusKind::Info, "Changes reverted.");
        }
        if cancel {
            self.close_editor();
        }
        if let Some(kind) = manage_content {
            self.close_editor();
            self.current_tab = Tab::Content;
            self.content_kind = kind;
            self.mod_results = ModSearchResults::default();
            self.mod_rows.clear();
            self.mod_icons.clear();
            self.mod_results_query.clear();
            self.mod_refine.clear();
            self.mod_last_request.clear();
            self.mod_pending_signature.clear();
            self.mod_signature_changed_at = Some(std::time::Instant::now());
            self.preview_key = None;
            self.set_status(
                StatusKind::Info,
                format!("Browsing {} for this instance.", kind.label()),
            );
        }
        if let Some(folder) = folder
            && let Some(edit) = self.editing.as_ref()
        {
            let (path, reveal) = match folder {
                InstanceFolder::Root => (edit.root.clone(), false),
                InstanceFolder::Json => (
                    Instance::instances_dir().join(format!("{}.json", edit.id)),
                    true,
                ),
                _ => (
                    edit.root.join(folder.relative_path().unwrap_or_default()),
                    false,
                ),
            };
            let result = if folder.relative_path().is_some() {
                std::fs::create_dir_all(&path)
                    .map_err(|error| error.to_string())
                    .and_then(|()| open_instance_path(&path, reveal))
            } else {
                open_instance_path(&path, reveal)
            };
            if let Err(error) = result {
                self.set_status(
                    StatusKind::Error,
                    format!("Could not open {}: {error}", folder.label()),
                );
            }
        }
    }

    fn show_instances(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        if self.editing.is_none() {
            ui.add_space(10.0);
            Self::glass_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.heading("Create instance");
                let conflict = instance_name_conflict(&self.instances, &self.new_instance_name)
                    .map(|instance| name_conflict_text(self.new_instance_name.trim(), instance));
                let name_taken = conflict.is_some();
                let busy = self.installing_instance.is_some() || self.installing_runtime.is_some();
                let ready = !self.new_instance_name.trim().is_empty()
                    && !name_taken
                    && !self.new_instance_version.trim().is_empty();
                // Wrapped: a narrow window stacks the controls instead of
                // clipping the Create button off the right edge.
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.new_instance_name)
                            .hint_text("Instance name")
                            .desired_width((ui.available_width() * 0.24).clamp(176.0, 360.0)),
                    );
                    // Each label stays on the same line as its own control:
                    // a wrapped row that splits "Minecraft" from the version
                    // box reads as two unrelated columns.
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label("Minecraft");
                        if self.new_instance_version.is_empty()
                            && let Some(latest) = self
                                .versions
                                .iter()
                                .find(|version| version.kind == "release")
                            && ui
                                .small_button(format!("Latest · {}", latest.id))
                                .on_hover_text("Use the newest Mojang release.")
                                .clicked()
                        {
                            self.new_instance_version = latest.id.clone();
                        }
                        let filtered =
                            self.versions
                                .iter()
                                .filter(|version| match version.kind.as_str() {
                                    "old_alpha" => self.show_alpha,
                                    "old_beta" => self.show_beta,
                                    "snapshot" => self.show_snapshots,
                                    _ => true,
                                });
                        egui::ComboBox::from_id_source("kaelis-instance-version")
                            .selected_text(if self.versions.is_empty() {
                                "Loading versions…"
                            } else if self.new_instance_version.is_empty() {
                                "Select version"
                            } else {
                                &self.new_instance_version
                            })
                            .show_ui(ui, |ui| {
                                if self.versions.is_empty() {
                                    ui.label(
                                        egui::RichText::new("Fetching the Mojang manifest…")
                                            .size(10.0)
                                            .color(MUTED),
                                    );
                                }
                                for version in filtered.into_iter().take(400) {
                                    if ui
                                        .selectable_label(
                                            self.new_instance_version == version.id,
                                            &version.id,
                                        )
                                        .clicked()
                                    {
                                        self.new_instance_version = version.id.clone();
                                    }
                                }
                            });
                    });
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label("Loader");
                    egui::ComboBox::from_id_source("kaelis-instance-loader")
                        .selected_text(&self.new_instance_loader)
                        .show_ui(ui, |ui| {
                            for loader in ["vanilla", "fabric", "quilt", "forge", "neoforge"] {
                                ui.selectable_value(
                                    &mut self.new_instance_loader,
                                    loader.to_owned(),
                                    loader,
                                );
                            }
                        });
                    });
                    let install_label = if busy { "Installing…" } else { "Create" };
                    // The primary action of this panel, in the one accent the
                    // whole app uses, at the same height and width as the
                    // import/export pair so the row reads as one control group.
                    let create = ui
                        .add_enabled_ui(!busy && ready, |ui| primary_button(ui, install_label))
                        .inner;
                    if create
                        .on_disabled_hover_text(if busy {
                            "Wait for the current installation to finish."
                        } else {
                            "Name the instance and pick a Minecraft version first."
                        })
                        .on_hover_text("Downloads the official client, libraries, and assets.")
                        .clicked()
                    {
                        self.create_instance(context.clone());
                    }
                    // Import and export are one pair of directions, so they
                    // sit together and share one icon family. Export acts on
                    // the selected instance, which the tooltip says plainly.
                    if glyph_button(
                        ui,
                        PortableGlyph::Import,
                        Some("Import"),
                        !busy,
                        "Open a .kaelis.zip, .mrpack, CurseForge or Prism export, or drop one anywhere.",
                    )
                    .clicked()
                    {
                        self.pick_instance_archive(context);
                    }
                    let export_target = self.selected_instance().cloned();
                    if glyph_button(
                        ui,
                        PortableGlyph::Export,
                        Some("Export"),
                        !busy && export_target.is_some(),
                        if export_target.is_some() {
                            "Write the selected instance and its files into a portable .kaelis.zip."
                        } else {
                            "Select an instance first, then export it as a portable .kaelis.zip."
                        },
                    )
                    .clicked()
                        && let Some(instance) = export_target
                    {
                        self.export_instance_archive(instance, context);
                    }
                });                ui.horizontal_wrapped(|ui| {
                    ui.checkbox(&mut self.show_snapshots, "Snapshots");
                    ui.checkbox(&mut self.show_alpha, "Old alpha");
                    ui.checkbox(&mut self.show_beta, "Old beta");
                    if ui
                        .small_button("Refresh versions")
                        .on_hover_text("Re-download the Mojang version manifest.")
                        .clicked()
                    {
                        self.request_versions(context.clone());
                    }
                });
                // The summary line carries only facts: a name conflict, or
                // what the button is about to build. Empty states say nothing,
                // because the Create button already refuses and explains why in
                // its own tooltip.
                let plan = if let Some(message) = &conflict {
                    (message.clone(), StatusKind::Warning.color())
                } else if self.new_instance_name.trim().is_empty()
                    || self.new_instance_version.trim().is_empty()
                {
                    (String::new(), MUTED)
                } else if self
                    .new_instance_loader
                    .eq_ignore_ascii_case("vanilla")
                {
                    (
                        format!(
                            "Minecraft {} · vanilla · no mods",
                            self.new_instance_version
                        ),
                        MUTED,
                    )
                } else {
                    (
                        format!(
                            "Minecraft {} · {} · mods supported",
                            self.new_instance_version, self.new_instance_loader
                        ),
                        accent_soft(),
                    )
                };
                if !plan.0.is_empty() {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        ui.label(egui::RichText::new(plan.0).size(10.0).color(plan.1));
                    });
                }
            });
        }
        self.show_instance_editor(ui, context);
        ui.add_space(14.0);
        // No heading: the tab is named Instances and the list starts where the
        // create card ends. The filter and Recheck keep the row to themselves.
        ui.horizontal(|ui| {
            search_pill(
                ui,
                &mut self.instance_filter,
                INSTANCE_FILTER_ID,
                "Filter by name, group, version, or loader…",
                330.0,
            );
            if ui
                .button("Recheck")
                .on_hover_text("Re-scan metadata, client jar, Java, and heap settings.")
                .clicked()
            {
                self.refresh_instance_cards();
                self.reload_instance_icons(context);
            }
            if !self.instance_filter.trim().is_empty() {
                let shown = self
                    .instances
                    .iter()
                    .filter(|instance| instance_matches(instance, &self.instance_filter))
                    .count();
                ui.label(
                    egui::RichText::new(format!("{shown} of {} shown", self.instances.len()))
                        .size(10.0)
                        .color(MUTED),
                );
            }
        });
        if self.instances.is_empty() {
            Self::glass_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    egui::RichText::new("Nothing installed yet")
                        .size(15.0)
                        .strong(),
                );
            });
        } else if !self.instance_filter.trim().is_empty()
            && !self
                .instances
                .iter()
                .any(|instance| instance_matches(instance, &self.instance_filter))
        {
            Self::glass_frame().show(ui, |ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "No instance matches \"{}\".",
                        self.instance_filter.trim()
                    ))
                    .color(MUTED),
                );
                if ui.small_button("Clear filter").clicked() {
                    self.instance_filter.clear();
                }
            });
        }
        let mut action = None;
        {
            let cards = &self.instance_cards;
            let icons = &self.instance_icons;
            let installing =
                self.installing_instance.is_some() || self.installing_runtime.is_some();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (index, instance) in self
                    .instances
                    .iter()
                    .enumerate()
                    .filter(|(_, instance)| instance_matches(instance, &self.instance_filter))
                {
                    let Some(card) = cards.get(&instance.id) else {
                        continue;
                    };
                    let running = self.running_instances.contains(&instance.id);
                    let card_frame = Self::glass_frame().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let icon = icons.get(&instance.id).and_then(|state| match state {
                                ModIconState::Ready(texture) => Some(texture),
                                _ => None,
                            });
                            let (stripe, _) = ui.allocate_exact_size(
                                egui::vec2(3.0, 44.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().rect_filled(
                                stripe,
                                egui::Rounding::same(2.0),
                                accent(),
                            );
                            ui.add_space(6.0);
                            let avatar = draw_instance_avatar(ui, icon, instance);
                            let pick = ui
                                .interact(
                                    avatar,
                                    ui.id().with(("instance-artwork", instance.id.as_str())),
                                    egui::Sense::click(),
                                )
                                .on_hover_text("Change icon…")
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                            // Screen readers and UI automation both get the same
                            // wording as the tooltip.
                            pick.widget_info(|| {
                                egui::WidgetInfo::labeled(
                                    egui::WidgetType::Button,
                                    true,
                                    "Change instance icon",
                                )
                            });
                            if pick.hovered() {
                                ui.painter().rect_filled(
                                    avatar,
                                    egui::Rounding::same(10.0),
                                    egui::Color32::from_black_alpha(140),
                                );
                                let centre = avatar.center();
                                let stroke = egui::Stroke::new(1.4_f32, TEXT);
                                ui.painter()
                                    .line_segment([centre - egui::vec2(5.0, 0.0), centre + egui::vec2(5.0, 0.0)], stroke);
                                ui.painter()
                                    .line_segment([centre - egui::vec2(0.0, 5.0), centre + egui::vec2(0.0, 5.0)], stroke);
                            }
                            if pick.clicked() {
                                action = Some((index, InstanceAction::PickIcon));
                            }
                            ui.add_space(8.0);
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(&instance.name)
                                            .strong()
                                            .size(15.0),
                                    );
                                    if !instance.group.trim().is_empty() {
                                        chip(ui, instance.group.trim(), MUTED);
                                    }
                                    if running {
                                        chip(ui, "RUNNING", StatusKind::Success.color());
                                    }
                                });
                                ui.label(egui::RichText::new(&card.detail).size(11.0));
                                ui.horizontal(|ui| {
                                    chip(ui, &card.content, accent());
                                    if !card.last_played.is_empty() {
                                        chip(ui, &card.last_played, MUTED);
                                    }
                                });
                                if !card.notes_preview.is_empty() {
                                    ui.label(
                                        egui::RichText::new(&card.notes_preview)
                                            .size(10.0)
                                            .italics()
                                            .color(accent_soft()),
                                    )
                                    .on_hover_text(&card.notes);
                                }
                                ui.label(
                                    egui::RichText::new(&card.root)
                                        .size(9.5)
                                        .color(MUTED),
                                );
                                ui.horizontal(|ui| {
                                    if check_chip(
                                        ui,
                                        card.health.metadata,
                                        "Files",
                                        "Mojang version metadata is missing. Click to install / repair.",
                                    )
                                    .clicked()
                                        && card.health.metadata != CheckState::Ok
                                    {
                                        action = Some((index, InstanceAction::InstallVanilla));
                                    }
                                    if check_chip(
                                        ui,
                                        card.health.client,
                                        "Client",
                                        "Mojang client jar is missing. Click to install / repair.",
                                    )
                                    .clicked()
                                        && card.health.client != CheckState::Ok
                                    {
                                        action = Some((index, InstanceAction::InstallVanilla));
                                    }
                                    let java_detail = match card.health.java {
                                        CheckState::Ok => "A saved Java runtime is present.",
                                        CheckState::Warn => {
                                            "No runtime saved; launches use Java from PATH. Click to install one."
                                        }
                                        CheckState::Missing => {
                                            "The saved Java path is gone. Click to reinstall Java."
                                        }
                                    };
                                    if check_chip(ui, card.health.java, "Java", java_detail).clicked()
                                        && card.health.java != CheckState::Ok
                                    {
                                        action = Some((index, InstanceAction::InstallJava));
                                    }
                                    if check_chip(
                                        ui,
                                        card.health.ram,
                                        "RAM",
                                        "Heap must be 512-65,536 MiB. Click to open Settings.",
                                    )
                                    .clicked()
                                        && card.health.ram != CheckState::Ok
                                    {
                                        action = Some((index, InstanceAction::Settings));
                                    }
                                });
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.menu_button("More", |ui| {
                                        ui.set_min_width(170.0);
                                        if ui.button("Select instance").clicked() {
                                            action = Some((index, InstanceAction::Select));
                                            ui.close_menu();
                                        }
                                        if ui
                                            .button("Change icon…")
                                            .on_hover_text(
                                                "Pick card artwork from the file explorer",
                                            )
                                            .clicked()
                                        {
                                            action = Some((index, InstanceAction::PickIcon));
                                            ui.close_menu();
                                        }
                                        if ui
                                            .button("Duplicate")
                                            .on_hover_text(
                                                "Copy mods, resource packs, shaders, settings, and notes — not worlds or logs",
                                            )
                                            .clicked()
                                        {
                                            action = Some((
                                                index,
                                                InstanceAction::Duplicate { saves: false },
                                            ));
                                            ui.close_menu();
                                        }
                                        if ui
                                            .button("Duplicate with worlds")
                                            .on_hover_text(
                                                "Copy everything the plain duplicate does, plus every world save",
                                            )
                                            .clicked()
                                        {
                                            action = Some((
                                                index,
                                                InstanceAction::Duplicate { saves: true },
                                            ));
                                            ui.close_menu();
                                        }
                                        if ui
                                            .add_enabled(
                                                !installing,
                                                egui::Button::new("Install / Repair"),
                                            )
                                            .clicked()
                                        {
                                            action =
                                                Some((index, InstanceAction::InstallVanilla));
                                            ui.close_menu();
                                        }
                                        let runtime_label = if instance.java_path.is_some() {
                                            "Repair Java"
                                        } else {
                                            "Install Java"
                                        };
                                        if ui
                                            .add_enabled(
                                                !installing,
                                                egui::Button::new(runtime_label),
                                            )
                                            .clicked()
                                        {
                                            action =
                                                Some((index, InstanceAction::InstallJava));
                                            ui.close_menu();
                                        }
                                        ui.separator();
                                        ui.menu_button("Open folders", |ui| {
                                            if ui.button("Instance folder").clicked() {
                                                action =
                                                    Some((index, InstanceAction::OpenFolder));
                                                ui.close_menu();
                                            }
                                            if ui.button("Mods").clicked() {
                                                action =
                                                    Some((index, InstanceAction::OpenMods));
                                                ui.close_menu();
                                            }
                                            for (label, folder) in [
                                                ("Resource packs", "resourcepacks"),
                                                ("Shaders", "shaderpacks"),
                                                ("Worlds", "saves"),
                                                ("Logs", "logs"),
                                            ] {
                                                if ui.button(label).clicked() {
                                                    action = Some((
                                                        index,
                                                        InstanceAction::OpenPath(PathBuf::from(
                                                            folder,
                                                        )),
                                                    ));
                                                    ui.close_menu();
                                                }
                                            }
                                        });
                                        ui.menu_button("Manage content", |ui| {
                                            for kind in ContentKind::ALL {
                                                if ui.button(kind.label()).clicked() {
                                                    action = Some((
                                                        index,
                                                        InstanceAction::ManageContent(kind),
                                                    ));
                                                    ui.close_menu();
                                                }
                                            }
                                        });
                                        if ui.button("Show instance JSON").clicked() {
                                            action =
                                                Some((index, InstanceAction::ShowJson));
                                            ui.close_menu();
                                        }
                                        ui.separator();
                                        if ui
                                            .add(egui::Button::new(
                                                egui::RichText::new("Erase instance")
                                                    .color(DANGER),
                                            ))
                                            .on_hover_text(
                                                "Delete every file, log, and record of this instance",
                                            )
                                            .clicked()
                                        {
                                            action = Some((index, InstanceAction::Erase));
                                            ui.close_menu();
                                        }
                                    });
                                    // The utility cluster sits at the right:
                                    // export next to More, destructive Erase
                                    // kept away from it.
                                    if glyph_button(
                                        ui,
                                        PortableGlyph::Export,
                                        None,
                                        true,
                                        "Export this instance as a portable .kaelis.zip",
                                    )
                                    .clicked()
                                    {
                                        action = Some((index, InstanceAction::Export));
                                    }
                                    if danger_button(ui, "Erase")
                                        .on_hover_text(
                                            "Erase every file and record of this instance in one click",
                                        )
                                        .clicked()
                                    {
                                        action = Some((index, InstanceAction::Erase));
                                    }
                                    if ui.button("Edit").clicked() {
                                        action = Some((index, InstanceAction::Edit));
                                    }
                                    if running {
                                        // The same slot becomes Quit: one
                                        // click signals the launch task that
                                        // owns the JVM, and the process ends
                                        // before the card can repaint.
                                        if quit_button(ui, "Quit", egui::vec2(62.0, 24.0))
                                            .on_hover_text(
                                                "End this Minecraft process now",
                                            )
                                            .clicked()
                                        {
                                            action = Some((index, InstanceAction::Stop));
                                        }
                                    } else if ui
                                        .add_enabled(
                                            !installing,
                                            egui::Button::new(
                                                egui::RichText::new("Play")
                                                    .strong()
                                                    .color(accent_on()),
                                            )
                                            .fill(accent())
                                            .min_size(egui::vec2(62.0, 24.0)),
                                        )
                                        .on_disabled_hover_text(
                                            "Wait for the current installation to finish.",
                                        )
                                        .clicked()
                                    {
                                        action = Some((index, InstanceAction::Play));
                                    }
                                },
                            );
                        });
                    });
                    // Hover only brightens the outline: no layout shift, no fill
                    // change, so a dense list stays calm while still answering
                    // the pointer.
                    if ui
                        .interact(
                            card_frame.response.rect,
                            ui.id().with(("instance-card", instance.id.as_str())),
                            egui::Sense::hover(),
                        )
                        .hovered()
                    {
                        ui.painter().rect_stroke(
                            card_frame.response.rect,
                            egui::Rounding::same(CORNER_LG),
                            egui::Stroke::new(1.0_f32, BORDER_BRIGHT),
                        );
                    }
                    ui.add_space(8.0);
                }
            });
        }
        if let Some((index, action)) = action {
            let Some(instance) = self.instances.get(index).cloned() else {
                return;
            };
            // Erasing must not first pull selection onto the card being erased.
            if !matches!(action, InstanceAction::Erase) {
                self.select_instance(&instance.id);
            }
            match action {
                InstanceAction::Play => self.launch_selected(context.clone()),
                InstanceAction::Stop => self.stop_instance(&instance.id, context),
                InstanceAction::Select => {}
                InstanceAction::PickIcon => {
                    self.select_instance(&instance.id);
                    self.browse_artwork_for(instance.id.clone(), context);
                }
                InstanceAction::Edit => self.begin_edit(instance),
                InstanceAction::InstallVanilla => {
                    self.install_vanilla_instance(context.clone(), instance)
                }
                InstanceAction::InstallJava => {
                    self.install_runtime_for_instance(context.clone(), instance)
                }
                InstanceAction::Settings => self.current_tab = Tab::Settings,
                InstanceAction::OpenFolder => {
                    if let Err(error) = open_instance_path(&instance.root, false) {
                        self.set_status(
                            StatusKind::Error,
                            format!("Could not open instance folder: {error}"),
                        );
                    }
                }
                InstanceAction::OpenMods => self.open_mods_folder(),
                InstanceAction::OpenPath(folder) => {
                    let path = instance.root.join(folder);
                    let result = std::fs::create_dir_all(&path)
                        .map_err(|error| error.to_string())
                        .and_then(|()| open_instance_path(&path, false));
                    if let Err(error) = result {
                        self.set_status(
                            StatusKind::Error,
                            format!("Could not open {}: {error}", path.display()),
                        );
                    }
                }
                InstanceAction::Export => {
                    self.export_instance_archive(instance, context);
                }
                InstanceAction::ManageContent(kind) => {
                    self.current_tab = Tab::Content;
                    self.content_kind = kind;
                    self.mod_results = ModSearchResults::default();
                    self.mod_rows.clear();
                    self.mod_icons.clear();
                    self.mod_results_query.clear();
                    self.mod_refine.clear();
                    self.mod_last_request.clear();
                    self.mod_pending_signature.clear();
                    self.mod_signature_changed_at = Some(std::time::Instant::now());
                    self.preview_key = None;
                    self.set_status(
                        StatusKind::Info,
                        format!("Browsing {} for {}.", kind.label(), instance.name),
                    );
                }
                InstanceAction::ShowJson => {
                    let path = Instance::instances_dir().join(format!("{}.json", instance.id));
                    if let Err(error) = open_instance_path(&path, true) {
                        self.set_status(
                            StatusKind::Error,
                            format!("Could not show instance JSON: {error}"),
                        );
                    }
                }
                InstanceAction::Erase => self.erase_instance(&instance.id),
                InstanceAction::Duplicate { saves } => {
                    self.duplicate_instance(instance, saves, context)
                }
            }
        }
    }

    /// One screen for every kind of content: search and install on the left,
    /// manage what is already installed on the right. The channel tabs, the
    /// single smart search box, and drag-and-drop are the only things a user
    /// has to learn.
    fn show_content(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        let mut request = ContentRequest::default();
        let mut import: Option<ManualImport> = None;
        let mut new_target: Option<String> = None;
        if self.instances.is_empty() && self.content_kind != ContentKind::Modpack {
            ui.add_space(24.0);
            Self::glass_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.heading("Create an instance first");
                ui.add_space(6.0);
                if primary_button(ui, "Create instance").clicked() {
                    self.current_tab = Tab::Instances;
                }
            });
            return;
        }
        ui.add_space(4.0);
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let selected_name = self
                    .selected_instance()
                    .map(|instance| format!("{} · {}", instance.name, instance.mc_version))
                    .unwrap_or_else(|| "Select an instance".to_owned());
                egui::ComboBox::from_id_source("kaelis-content-target")
                    .selected_text(selected_name)
                    .width(230.0)
                    .show_ui(ui, |ui| {
                        for instance in &self.instances {
                            let selected =
                                Some(instance.id.as_str()) == self.selected_instance.as_deref();
                            let label = format!(
                                "{} · {} · {}",
                                instance.name, instance.mc_version, instance.loader
                            );
                            if ui.selectable_label(selected, label).clicked() {
                                new_target = Some(instance.id.clone());
                            }
                        }
                    });
                if self.content_kind == ContentKind::Modpack && self.selected_instance().is_none() {
                    // Packs can be browsed before any instance exists; the pack
                    // itself decides the version it needs.
                    ui.label(
                        egui::RichText::new("TARGET MC")
                            .size(9.0)
                            .strong()
                            .color(MUTED),
                    );
                    let releases = self
                        .versions
                        .iter()
                        .filter(|version| version.kind == "release")
                        .map(|version| version.id.clone())
                        .collect::<Vec<_>>();
                    let current = self
                        .browse_game_version()
                        .unwrap_or_else(|| "Pick a version".to_owned());
                    let mut chosen = None;
                    egui::ComboBox::from_id_source("kaelis-modpack-version")
                        .selected_text(current)
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for release in &releases {
                                let selected = self.new_instance_version == *release;
                                if ui.selectable_label(selected, release).clicked() {
                                    chosen = Some(release.clone());
                                }
                            }
                        });
                    if let Some(version) = chosen {
                        self.new_instance_version = version;
                        self.reset_browse();
                        self.persist_settings();
                        request.search = true;
                    }
                }
                // The version and loader chips are gone: the combo box beside
                // them already ends in "Name · version", and the loader belongs
                // to the instance, not to the toolbar.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("Rescan")
                        .on_hover_text("Re-read the instance folders from disk.")
                        .clicked()
                    {
                        request.rescan = true;
                    }
                    let instance = self.selected_instance().cloned();
                    ui.add_enabled_ui(instance.is_some(), |ui| {
                        ui.menu_button("Folders", |ui| {
                            let Some(instance) = instance.as_ref() else {
                                return;
                            };
                            if ui.button("Instance folder").clicked() {
                                request.open_path = Some(instance.root.clone());
                                ui.close_menu();
                            }
                            ui.separator();
                            for (label, folder) in [
                                ("Mods", "mods"),
                                ("Resource packs", "resourcepacks"),
                                ("Shaders", "shaderpacks"),
                                ("Worlds", "saves"),
                            ] {
                                if ui.button(label).clicked() {
                                    request.open_path = Some(instance.root.join(folder));
                                    ui.close_menu();
                                }
                            }
                        })
                        .response
                        .on_hover_text("Open the instance or any content folder in the file manager.");
                    });
                });
            });
            ui.add_space(6.0);
            let tab_width = ((ui.available_width() - 18.0) / 4.0).max(96.0);
            ui.horizontal(|ui| {
                for kind in ContentKind::ALL {
                    let count = match kind {
                        // A pack is a whole instance, not a folder entry.
                        ContentKind::Modpack => self.instances.len(),
                        _ => self
                            .installed
                            .as_ref()
                            .map(|scan| self.content_library.count(kind, scan))
                            .unwrap_or(0),
                    };
                    let selected = self.content_kind == kind;
                    let text = egui::RichText::new(format!("{}  ·  {count}", kind.label()))
                        .size(10.5)
                        .color(if selected { TEXT } else { MUTED });
                    let button = egui::Button::new(text)
                        .fill(if selected { SURFACE_HOVER } else { SURFACE_ALT })
                        .stroke(egui::Stroke::new(
                            if selected { 1.2_f32 } else { 1.0 },
                            if selected { kind_color(kind) } else { BORDER },
                        ))
                        .rounding(egui::Rounding::same(CORNER_MD));
                    if ui.add_sized([tab_width, 26.0], button).clicked() && !selected {
                        self.content_kind = kind;
                        request.kind = Some(kind);
                    }
                }
            });
            ui.add_space(6.0);
            let hint = match self.content_kind {
                ContentKind::Mod => "Search mods by name, or paste a link or .jar path…",
                ContentKind::ResourcePack => "Search resource packs, or paste a link or .zip path…",
                ContentKind::Shader => "Search shaders, or paste a link or .zip path…",
                ContentKind::Modpack => "Search modpacks, or drop a .mrpack to install it…",
            };
            ui.horizontal(|ui| {
                // Capped: a field that spans the window on a 1440 px display is
                // mostly empty surface, and the actions beside it read as a
                // loose row. 420 fits the longest hint text and nothing more.
                let width = (ui.available_width() - 236.0).clamp(200.0, 420.0);
                let query = search_pill(ui, &mut self.mod_query, MOD_SEARCH_ID, hint, width);
                let text = self.mod_query.trim().to_owned();
                let link = text.starts_with("https://") || text.starts_with("http://");
                let file_like = !link
                    && text.len() > 4
                    && (text.ends_with(".jar")
                        || text.ends_with(".zip")
                        || text.ends_with(".mrpack"));
                if query.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
                    request.clear_search = true;
                }
                let submitted =
                    query.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                let project = kaelis_api::mods::parse_project_link(&text);
                let primary = match &project {
                    Some(link) => format!("Install {}", link.kind.singular()),
                    None if link => "Install link".to_owned(),
                    None if file_like => "Install file".to_owned(),
                    None => "Search".to_owned(),
                };
                let pressed = primary_button(ui, &primary).clicked() || submitted;
                if pressed {
                    if project.is_some() || link {
                        import = Some(ManualImport::Url(text.to_owned()));
                    } else if file_like {
                        import = Some(ManualImport::File(PathBuf::from(&text)));
                    } else {
                        request.search = true;
                    }
                }
                if (project.is_some() || link || file_like) && ui.small_button("Search text").clicked()
                {
                    request.search = true;
                }
                if ui
                    .button(if self.content_import_open {
                        "Hide import"
                    } else {
                        "Import…"
                    })
                    .on_hover_text("Download a direct link, pick a file, or drag one onto the tab.")
                    .clicked()
                {
                    self.content_import_open = !self.content_import_open;
                }
            });
            if self.content_import_open {
                ui.add_space(4.0);
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("LINK").size(9.0).strong().color(MUTED));
                    ui.add_enabled(
                        !self.manual_busy,
                        egui::TextEdit::singleline(&mut self.manual_url)
                            .hint_text("Project page or direct .jar/.zip/.mrpack link")
                            .desired_width(300.0),
                    );
                    if ui
                        .add_enabled(!self.manual_busy, egui::Button::new("Download"))
                        .clicked()
                    {
                        let url = self.manual_url.trim().to_owned();
                        if url.is_empty() {
                            self.manual_status = "Paste a direct HTTPS link first.".to_owned();
                        } else {
                            import = Some(ManualImport::Url(url));
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("FILE").size(9.0).strong().color(MUTED));
                    ui.add_enabled(
                        !self.manual_busy,
                        egui::TextEdit::singleline(&mut self.manual_path)
                            .hint_text("C:\\Downloads\\sodium.jar or pack.mrpack")
                            .desired_width(300.0),
                    );
                    if ui
                        .add_enabled(!self.manual_busy, egui::Button::new("Install"))
                        .clicked()
                    {
                        let path = self.manual_path.trim().to_owned();
                        if path.is_empty() {
                            self.manual_status =
                                "Paste a local file path first, or drag a file here.".to_owned();
                        } else {
                            import = Some(ManualImport::File(PathBuf::from(path)));
                        }
                    }
                    ui.label(
                        egui::RichText::new("or drop .jar / .zip anywhere")
                            .size(9.5)
                            .color(MUTED),
                    );
                    if self.manual_busy {
                        ui.add(egui::Spinner::new().size(13.0));
                    }
                });
                if !self.manual_status.is_empty() {
                    ui.label(
                        egui::RichText::new(&self.manual_status)
                            .size(9.8)
                            .color(if self.manual_busy { accent() } else { MUTED }),
                    );
                }
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                // The combo names itself ("fabric"), so the label in front of it
                // was a word the value already says.
                if self.content_kind.uses_loader() {
                    egui::ComboBox::from_id_source("kaelis-mod-loader")
                        .selected_text(&self.mod_loader)
                        .width(92.0)
                        .show_ui(ui, |ui| {
                            for loader in ["fabric", "forge", "neoforge", "quilt"] {
                                ui.selectable_value(
                                    &mut self.mod_loader,
                                    loader.to_owned(),
                                    loader,
                                );
                            }
                        });
                    ui.checkbox(&mut self.install_dependencies, "Required dependencies")
                        .on_hover_text(
                            "Also install every required dependency a project declares.",
                        );
                    if let Some(instance) = self.selected_instance()
                        && !instance.loader.eq_ignore_ascii_case("vanilla")
                        && !instance.loader.eq_ignore_ascii_case(&self.mod_loader)
                    {
                        status_pill(
                            ui,
                            &format!("Instance is {}", instance.loader),
                            StatusKind::Warning.color(),
                        )
                        .on_hover_text(
                            "Choose the instance's loader here, or change it in the instance properties.",
                        );
                    }
                    // The "Detected fabric" pill is gone: it repeated the value
                    // in the combo box beside it. The warning above stays,
                    // because a mismatch with the instance is news.
                } else if self.content_kind == ContentKind::Modpack {
                    chip(ui, "Pack includes its loader", MUTED);
                } else {
                    chip(ui, "No loader needed", MUTED);
                }
                ui.menu_button("Options", |ui| {
                    ui.checkbox(&mut self.mod_show_modrinth, "Search Modrinth");
                    if self.content_kind == ContentKind::Modpack {
                        ui.add_enabled(false, egui::Checkbox::new(&mut false, "Search CurseForge"))
                            .on_disabled_hover_text(
                                "CurseForge packs list their files by id only; Modrinth .mrpack installs directly.",
                            );
                    } else {
                        ui.checkbox(&mut self.mod_show_curseforge, "Search CurseForge")
                            .on_hover_text(
                                "Searches all of CurseForge with no key; a link or id resolves exactly.",
                            );
                    }
                    ui.checkbox(&mut self.mod_auto_search, "Search as I type");
                });
                if self.search_busy {
                    ui.add(egui::Spinner::new().size(13.0));
                }
            });
            if self.content_kind == ContentKind::Modpack {
                ui.horizontal_wrapped(|ui| {
                    chip(ui, "Whole instances", accent_soft()).on_hover_text(
                        "A pack ships its Minecraft version, loader, and mods, so it installs as its own instance.",
                    );
                    ui.label(
                        egui::RichText::new(
                            "Installs from Modrinth .mrpack archives, or drop a .mrpack here.",
                        )
                        .size(9.5)
                        .color(MUTED),
                    );
                });
            }
            if self.content_kind == ContentKind::Shader && !self.has_shader_loader() {
                ui.horizontal_wrapped(|ui| {
                    chip(
                        ui,
                        "Shaders need Iris or Oculus",
                        StatusKind::Warning.color(),
                    );
                    if ui
                        .small_button("Find Iris")
                        .on_hover_text("Search Iris, the shader loader for Fabric and NeoForge.")
                        .clicked()
                    {
                        self.mod_query = "iris".to_owned();
                        request.kind = Some(ContentKind::Mod);
                    }
                    ui.label(
                        egui::RichText::new(
                            "Shader packs only render once a shader loader is installed.",
                        )
                        .size(9.5)
                        .color(MUTED),
                    );
                });
            }
            if self.content_kind.uses_loader()
                && self.detected_loader.is_none()
                && self
                    .selected_instance()
                    .is_some_and(|instance| instance.loader.eq_ignore_ascii_case("vanilla"))
            {
                ui.horizontal_wrapped(|ui| {
                    chip(
                        ui,
                        "Vanilla instance",
                        StatusKind::Warning.color(),
                    );
                    ui.label(
                        egui::RichText::new(
                            "Mods need Fabric, Forge, NeoForge, or Quilt — set one up in Instances first.",
                        )
                        .size(9.5)
                        .color(MUTED),
                    );
                });
            }
        });
        if let Some(kind) = request.kind {
            self.content_kind = kind;
            self.mod_refine.clear();
            self.reset_browse();
            self.persist_settings();
            request.search = true;
        }
        if request.clear_search {
            self.mod_query.clear();
            self.mod_refine.clear();
            self.reset_browse();
        }
        if let Some(id) = new_target
            && self.selected_instance.as_deref() != Some(id.as_str())
        {
            self.selected_instance = Some(id);
            self.refresh_installed();
            self.reset_browse();
        }
        if let Some(title) = self.modpack_installing.clone() {
            ui.add_space(4.0);
            Self::card_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(13.0));
                    ui.label(
                        egui::RichText::new(format!("Importing {title}"))
                            .strong()
                            .size(11.5)
                            .color(accent()),
                    );
                    if !self.modpack_detail.is_empty() {
                        ui.label(
                            egui::RichText::new(&self.modpack_detail)
                                .size(10.0)
                                .color(MUTED),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ghost_button(ui, "Cancel")
                            .on_hover_text("Stop and remove the unfinished instance")
                            .clicked()
                        {
                            request.cancel_modpack = true;
                        }
                    });
                });
            });
        }
        if let Some(job) = &self.installing_job {
            ui.add_space(4.0);
            Self::card_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(13.0));
                    ui.label(
                        egui::RichText::new(format!("Installing {}", job.item.title))
                            .strong()
                            .size(11.5)
                            .color(accent()),
                    );
                    if let Some(detail) = &self.install_detail {
                        ui.label(egui::RichText::new(detail).size(10.0).color(MUTED));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ghost_button(ui, "Cancel")
                            .on_hover_text(
                                "Stop this install; verified files already written are kept",
                            )
                            .clicked()
                        {
                            request.cancel_install = true;
                        }
                        if !self.install_queue.is_empty() {
                            if ghost_button(ui, "Clear queue").clicked() {
                                request.clear_queue = true;
                            }
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} waiting",
                                    self.install_queue.len()
                                ))
                                .size(9.5)
                                .color(MUTED),
                            );
                        }
                        if let Some(started) = self.install_started_at {
                            ui.label(
                                egui::RichText::new(format!("{}s", started.elapsed().as_secs()))
                                    .size(9.5)
                                    .color(MUTED),
                            );
                        }
                    });
                });
            });
        } else if !self.install_queue.is_empty() {
            ui.add_space(4.0);
            Self::card_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} install(s) queued",
                            self.install_queue.len()
                        ))
                        .size(10.0)
                        .color(StatusKind::Warning.color()),
                    );
                    if ghost_button(ui, "Clear").clicked() {
                        request.clear_queue = true;
                    }
                });
            });
        } else if let Some(report) = &self.last_install {
            ui.add_space(4.0);
            Self::card_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    chip(
                        ui,
                        if report.warnings.is_empty() {
                            "Installed"
                        } else {
                            "Installed · check warnings"
                        },
                        if report.warnings.is_empty() {
                            StatusKind::Success.color()
                        } else {
                            StatusKind::Warning.color()
                        },
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(report.summary()).size(10.5).color(TEXT),
                        )
                        .truncate(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ghost_button(ui, "×")
                            .on_hover_text("Dismiss this result")
                            .clicked()
                        {
                            request.dismiss_report = true;
                        }
                    });
                });
                if let Some(warning) = report.warnings.first() {
                    ui.label(
                        egui::RichText::new(warning)
                            .size(9.5)
                            .color(StatusKind::Warning.color()),
                    );
                }
            });
        }
        let signature = self.mod_signature();
        if signature != self.mod_pending_signature {
            self.mod_pending_signature = signature.clone();
            self.mod_signature_changed_at = Some(std::time::Instant::now());
        }
        let can_search = self.browse_game_version().is_some()
            && (self.mod_show_modrinth || self.mod_show_curseforge)
            && self.mod_query.trim().len() != 1;
        let debounce_due = self
            .mod_signature_changed_at
            .is_some_and(|since| since.elapsed() >= MOD_SEARCH_DEBOUNCE);
        let first_load = self.mod_last_request.is_empty() && self.mod_results.items.is_empty();
        if !request.search
            && can_search
            && (debounce_due || first_load)
            && (self.mod_auto_search || self.mod_results.items.is_empty())
            && signature != self.mod_last_request
        {
            request.search = true;
        }
        if request.search || request.retry_search {
            self.start_mod_search(context.clone());
        }
        ui.add_space(4.0);
        let size = ui.available_size();
        let gap = 10.0;
        let body_height = size.y.max(220.0);
        if size.x < 960.0 {
            // One full-height pane instead of two cramped ones: the selector
            // swaps between searching and managing what is already there.
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(!self.content_show_installed, "Browse")
                    .clicked()
                {
                    self.content_show_installed = false;
                }
                if ui
                    .selectable_label(self.content_show_installed, "Installed")
                    .clicked()
                {
                    self.content_show_installed = true;
                }
            });
            let pane_height = (body_height - 30.0).max(200.0);
            ui.allocate_ui_with_layout(
                egui::vec2(size.x, pane_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_size(egui::vec2(size.x, pane_height));
                    if self.content_show_installed {
                        self.show_content_library(ui, &mut request);
                    } else {
                        self.show_content_results(ui, &mut request);
                    }
                },
            );
        } else {
            let right_width = (size.x * 0.34).clamp(248.0, 372.0);
            let left_width = (size.x - right_width - gap).max(280.0);
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(left_width, body_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_min_size(egui::vec2(left_width, body_height));
                        self.show_content_results(ui, &mut request);
                    },
                );
                ui.add_space(gap);
                ui.allocate_ui_with_layout(
                    egui::vec2(right_width, body_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_min_size(egui::vec2(right_width, body_height));
                        self.show_content_library(ui, &mut request);
                    },
                );
            });
        }
        if request.load_more {
            self.start_mod_search_page(context.clone(), true);
        }
        if request.cancel_modpack {
            self.cancel_modpack_install(context.clone());
        }
        if request.cancel_install {
            self.cancel_mod_install(context.clone());
        }
        if request.clear_queue {
            self.clear_install_queue();
        }
        if request.dismiss_report {
            self.last_install = None;
        }
        if request.copy_list {
            let header = self.selected_instance().map_or_else(
                || self.content_kind.label().to_owned(),
                |instance| {
                    format!(
                        "{} — {} · Minecraft {} · {}",
                        self.content_kind.label(),
                        instance.name,
                        instance.mc_version,
                        instance.loader
                    )
                },
            );
            let kind = self.content_kind;
            let (text, count) = {
                let entries = self.content_names();
                (content_list_text(&header, &entries), entries.len())
            };
            context.output_mut(|output| {
                output.copied_text = text;
            });
            self.set_status(
                StatusKind::Success,
                format!(
                    "Copied {count} {} name(s) to the clipboard.",
                    kind.label().to_ascii_lowercase()
                ),
            );
        }
        if request.reset_filters {
            self.mod_refine.clear();
        }
        if let Some(index) = request.install
            && let Some(item) = self.mod_results.items.get(index).cloned()
        {
            self.queue_mod_install(context.clone(), item);
        }
        if let Some(key) = request.preview {
            self.preview_key = Some(key);
        }
        if request.rescan {
            self.refresh_installed();
            self.set_status(StatusKind::Info, "Content library rescanned.");
        }
        if request.open_folder
            && let Some(instance) = self.selected_instance().cloned()
        {
            self.open_content_folder(self.content_kind, &instance);
        }
        if let Some(path) = request.open_path.take() {
            let result = std::fs::create_dir_all(&path)
                .map_err(|error| error.to_string())
                .and_then(|()| open_instance_path(&path, false));
            if let Err(error) = result {
                self.set_status(
                    StatusKind::Error,
                    format!("Could not open {path:?}: {error}"),
                );
            }
        }
        if std::mem::take(&mut request.add_files) {
            self.pick_content_files(context);
        }
        if request.check_updates {
            self.start_update_check(context.clone());
        }
        if let Some(index) = request.apply_update
            && let Some((_, report)) = &self.update_report
            && let Some(update) = report.updates.get(index).cloned()
        {
            self.apply_mod_update(update, context.clone());
        }
        if request.apply_all_updates {
            let updates = self
                .update_report
                .as_ref()
                .map(|(_, report)| report.updates.clone())
                .unwrap_or_default();
            for update in updates {
                self.apply_mod_update(update, context.clone());
            }
        }
        if request.repair {
            self.repair_mod_environment(context.clone());
        }
        if request.duplicates {
            self.resolve_duplicate_mods(context.clone());
        }
        if let Some(target) = request.erase.take() {
            self.erase_installed_content(&target);
        }
        if let Some(name) = request.arm_delete {
            self.confirm_mod_delete = Some(name);
        }
        if request.cancel_delete {
            self.confirm_mod_delete = None;
        }
        if let Some((action, path, is_dir)) = request.file_action {
            self.confirm_mod_delete = None;
            if matches!(action, ModFileAction::Delete) && is_dir {
                match std::fs::remove_dir_all(&path) {
                    Ok(()) => self.set_status(StatusKind::Success, "Content deleted."),
                    Err(error) => self.set_status(
                        StatusKind::Error,
                        format!("Could not delete content: {error}"),
                    ),
                }
                self.refresh_installed();
            } else {
                self.apply_mod_file_action(action, &path);
            }
        }
        if let Some(source) = import {
            self.manual_url.clear();
            self.manual_path.clear();
            self.start_manual_import(source, context.clone());
        }
    }

    fn show_content_results(&mut self, ui: &mut egui::Ui, request: &mut ContentRequest) {
        let mut install_request: Option<usize> = None;
        let mut preview_request: Option<String> = None;
        let mut erase_request: Option<EraseTarget> = None;
        let visible = visible_mod_indices(
            &self.mod_results.items,
            &self.mod_rows,
            &self.mod_refine,
            ModView {
                sort: self.mod_sort,
                show_modrinth: self.mod_show_modrinth,
                show_curseforge: self.mod_show_curseforge,
            },
        );
        let loaded = self.mod_results.items.len();
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // No caption: the pill's own hint says what it filters, and the
                // row is the first thing under the search bar it belongs to.
                // Capped, not full width: a search field the width of the
                // window reads as a text editor, and the sort control beside it
                // was the only thing it had to share the row with.
                let width = (ui.available_width() - 190.0).clamp(120.0, 320.0);
                search_pill(
                    ui,
                    &mut self.mod_refine,
                    MOD_REFINE_ID,
                    "Filter these results…",
                    width,
                );
                egui::ComboBox::from_id_source("kaelis-mod-sort")
                    .selected_text(self.mod_sort.label())
                    .width(112.0)
                    .show_ui(ui, |ui| {
                        for sort in ModSort::ALL {
                            ui.selectable_value(&mut self.mod_sort, sort, sort.label());
                        }
                    });
            });
        });
        if !self.mod_results.failures.is_empty() {
            Self::card_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                for failure in &self.mod_results.failures {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} unavailable · {}",
                            failure.provider, failure.message
                        ))
                        .size(10.0)
                        .color(StatusKind::Warning.color()),
                    );
                }
                if !self.search_busy && ui.small_button("Retry search").clicked() {
                    request.retry_search = true;
                }
            });
        }
        if visible.is_empty() {
            Self::glass_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                let (title, hint) = if loaded > 0 {
                    (
                        "Nothing matches these filters",
                        "Clear the filter text or switch back to All.".to_owned(),
                    )
                } else if self.search_busy {
                    (
                        "Searching…",
                        if self.content_kind == ContentKind::Modpack {
                            "Asking Modrinth for .mrpack releases.".to_owned()
                        } else {
                            "Asking Modrinth and CurseForge in parallel.".to_owned()
                        },
                    )
                } else {
                    (
                        "Search the whole library",
                        format!(
                            "Type a name, paste a project link, or drop a {} file here.",
                            if self.content_kind == ContentKind::Mod {
                                "mod"
                            } else {
                                self.content_kind.singular()
                            }
                        ),
                    )
                };
                ui.label(egui::RichText::new(title).size(13.0).strong());
                ui.label(egui::RichText::new(hint).size(10.5).color(MUTED));
                if loaded > 0 && ui.small_button("Reset filters").clicked() {
                    request.reset_filters = true;
                }
            });
            return;
        }
        let selected_id = self.selected_instance().map(|instance| instance.id.clone());
        let items = &self.mod_results.items;
        let rows = &self.mod_rows;
        let icons = &self.mod_icons;
        let installed = self.installed.as_ref();
        let content = &self.content_library;
        let installing = self.installing_job.as_ref();
        let states = &self.install_states;
        let queue = &self.install_queue;
        let detail = self.install_detail.as_deref();
        // Six pixels between the filter card and the first result, and no
        // overshoot: a taller-than-available scroll area clips the first row.
        ui.add_space(6.0);
        let list_height = ui.available_height().max(120.0);
        egui::ScrollArea::vertical()
            .id_source("kaelis-content-results")
            .max_height(list_height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for index in visible {
                    let Some(item) = items.get(index) else {
                        continue;
                    };
                    let Some(row) = rows.get(index) else {
                        continue;
                    };
                    let icon = icons.get(&row.key).and_then(|state| match state {
                        ModIconState::Ready(texture) => Some(texture),
                        _ => None,
                    });
                    let running = installing.is_some_and(|job| {
                        selected_id.as_deref() == Some(job.instance_id.as_str())
                            && job.key == row.key
                    });
                    let queue_position = queue.iter().position(|job| {
                        selected_id.as_deref() == Some(job.instance_id.as_str())
                            && job.key == row.key
                    });
                    let queued = queue_position.is_some();
                    let state = selected_id
                        .as_ref()
                        .and_then(|id| states.get(&install_state_key(id, &row.key)));
                    let installed_entry = if item.kind == ContentKind::Mod {
                        installed
                            .and_then(|scan| scan.match_file(&item.slug, &row.match_tokens))
                            .map(|file| (file.name.as_str(), file.disabled))
                    } else {
                        content
                            .match_entry(item.kind, &item.slug, &row.match_tokens)
                            .map(|entry| (entry.name.as_str(), entry.disabled))
                    };
                    let (state_label, state_color) = match state {
                        Some(ModInstallState::Running) => (
                            detail
                                .map(|detail| format!("Installing · {detail}"))
                                .unwrap_or_else(|| "Installing…".to_owned()),
                            accent(),
                        ),
                        Some(ModInstallState::Queued) => (
                            queue_position
                                .map(|position| format!("Queued · position {}", position + 1))
                                .unwrap_or_else(|| "Queued".to_owned()),
                            StatusKind::Warning.color(),
                        ),
                        Some(ModInstallState::Done { summary, warnings }) => (
                            if *warnings == 0 {
                                format!("✓ {summary}")
                            } else {
                                format!("✓ {summary} · {warnings} warning(s)")
                            },
                            if *warnings == 0 {
                                StatusKind::Success.color()
                            } else {
                                StatusKind::Warning.color()
                            },
                        ),
                        Some(ModInstallState::Cancelled) => {
                            ("Stopped".to_owned(), StatusKind::Warning.color())
                        }
                        Some(ModInstallState::Failed(error)) => {
                            (format!("Failed · {error}"), StatusKind::Error.color())
                        }
                        None => (String::new(), MUTED),
                    };
                    let mut clicked = false;
                    let mut preview_clicked = false;
                    Self::list_frame().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_top(|ui| {
                            draw_mod_icon(ui, icon, item);
                            ui.add_space(6.0);
                            // Reserve the real width of Install + Details, at
                            // the compact control width, so long metadata never
                            // runs under the action buttons.
                            let text_width = (ui.available_width() - 150.0).max(120.0);
                            ui.vertical(|ui| {
                                ui.set_max_width(text_width);
                                // Title only. The provider pill and the
                                // "Installed" pill beside it were two boxes per
                                // row that repeated what the action already
                                // says: an Erase means installed, an Install
                                // means not. Both live in the Details preview
                                // for the rare time the source matters.
                                ui.label(egui::RichText::new(&item.title).strong().size(13.5))
                                    .on_hover_text(if item.description.is_empty() {
                                        item.title.clone()
                                    } else {
                                        item.description.clone()
                                    });
                                // Two visible lines per project instead of four:
                                // author, downloads, loader support, and version
                                // share one line, and description/categories are
                                // one hover away. A screenful now holds roughly
                                // twice as many installable projects.
                                let mut meta = format!("{} · {}", row.author_line, row.meta_line);
                                if !row.compatibility_line.is_empty() {
                                    meta.push_str(" · ");
                                    meta.push_str(&row.compatibility_line);
                                }
                                let hover = if row.categories_line.is_empty() {
                                    item.description.clone()
                                } else {
                                    format!("{}\n\n{}", item.description, row.categories_line)
                                };
                                ui.label(egui::RichText::new(meta).size(9.5).color(MUTED))
                                    .on_hover_text(hover);
                                if !state_label.is_empty() {
                                    ui.label(
                                        egui::RichText::new(&state_label)
                                            .size(10.0)
                                            .color(state_color),
                                    );
                                }
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                                // A top-aligned row places a button label four
                                // pixels below its box top, which lifts every
                                // action off the capsule's centre line. Six
                                // pixels of vertical padding centre the ink in
                                // the 26 px control without stretching the row.
                                ui.spacing_mut().button_padding.y = 6.0;
                                // One action per row: Install, or Erase once the
                                // project is present. Erasing flips the row back
                                // to Install on the next scan.
                                if installed_entry.is_some() {
                                    let erase = danger_button(ui, "Erase")
                                        .on_hover_text(format!(
                                            "Delete this {} from the instance right now. No confirmation.",
                                            item.kind.singular()
                                        ));
                                    if erase.clicked() {
                                        erase_request = Some(EraseTarget {
                                            kind: item.kind,
                                            key: row.key.clone(),
                                            slug: item.slug.clone(),
                                            tokens: row.match_tokens.clone(),
                                            title: item.title.clone(),
                                        });
                                    }
                                } else {
                                    let label = if running {
                                        "Installing…"
                                    } else if queued {
                                        "Queued"
                                    } else {
                                        "Install"
                                    };
                                    // Fixed width: "Install", "Queued", and
                                    // "Installing…" must not resize the row on
                                    // every state change. 78 px is the longest
                                    // of the three plus the compact padding.
                                    let button = ui
                                        .add_enabled(
                                            !running && !queued,
                                            egui::Button::new(label)
                                                .min_size(egui::vec2(78.0, 0.0)),
                                        )
                                        .on_disabled_hover_text(
                                            "This install is already in the queue.",
                                        )
                                        .on_hover_text(if item.kind == ContentKind::Modpack {
                                            "Create a new instance from this modpack."
                                        } else {
                                            "Add this content to the instance."
                                        });
                                    if button.clicked() {
                                        clicked = true;
                                    }
                                }
                                if ui
                                    .add(egui::Button::new(
                                        egui::RichText::new("Details").size(10.0).color(MUTED),
                                    )
                                    .frame(false))
                                    .on_hover_text("Preview the project")
                                    .clicked()
                                {
                                    preview_clicked = true;
                                }
                                // Only a live install spins. A finished or
                                // failed row used to keep an animated spinner
                                // next to "✓ …" forever, which also kept the
                                // whole window repainting.
                                if running {
                                    ui.add(egui::Spinner::new().size(12.0));
                                }
                            });
                        });
                    });
                    ui.add_space(2.0);
                    if clicked {
                        install_request = Some(index);
                    }
                    if preview_clicked {
                        preview_request = Some(row.key.clone());
                    }
                }
            });
        let loaded = self.mod_results.items.len();
        let total = self.mod_results.total;
        ui.horizontal(|ui| {
            if self.search_busy {
                ui.add(egui::Spinner::new().size(12.0));
            } else if self.mod_has_more && ui.button("Load more").clicked() {
                request.load_more = true;
            }
            let label = if total > 0 {
                format!("{loaded} of {total} loaded")
            } else {
                format!("{loaded} loaded")
            };
            ui.label(egui::RichText::new(label).size(9.5).color(MUTED));
            if self.mod_has_more && !self.search_busy {
                ui.label(
                    egui::RichText::new("· more results available")
                        .size(9.5)
                        .color(MUTED),
                );
            }
        });
        if let Some(index) = install_request {
            request.install = Some(index);
        }
        if let Some(key) = preview_request {
            request.preview = Some(key);
        }
        if let Some(target) = erase_request {
            request.erase = Some(target);
        }
    }

    /// The origin of an install is its project page; keep that one click away.
    /// Modpacks are instances, so the right column lists real instances with
    /// their game version and loader instead of scanning a folder.
    fn show_modpack_library(&mut self, ui: &mut egui::Ui) {
        let mut manage: Option<String> = None;
        let mut erase: Option<String> = None;
        let instances = self.instances.clone();
        let selected = self.selected_instance.clone();
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("INSTALLED · MODPACKS")
                        .size(9.0)
                        .strong()
                        .color(MUTED),
                );
                chip(ui, &instances.len().to_string(), kind_color(ContentKind::Modpack));
            });
            ui.label(
                egui::RichText::new("Each pack is its own instance, with its mods, configs, and loader.")
                    .size(9.5)
                    .color(MUTED),
            );
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .id_source("kaelis-modpack-installed")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    if instances.is_empty() {
                        ui.label(
                            egui::RichText::new("No instances yet. Search a pack on the left to create one.")
                                .size(10.0)
                                .color(MUTED),
                        );
                        return;
                    }
                    for instance in &instances {
                        let active = selected.as_deref() == Some(instance.id.as_str());
                        Self::card_frame().show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.vertical(|ui| {
                                    let name = egui::RichText::new(&instance.name)
                                        .strong()
                                        .size(12.0)
                                        .color(if active { accent_soft() } else { TEXT });
                                    ui.label(name);
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "MC {} · {}",
                                            instance.mc_version, instance.loader
                                        ))
                                        .size(9.5)
                                        .color(MUTED),
                                    );
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if danger_button(ui, "Erase")
                                            .on_hover_text(
                                                "Delete this instance and every file in it. No confirmation.",
                                            )
                                            .clicked()
                                        {
                                            erase = Some(instance.id.clone());
                                        }
                                        if ui.small_button("Manage").clicked() {
                                            manage = Some(instance.id.clone());
                                        }
                                    },
                                );
                            });
                        });
                        ui.add_space(4.0);
                    }
                });
        });
        if let Some(id) = manage {
            self.selected_instance = Some(id.clone());
            self.current_tab = Tab::Instances;
            self.refresh_installed();
        }
        if let Some(id) = erase {
            self.erase_instance(&id);
        }
    }

    fn show_content_library(&mut self, ui: &mut egui::Ui, request: &mut ContentRequest) {
        let kind = self.content_kind;
        if kind == ContentKind::Modpack {
            self.show_modpack_library(ui);
            return;
        }
        if self.selected_instance().is_none() {
            Self::glass_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    egui::RichText::new("Select an instance to see what is installed.")
                        .size(10.5)
                        .color(MUTED),
                );
            });
            return;
        }
        let mut filter = std::mem::take(&mut self.installed_filter);
        let mut missing_only = self.installed_missing_only;
        let confirm = &self.confirm_mod_delete;
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let total = if kind == ContentKind::Mod {
                self.installed.as_ref().map_or(0, |scan| scan.files.len())
            } else {
                self.content_library.entries(kind).len()
            };
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("INSTALLED · {}", kind.label().to_uppercase()))
                        .size(9.0)
                        .strong()
                        .color(MUTED),
                );
                chip(ui, &total.to_string(), kind_color(kind));
                if self.repairing_mods {
                    ui.add(egui::Spinner::new().size(12.0));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if total > 0
                        && ui
                            .small_button("Copy")
                            .on_hover_text("Copy every name, with disabled items marked.")
                            .clicked()
                    {
                        request.copy_list = true;
                    }
                    if ui
                        .small_button("Add files…")
                        .on_hover_text("Add files to this folder from the file manager.")
                        .clicked()
                    {
                        request.add_files = true;
                    }
                });
            });
            if kind == ContentKind::Mod {
                let current = self.selected_instance.as_deref();
                if let Some((report_id, report)) = self.update_report.as_ref()
                    && Some(report_id.as_str()) == current
                {
                    ui.add_space(2.0);
                    ui.horizontal_wrapped(|ui| {
                        if report.updates.is_empty() {
                            chip(ui, "up to date", StatusKind::Success.color());
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} current · {} not on Modrinth{}",
                                    report.up_to_date,
                                    report.unmatched,
                                    if report.skipped > 0 {
                                        format!(" · {} skipped", report.skipped)
                                    } else {
                                        String::new()
                                    }
                                ))
                                .size(9.0)
                                .color(MUTED),
                            );
                        } else {
                            chip(
                                ui,
                                &format!(
                                    "{} update{}",
                                    report.updates.len(),
                                    if report.updates.len() == 1 { "" } else { "s" }
                                ),
                                accent(),
                            );
                            if self.updating_mods.is_empty()
                                && ui
                                    .small_button("Update all")
                                    .on_hover_text(
                                        "Download and verify every update, keeping disabled jars disabled.",
                                    )
                                    .clicked()
                            {
                                request.apply_all_updates = true;
                            }
                        }
                    });
                    for (index, update) in report.updates.iter().enumerate() {
                        ui.horizontal(|ui| {
                            // Two lines inside a bounded column: the right pane
                            // is narrow, so a single line of title + file names
                            // + button used to run past the window edge.
                            let button_width = 76.0;
                            let width = (ui.available_width() - button_width).max(60.0);
                            ui.allocate_ui_with_layout(
                                egui::vec2(width, 30.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.label(
                                        egui::RichText::new(&update.title)
                                            .size(10.5)
                                            .strong(),
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(format!(
                                                "{} → {}{}",
                                                update.current_file,
                                                update.latest_version,
                                                if update.channel == "release" {
                                                    String::new()
                                                } else {
                                                    format!(" · {}", update.channel)
                                                }
                                            ))
                                            .size(9.0)
                                            .color(MUTED),
                                        )
                                        .truncate(),
                                    );
                                },
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let busy = self.updating_mods.contains(&update.project_id);
                                    if ui
                                        .add_enabled(
                                            !busy,
                                            egui::Button::new(if busy {
                                                "Updating…"
                                            } else {
                                                "Update"
                                            })
                                            .small(),
                                        )
                                        .clicked()
                                    {
                                        request.apply_update = Some(index);
                                    }
                                },
                            );
                        });
                    }
                } else if self.updates_busy {
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(12.0));
                        ui.label(
                            egui::RichText::new("Checking Modrinth for newer versions…")
                                .size(9.5)
                                .color(MUTED),
                        );
                    });
                }
            }
            // The filter and the update check share one row: both act on the
            // installed list, and a separate header button squeezed the title
            // out at the default window width.
            let mut check_updates = false;
            ui.horizontal(|ui| {
                // Reserve the button's real width first; the field takes what
                // is left. Reserving a guessed width clipped the button at the
                // default window size.
                let button_width = if kind == ContentKind::Mod { 82.0 } else { 0.0 };
                let width = (ui.available_width() - button_width - 18.0).clamp(80.0, 300.0);
                search_pill(
                    ui,
                    &mut filter,
                    INSTALLED_FILTER_ID,
                    "Filter installed…",
                    width,
                );
                if kind == ContentKind::Mod
                    && self.selected_instance.is_some()
                    && ui
                        .add_enabled(
                            !self.updates_busy,
                            egui::Button::new(if self.updates_busy {
                                "Checking…"
                            } else {
                                "Updates"
                            })
                            .small(),
                        )
                        .on_hover_text(
                            "Hash installed jars and ask Modrinth for the newest compatible version of each.",
                        )
                        .clicked()
                {
                    check_updates = true;
                }
            });
            if check_updates {
                request.check_updates = true;
            }
            if kind == ContentKind::Mod
                && let Some(scan) = self.installed.as_ref()
            {
                let missing = scan.missing.len();
                let conflicts = scan.conflicts.len();
                if missing > 0 || conflicts > 0 {
                    ui.add_space(2.0);
                    ui.horizontal_wrapped(|ui| {
                        if missing > 0 {
                            chip(
                                ui,
                                &format!("{missing} missing"),
                                StatusKind::Warning.color(),
                            );
                            if ui
                                .small_button("Repair")
                                .on_hover_text("Install every missing required mod.")
                                .clicked()
                            {
                                request.repair = true;
                            }
                        }
                        if conflicts > 0 {
                            chip(
                                ui,
                                &format!("{conflicts} duplicate"),
                                StatusKind::Error.color(),
                            );
                            if ui
                                .small_button("Resolve")
                                .on_hover_text("Disable the older copy of each duplicate.")
                                .clicked()
                            {
                                request.duplicates = true;
                            }
                        }
                        ui.checkbox(&mut missing_only, "Only missing");
                    });
                }
            }
            ui.add_space(2.0);
            egui::ScrollArea::vertical()
                .id_source("kaelis-content-installed")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let tokens = filter_tokens(&filter);
                    // The jar scan runs on a worker, so an empty pane here
                    // means "in progress", not "nothing installed".
                    let Some(scan) = self.installed.as_ref() else {
                        ui.horizontal(|ui| {
                            ui.add(egui::Spinner::new().size(12.0));
                            ui.label(
                                egui::RichText::new("Scanning the instance…")
                                    .size(10.0)
                                    .color(MUTED),
                            );
                        });
                        return;
                    };
                    if kind == ContentKind::Mod {
                        let mut shown = 0_usize;
                        for file in &scan.files {
                            if !tokens.is_empty()
                                && !tokens.iter().all(|token| {
                                    file.name.to_ascii_lowercase().contains(token.as_str())
                                        || file.ids.iter().any(|id| id.contains(token.as_str()))
                                })
                            {
                                continue;
                            }
                            if missing_only
                                && !file.requires.iter().any(|id| scan.missing.contains(id))
                            {
                                continue;
                            }
                            shown += 1;
                            installed_row(
                                ui,
                                InstalledRow {
                                    name: &file.name,
                                    detail: &file.path.display().to_string(),
                                    path: &file.path,
                                    disabled: file.disabled,
                                    is_dir: false,
                                },
                                confirm,
                                request,
                            );
                        }
                        if shown == 0 {
                            ui.label(
                                egui::RichText::new(if scan.files.is_empty() {
                                    "No mods yet. Search on the left, or drop a .jar here."
                                } else {
                                    "No installed jar matches this filter."
                                })
                                .size(10.0)
                                .color(MUTED),
                            );
                        }
                    } else {
                        let entries = self.content_library.entries(kind);
                        let mut shown = 0_usize;
                        for entry in entries {
                            if !tokens.is_empty()
                                && !tokens
                                    .iter()
                                    .all(|token| entry.search_text.contains(token.as_str()))
                            {
                                continue;
                            }
                            shown += 1;
                            installed_row(
                                ui,
                                InstalledRow {
                                    name: &entry.name,
                                    detail: &format!("{} · {}", entry.detail, entry.path.display()),
                                    path: &entry.path,
                                    disabled: entry.disabled,
                                    is_dir: false,
                                },
                                confirm,
                                request,
                            );
                        }
                        if shown == 0 {
                            ui.label(
                                egui::RichText::new(format!(
                                    "No {} yet. Search on the left, or drop a file here.",
                                    kind.label().to_ascii_lowercase()
                                ))
                                .size(10.0)
                                .color(MUTED),
                            );
                        }
                    }
                });
        });
        self.installed_filter = filter;
        self.installed_missing_only = missing_only;
    }

    /// Names of everything installed on the selected instance, split into the
    /// channel the user is looking at. Used by the clipboard export.
    fn content_names(&self) -> Vec<(&str, bool)> {
        match self.content_kind {
            ContentKind::Mod => self.installed.as_ref().map_or_else(Vec::new, |scan| {
                scan.files
                    .iter()
                    .map(|file| (file.name.as_str(), file.disabled))
                    .collect()
            }),
            ContentKind::Modpack => self
                .instances
                .iter()
                .map(|instance| (instance.name.as_str(), false))
                .collect(),
            kind => self
                .content_library
                .entries(kind)
                .iter()
                .map(|entry| (entry.name.as_str(), entry.disabled))
                .collect(),
        }
    }

    fn reset_browse(&mut self) {
        self.mod_results = ModSearchResults::default();
        self.mod_rows.clear();
        self.mod_results_query.clear();
        self.mod_icons.clear();
        self.mod_last_request.clear();
        self.mod_pending_signature.clear();
        self.mod_signature_changed_at = Some(std::time::Instant::now());
        self.mod_pages = 0;
        self.mod_has_more = false;
        self.preview_key = None;
    }

    fn show_content_preview(&mut self, context: &egui::Context) {
        if self.preview_key.is_some() && context.input(|input| input.key_pressed(egui::Key::Escape))
        {
            self.preview_key = None;
            return;
        }
        let Some(key) = self.preview_key.clone() else {
            return;
        };
        let Some(index) = self.mod_rows.iter().position(|row| row.key == key) else {
            self.preview_key = None;
            return;
        };
        let Some(item) = self.mod_results.items.get(index).cloned() else {
            self.preview_key = None;
            return;
        };
        let row = self.mod_rows[index].clone();
        let icon = self.mod_icons.get(&key).and_then(|state| match state {
            ModIconState::Ready(texture) => Some(texture.clone()),
            _ => None,
        });
        let installed = if item.kind == ContentKind::Mod {
            self.installed
                .as_ref()
                .is_some_and(|scan| scan.match_file(&item.slug, &row.match_tokens).is_some())
        } else {
            self.content_library
                .match_entry(item.kind, &item.slug, &row.match_tokens)
                .is_some()
        };
        let mut install = false;
        let mut erase = false;
        let mut close = false;
        let mut copy = false;
        egui::Window::new("Project preview")
            .id(egui::Id::new("kaelis-content-preview"))
            .collapsible(false)
            .resizable(false)
            .default_width(390.0)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-22.0, 66.0))
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    draw_project_icon(ui, icon.as_ref(), &item, 78.0);
                    ui.add_space(10.0);
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&item.title).size(20.0).strong());
                        ui.label(
                            egui::RichText::new(&row.author_line)
                                .size(10.5)
                                .color(accent_soft()),
                        );
                        ui.horizontal(|ui| {
                            chip(ui, row.kind.label(), kind_color(row.kind));
                            chip(ui, &item.provider.to_string(), MUTED);
                            if installed {
                                chip(ui, "Installed", StatusKind::Success.color());
                            }
                        });
                    });
                });
                ui.add_space(10.0);
                ui.label(egui::RichText::new(&item.description).size(11.5));
                ui.add_space(8.0);
                ui.label(egui::RichText::new(&row.meta_line).size(10.0).color(MUTED));
                if !row.categories_line.is_empty() {
                    ui.label(
                        egui::RichText::new(&row.categories_line)
                            .size(10.0)
                            .color(MUTED),
                    );
                }
                ui.add_space(8.0);
                egui::Frame::none()
                    .fill(SURFACE_ALT)
                    .rounding(egui::Rounding::same(CORNER_MD))
                    .inner_margin(egui::Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            egui::RichText::new("AT A GLANCE")
                                .size(9.0)
                                .strong()
                                .color(MUTED),
                        );
                        ui.label(egui::RichText::new(&row.compatibility_line).size(10.5));
                        if let Some(date) = short_date(&item.updated) {
                            ui.label(
                                egui::RichText::new(format!("Last updated {date}"))
                                    .size(10.0)
                                    .color(MUTED),
                            );
                        }
                        if !item.client_side.is_empty() {
                            ui.label(
                                egui::RichText::new(format!(
                                    "Client: {} · Server: {}",
                                    if item.client_side == "unspecified" {
                                        "universal"
                                    } else {
                                        &item.client_side
                                    },
                                    if item.server_side == "unspecified" {
                                        "universal"
                                    } else {
                                        &item.server_side
                                    }
                                ))
                                .size(10.0)
                                .color(MUTED),
                            );
                        }
                    });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if installed {
                        // An installed project has exactly one action: erase it
                        // now. The row flips back to Install afterwards.
                        if danger_button(ui, "Erase")
                            .on_hover_text(format!(
                                "Delete this {} from the instance right now. No confirmation.",
                                item.kind.singular()
                            ))
                            .clicked()
                        {
                            erase = true;
                        }
                    } else if primary_button(ui, "Install").clicked() {
                        install = true;
                    }
                    if ui.button("Project page").clicked() {
                        context.open_url(egui::OpenUrl::new_tab(row.project_url.clone()));
                    }
                    if ui.small_button("Copy link").clicked() {
                        copy = true;
                    }
                });
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(if item.kind == ContentKind::Modpack {
                        format!(
                            "Creates an instance with this pack's version, loader, and mods. {} projects; nothing stored.",
                            item.provider
                        )
                    } else {
                        format!(
                            "Installs into {} for this instance. {} project metadata shown; no history is stored.",
                            item.kind.directory(),
                            item.provider
                        )
                    })
                    .size(9.5)
                    .color(MUTED),
                );
                ui.horizontal(|ui| {
                    if ui.small_button("Close preview").clicked() {
                        close = true;
                    }
                });
            });
        if copy {
            context.copy_text(row.project_url.clone());
        }
        if erase {
            let target = EraseTarget {
                kind: item.kind,
                key,
                slug: item.slug.clone(),
                tokens: row.match_tokens.clone(),
                title: item.title.clone(),
            };
            self.erase_installed_content(&target);
        } else if install {
            self.preview_key = None;
            self.queue_mod_install(context.clone(), item);
        } else if close || context.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.preview_key = None;
        }
    }

    fn open_content_folder(&mut self, kind: ContentKind, instance: &Instance) {
        // A pack is an instance; its content lives across the whole folder.
        let path = if kind == ContentKind::Modpack {
            Instance::instances_dir()
        } else {
            kind.install_dir(&instance.root)
        };
        let result = std::fs::create_dir_all(&path)
            .map_err(|error| error.to_string())
            .and_then(|()| open_instance_path(&path, false));
        if let Err(error) = result {
            self.set_status(
                StatusKind::Error,
                format!("Could not open the {} folder: {error}", kind.label()),
            );
        }
    }

    /// Accounts, in one screen: the active identity, every saved row with its
    /// skin head, and the two ways to add another. Rows are the only place an
    /// identity can be switched or removed, so nothing is duplicated.
    fn show_account(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.y = 5.0;
        // A live session is the source of truth. If a completion event was
        // missed, the list was overwritten by an import, or the settings file
        // was edited by hand, the signed-in identity still gets its row the
        // moment this tab is shown instead of vanishing from the list.
        if let Some(session) = &self.microsoft_session {
            let name = session.profile.name.clone();
            let uuid = session.profile.id.clone();
            let known = self.accounts.iter().any(|account| {
                account.kind == AccountKind::Microsoft && account.name.eq_ignore_ascii_case(&name)
            });
            if !known {
                self.upsert_account(AccountKind::Microsoft, &name, &uuid);
            }
        }
        self.ensure_account_heads(context);
        self.ensure_steve_texture(context);

        let signed_in = self.microsoft_session.is_some() || self.offline_account.is_some();
        let online = self.microsoft_session.is_some();
        let active_uuid = self
            .microsoft_session
            .as_ref()
            .map(|session| session.profile.id.clone())
            .or_else(|| {
                self.offline_account
                    .as_ref()
                    .map(|profile| profile.uuid.clone())
            })
            .unwrap_or_default();

        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 9.0;
                let size = 46.0;
                match &self.avatar {
                    Some(avatar) => {
                        ui.add(
                            egui::Image::new((avatar.id(), egui::vec2(size, size)))
                                .rounding(egui::Rounding::same(CORNER_MD)),
                        );
                    }
                    None => {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
                        ui.painter().rect_filled(rect, CORNER_MD, GLASS_LIGHT);
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            if signed_in {
                                self.account_name().chars().next().unwrap_or('K')
                            } else {
                                'K'
                            },
                            egui::FontId::proportional(21.0),
                            accent(),
                        );
                    }
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(
                        egui::RichText::new(if signed_in {
                            self.account_name()
                        } else {
                            "Guest session".to_owned()
                        })
                        .size(17.0)
                        .strong(),
                    );
                    let kind = if online {
                        "Microsoft · online ready"
                    } else if signed_in {
                        "Offline · local only"
                    } else {
                        "Pick an account below"
                    };
                    ui.label(
                        egui::RichText::new(kind)
                            .size(9.5)
                            .color(if online { StatusKind::Success.color() } else { MUTED }),
                    );
                    if !active_uuid.is_empty() {
                        ui.label(egui::RichText::new(&active_uuid).size(9.0).color(MUTED));
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if signed_in
                        && ui
                            .small_button("Sign out")
                            .on_hover_text(
                                "Removes this saved identity. A Microsoft row also deletes its keychain token.",
                            )
                            .clicked()
                    {
                        self.forget_account(context.clone());
                    }
                    if !active_uuid.is_empty() && ui.small_button("Copy UUID").clicked() {
                        ui.output_mut(|output| {
                            output.copied_text = active_uuid.clone();
                        });
                    }
                });
            });
        });

        let mut activate = None;
        let mut remove = None;
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                section_label(ui, "ACCOUNTS");
                ui.label(
                    egui::RichText::new(format!("{}", self.accounts.len()))
                        .size(9.0)
                        .color(MUTED),
                );

            });
            if self.accounts.is_empty() {
                ui.label(
                    egui::RichText::new(
                        "No saved identities yet. Add an offline name or sign in below.",
                    )
                    .size(10.0)
                    .color(MUTED),
                );
                return;
            }
            for index in 0..self.accounts.len() {
                let account = self.accounts[index].clone();
                let key = account.key();
                let active = self.active_identity_matches(&account);
                // A resolved premium head wins. Everything else — still
                // loading, or a name with no skin — falls back to the vanilla
                // face, so a row never flashes a letter while it fetches.
                let face = match self.account_heads.get(&key) {
                    Some(Some(texture)) => Some(texture.id()),
                    _ => self.steve_head_texture.as_ref().map(|steve| steve.id()),
                };
                let short_id = if account.uuid.len() >= 8 {
                    format!("{}…", &account.uuid[..8])
                } else if account.uuid.is_empty() {
                    "no profile id yet".to_owned()
                } else {
                    account.uuid.clone()
                };
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 7.0;
                    match face {
                        Some(texture) => {
                            ui.add(
                                egui::Image::new((texture, egui::vec2(26.0, 26.0)))
                                    .rounding(egui::Rounding::same(CORNER_SM)),
                            );
                        }
                        None => {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(26.0, 26.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, CORNER_SM, GLASS_LIGHT);
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                account.name.chars().next().unwrap_or('K'),
                                egui::FontId::proportional(13.0),
                                MUTED,
                            );
                        }
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(egui::RichText::new(&account.name).size(12.5).strong());
                            if active {
                                dot_label(ui, "in use", accent_soft());
                            }
                        });
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {short_id}",
                                account.kind.label()
                            ))
                            .size(9.0)
                            .color(MUTED),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // The active row is removed with the header's Sign out,
                        // so the list itself never offers a redundant action.
                        if !active {
                            if ui
                                .small_button("Remove")
                                .on_hover_text(
                                    "Deletes this row. A Microsoft row also deletes its saved token.",
                                )
                                .clicked()
                            {
                                remove = Some(index);
                            }
                            if ui.small_button("Use").clicked() {
                                activate = Some(index);
                            }
                        }
                    });
                });
            }
        });
        if let Some(index) = remove {
            self.remove_account(index, context);
        } else if let Some(index) = activate {
            self.activate_account(index, context);
        }

        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_label(ui, "ADD ACCOUNT");
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                self.refresh_offline_typing_avatar(context);
                if let Some(texture) = &self.offline_typing_avatar {
                    ui.add(
                        egui::Image::new((texture.id(), egui::vec2(24.0, 24.0)))
                            .rounding(egui::Rounding::same(CORNER_SM)),
                    )
                    .on_hover_text("Premium head for this name, or Steve when it is unclaimed.");
                }
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.offline_name)
                        .hint_text("Username")
                        .desired_width(150.0),
                );
                if edit.changed() {
                    if valid_username(self.offline_name.trim()) {
                        self.offline_skin_due = Some(
                            std::time::Instant::now() + std::time::Duration::from_millis(350),
                        );
                        context.request_repaint_after(std::time::Duration::from_millis(350));
                    } else {
                        self.offline_skin_due = None;
                    }
                }
                if ui
                    .add_enabled(!self.auth_busy, egui::Button::new("Add offline"))
                    .on_hover_text("Local name and vanilla OfflinePlayer UUID. Online servers reject it.")
                    .clicked()
                {
                    self.sign_in_offline(context.clone());
                }
                ui.separator();
                // The marquee action: accent fill, same height and radius as
                // every other control in the row.
                let sign_in = primary_button(ui, "Sign in with Microsoft")
                    .on_hover_text(
                        "Shows a code, opens the Microsoft page, and finishes once you approve.",
                    );
                if sign_in.clicked() && !self.auth_busy {
                    self.start_sign_in(context.clone());
                }
                if self.auth_busy && ui.button("Cancel").clicked() {
                    if let Some(cancel) = self.auth_cancel.take() {
                        let _ = cancel.send(true);
                    }
                    self.auth_busy = false;
                    self.auth_device = None;
                    self.auth_status = "Sign-in cancelled.".to_owned();
                }
            });
            // Import is its own row: it starts under the name field, not in
            // the avatar column, and a single leading label names it.
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                // Line the row up under the name field, past the head.
                ui.add_space(31.0);
                ui.label(
                    egui::RichText::new("IMPORT")
                        .size(9.0)
                        .strong()
                        .color(MUTED),
                );
                if ui
                    .button("Scan for launchers")
                    .on_hover_text(
                        "Reads Minecraft Launcher, Prism, MultiMC, and PolyMC account files on this computer.",
                    )
                    .clicked()
                {
                    self.scan_for_accounts(context, true);
                }
                if ui
                    .button("Import a file…")
                    .on_hover_text("Any JSON with player records works, not only a known launcher file.")
                    .clicked()
                {
                    self.pick_account_file(context);
                }
            });
            ui.label(
                egui::RichText::new(&self.auth_status)
                    .size(9.5)
                    .color(MUTED),
            );
            if let Some(device) = &self.auth_device {
                ui.add_space(2.0);
                egui::Frame::none()
                    .fill(GLASS_LIGHT)
                    .stroke(egui::Stroke::new(1.0_f32, BORDER_BRIGHT))
                    .rounding(egui::Rounding::same(CORNER_MD))
                    .inner_margin(egui::Margin::symmetric(12.0, 10.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 14.0;
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = 3.0;
                                ui.label(
                                    egui::RichText::new("ENTER THIS CODE")
                                        .size(8.5)
                                        .strong()
                                        .color(MUTED),
                                );
                                ui.label(
                                    egui::RichText::new(&device.user_code)
                                        .size(30.0)
                                        .strong()
                                        .color(accent()),
                                );
                                let uri = device
                                    .verification_uri_complete
                                    .clone()
                                    .unwrap_or_else(|| device.verification_uri.clone());
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 6.0;
                                    if primary_button(ui, "Open Microsoft page").clicked() {
                                        context.open_url(egui::OpenUrl::new_tab(uri.clone()));
                                    }
                                    let copy = if self.auth_code_copied {
                                        "Copied"
                                    } else {
                                        "Copy code"
                                    };
                                    if outline_button(ui, copy).clicked() {
                                        context.copy_text(device.user_code.clone());
                                        self.auth_code_copied = true;
                                    }
                                });
                                let remaining = self
                                    .auth_device_until
                                    .map(|until| {
                                        until.saturating_duration_since(std::time::Instant::now())
                                    })
                                    .unwrap_or_default();
                                let expired = remaining.is_zero();
                                ui.label(
                                    egui::RichText::new(if expired {
                                        "This code expired. Start sign-in again.".to_owned()
                                    } else {
                                        format!(
                                            "Expires in {:02}:{:02} · this window finishes automatically",
                                            remaining.as_secs() / 60,
                                            remaining.as_secs() % 60
                                        )
                                    })
                                    .size(9.0)
                                    .color(if expired {
                                        StatusKind::Warning.color()
                                    } else {
                                        MUTED
                                    }),
                                );
                                if let Some(message) = &device.message {
                                    ui.label(egui::RichText::new(message).size(9.0).weak());
                                }
                            });
                            // A fixed box keeps the code column and the QR
                            // side by side instead of letting the layout
                            // stretch the QR into the middle of the panel.
                            ui.allocate_ui(egui::vec2(140.0, 168.0), |ui| {
                                ui.with_layout(
                                    egui::Layout::top_down(egui::Align::Center),
                                    |ui| {
                                        show_qr(ui, device);
                                        ui.label(
                                            egui::RichText::new("Scan with your phone")
                                                .size(8.5)
                                                .color(MUTED),
                                        );
                                    },
                                );
                            });
                        });
                    });
                // A visible countdown only needs one repaint per second.
                context.request_repaint_after(std::time::Duration::from_secs(1));
            }
        });
    }

    /// The command list. Every rule is one visible line: what it is called,
    /// when it runs, and the exact shell string. Nothing is hidden behind a
    /// second window, so the whole automation is readable at a glance.
    fn show_automation_commands(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        section_label(ui, "COMMANDS");
        ui.label(
            egui::RichText::new(
                "A command runs in the instance folder. When: on demand, at start, \
                 before launch, after exit, or every 15m. Edit automation.json directly \
                 if you prefer a text editor.",
            )
            .size(9.5)
            .color(MUTED),
        );
        let mut run: Option<Rule> = None;
        let mut edit: Option<Rule> = None;
        let mut delete: Option<String> = None;
        let mut toggle: Option<(String, bool)> = None;
        for rule in self.automation.rules().to_vec() {
            ui.horizontal(|ui| {
                let mut enabled = rule.enabled;
                if ui
                    .checkbox(&mut enabled, "")
                    .on_hover_text("Run this command automatically.")
                    .changed()
                {
                    toggle = Some((rule.id.clone(), enabled));
                }
                ui.label(egui::RichText::new(rule.title()).size(10.5));
                chip(ui, &rule.trigger.label(), MUTED);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Delete").clicked() {
                        delete = Some(rule.id.clone());
                    }
                    if ui.small_button("Edit").clicked() {
                        edit = Some(rule.clone());
                    }
                    if ui.small_button("Run").clicked() {
                        run = Some(rule.clone());
                    }
                });
            });
        }
        if self.automation.rules().is_empty() {
            ui.label(
                egui::RichText::new("No commands yet.")
                    .size(10.0)
                    .color(MUTED),
            );
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.automation_draft_name)
                    .hint_text("Name (optional)")
                    .desired_width(132.0),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.automation_draft_when)
                    .hint_text("when")
                    .desired_width(96.0),
            );
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.automation_draft_command)
                .hint_text("Command — for example ./backup.sh or dir mods")
                .desired_width(f32::INFINITY),
        );
        ui.horizontal(|ui| {
            let label = if self.automation_editing.is_some() {
                "Save"
            } else {
                "Add command"
            };
            if ui.button(label).clicked() {
                self.commit_automation_draft();
            }
            if self.automation_editing.is_some() && ui.button("Cancel").clicked() {
                self.reset_automation_draft();
            }
        });
        for line in self.automation_log.iter().rev().take(3) {
            ui.add(egui::Label::new(egui::RichText::new(line).size(9.0).color(MUTED)).wrap());
        }
        if let Some((id, enabled)) = toggle
            && let Some(rule) = self
                .automation
                .rules_mut()
                .iter_mut()
                .find(|rule| rule.id == id)
        {
            rule.enabled = enabled;
            if let Err(error) = self.automation.save() {
                self.set_status(StatusKind::Error, error.to_string());
            }
        }
        if let Some(id) = delete
            && self.automation.remove(&id)
        {
            match self.automation.save() {
                Ok(()) => self.set_status(StatusKind::Info, "Command removed."),
                Err(error) => self.set_status(StatusKind::Error, error.to_string()),
            }
        }
        if let Some(rule) = edit {
            self.automation_editing = Some(rule.id.clone());
            self.automation_draft_name = rule.name.clone();
            self.automation_draft_command = rule.command.clone();
            self.automation_draft_when = rule.trigger.label();
        }
        if let Some(rule) = run {
            let context = ui.ctx().clone();
            self.spawn_automation(rule, &context);
        }
    }

    /// Validate and store the draft. The same checks run for a typed rule and
    /// for one loaded from disk, so a hand edit cannot smuggle in an interval
    /// that fires every second.
    fn commit_automation_draft(&mut self) {
        let Some(trigger) = Trigger::parse(&self.automation_draft_when) else {
            self.set_status(
                StatusKind::Warning,
                "Use on demand, at start, before launch, after exit, or every 15m.",
            );
            return;
        };
        let id = self
            .automation_editing
            .clone()
            .unwrap_or_else(|| format!("rule-{}-{}", unix_now(), self.automation.rules().len()));
        let previous = self.automation.rule(&id).map(|rule| rule.last_run_unix);
        let mut rule = Rule::new(
            &id,
            self.automation_draft_name.trim(),
            self.automation_draft_command.trim(),
            trigger,
        );
        rule.last_run_unix = previous.unwrap_or(0);
        if let Some(existing) = self.automation.rule(&id) {
            rule.enabled = existing.enabled;
            rule.timeout_secs = existing.timeout_secs;
        }
        if let Err(error) = self.automation.upsert(rule) {
            self.set_status(StatusKind::Error, error.to_string());
            return;
        }
        match self.automation.save() {
            Ok(()) => {
                let text = if self.automation_editing.is_some() {
                    "Command saved."
                } else {
                    "Command added."
                };
                self.reset_automation_draft();
                self.set_status(StatusKind::Success, text);
            }
            Err(error) => self.set_status(StatusKind::Error, error.to_string()),
        }
    }

    fn reset_automation_draft(&mut self) {
        self.automation_editing = None;
        self.automation_draft_name.clear();
        self.automation_draft_command.clear();
        self.automation_draft_when = "before launch".to_owned();
    }

    /// Copy the selected instance record as JSON, ready for a chat message, a
    /// ticket, or another machine running Kaelis.
    fn copy_instance_json(&mut self, context: &egui::Context) {
        let Some(instance) = self.selected_instance().cloned() else {
            self.set_status(StatusKind::Warning, "Select an instance first.");
            return;
        };
        match serde_json::to_string_pretty(&instance) {
            Ok(text) => {
                context.copy_text(text);
                self.set_status(
                    StatusKind::Success,
                    format!("{} copied as JSON.", instance.name),
                );
            }
            Err(error) => self.set_status(
                StatusKind::Error,
                format!("Could not encode the instance: {error}"),
            ),
        }
    }

    /// Add an instance from clipboard JSON. This copies settings only: mods,
    /// saves, and worlds stay where they are, so pasting never pulls a large
    /// folder through the clipboard.
    fn paste_instance_json(&mut self) {
        let text = match clipboard_text() {
            Ok(text) => text,
            Err(message) => {
                self.set_status(StatusKind::Warning, message);
                return;
            }
        };
        let Some(decoded) = parse_instance_json(&text) else {
            self.set_status(
                StatusKind::Error,
                "Clipboard text is not an instance JSON record.",
            );
            return;
        };
        let name = decoded.name.trim().to_owned();
        if name.is_empty() || name.chars().count() > 64 {
            self.set_status(
                StatusKind::Error,
                "The pasted instance needs a name of 64 characters or fewer.",
            );
            return;
        }
        if !valid_instance_version(&decoded.mc_version) {
            self.set_status(
                StatusKind::Error,
                "The pasted instance has an invalid Minecraft version.",
            );
            return;
        }
        if decoded.loader.trim().is_empty() {
            self.set_status(StatusKind::Error, "The pasted instance has no loader.");
            return;
        }
        let mut unique = name.clone();
        let mut counter = 2;
        while self
            .instances
            .iter()
            .any(|item| item.name.eq_ignore_ascii_case(&unique))
        {
            unique = format!("{name} {counter}");
            counter += 1;
        }
        let mut id = format!("paste-{}-{}", unix_now(), self.instances.len());
        let mut bump = 0;
        while Instance::instances_dir().join(&id).exists() {
            bump += 1;
            id = format!("paste-{}-{bump}", unix_now());
        }
        let root = Instance::instances_dir().join(&id);
        if let Err(error) = std::fs::create_dir_all(&root) {
            self.set_status(
                StatusKind::Error,
                format!("Could not create the instance folder: {error}"),
            );
            return;
        }
        let mut instance = decoded;
        instance.id = id.clone();
        instance.name = unique.clone();
        instance.root = root;
        instance.last_played_unix = 0;
        // Artwork from another machine points at a path that does not exist
        // here; the variant monogram is a better default than a broken image.
        if instance.icon.as_ref().is_some_and(|path| !path.is_file()) {
            instance.icon = None;
        }
        match instance.save() {
            Ok(_) => {
                self.instances.insert(0, instance.clone());
                self.refresh_instance_cards();
                self.select_instance(&id);
                self.set_status(
                    StatusKind::Success,
                    format!("{unique} added from the clipboard."),
                );
            }
            Err(error) => self.set_status(
                StatusKind::Error,
                format!("Could not save the pasted instance: {error}"),
            ),
        }
    }

    fn show_settings(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        ui.spacing_mut().item_spacing.y = 4.0;
        // Two columns once the workspace can hold them: settings is a wall of
        // short rows, and one 1180 px column reads as empty space.
        if ui.available_width() >= 1000.0 {
            ui.columns(2, |columns| {
                self.settings_upper(&mut columns[0]);
                self.settings_lower(&mut columns[1]);
            });
        } else {
            self.settings_upper(ui);
            ui.add_space(6.0);
            self.settings_lower(ui);
        }
    }

    fn settings_upper(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        let mut save_automation = false;
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_label(ui, "PERFORMANCE");
            ui.horizontal(|ui| {
                ui.add_sized(
                    [74.0, 24.0],
                    egui::Label::new(egui::RichText::new("Heap").size(10.5).strong().color(MUTED)),
                );
                ui.spacing_mut().slider_width = 210.0;
                ui.add(
                    egui::Slider::new(&mut self.settings_ram_mb, 1024..=32768)
                        .step_by(512.0)
                        .suffix(" MiB"),
                );
            });
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("JVM ARGUMENTS")
                    .size(9.5)
                    .strong()
                    .color(MUTED),
            );
            ui.add(
                egui::TextEdit::multiline(&mut self.settings_jvm_args)
                    .code_editor()
                    .desired_rows(2)
                    .hint_text("JVM arguments — one per line"),
            );
            let selected_index = self
                .selected_instance
                .as_deref()
                .and_then(|id| self.instances.iter().position(|instance| instance.id == id));
            match selected_index {
                Some(index) => {
                    let name = self.instances[index].name.clone();
                    if ui
                        .horizontal(|ui| {
                            ui.add_sized(
                                [200.0, CONTROL_HEIGHT],
                                egui::Button::new(format!("Apply to {name}")),
                            )
                            .on_hover_text("Saves the heap and JVM arguments to this instance.")
                        })
                        .inner
                        .clicked()
                    {
                        let instance = &mut self.instances[index];
                        let previous_ram = instance.ram_mb;
                        let previous_args = instance.jvm_args.clone();
                        instance.ram_mb = self.settings_ram_mb;
                        instance.jvm_args = parse_jvm_args(&self.settings_jvm_args);
                        let result = instance.save();
                        if result.is_err() {
                            instance.ram_mb = previous_ram;
                            instance.jvm_args = previous_args;
                        }
                        let (kind, text) = match result {
                            Ok(_) => (StatusKind::Success, "Instance settings saved.".to_owned()),
                            Err(error) => (
                                StatusKind::Error,
                                format!("Could not save instance settings: {error}"),
                            ),
                        };
                        self.set_status(kind, text);
                        self.refresh_instance_cards();
                    }
                }
                None => {
                    ui.add_enabled(
                        false,
                        egui::Button::new("Apply to selected instance")
                            .min_size(egui::vec2(200.0, CONTROL_HEIGHT)),
                    )
                    .on_disabled_hover_text(
                        "Select an instance on the Overview, Instances, or Content tab first.",
                    );
                }
            }
        });
        ui.add_space(6.0);
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_label(ui, "INTERFACE");
            let mut chosen: Option<f32> = None;
            ui.horizontal(|ui| {
                ui.add_sized(
                    [74.0, 22.0],
                    egui::Label::new(
                        egui::RichText::new("Scale")
                            .size(10.0)
                            .strong()
                            .color(MUTED),
                    ),
                );
                if ui
                    .selectable_label(self.ui_scale <= 0.0, "Auto")
                    .on_hover_text(
                        "Follow the display size and DPI, including when the window moves.",
                    )
                    .clicked()
                {
                    chosen = Some(0.0);
                }
                for step in [0.9_f32, 1.0, 1.1, 1.25] {
                    let selected = (self.ui_scale - step).abs() < 0.001;
                    if ui
                        .selectable_label(selected, format!("{}%", (step * 100.0).round()))
                        .clicked()
                    {
                        chosen = Some(step);
                    }
                }
                if self.ui_scale <= 0.0 {
                    // Auto hides the resolved value behind a discrete choice;
                    // naming it makes a monitor change visible at a glance.
                    ui.label(
                        egui::RichText::new(format!(
                            "· {}% now",
                            (self.applied_zoom * 100.0).round()
                        ))
                        .size(9.0)
                        .color(accent_soft()),
                    );
                }
            });
            if let Some(step) = chosen {
                if step <= 0.0 {
                    self.use_auto_scale(ui.ctx());
                } else {
                    self.set_ui_scale(ui.ctx(), step);
                }
            }
        });
        if cfg!(windows) {
            ui.add_space(6.0);
            Self::glass_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                section_label(ui, "STARTUP");
                if ui
                    .checkbox(&mut self.launch_at_startup, "Start Kaelis when I sign in")
                    .on_hover_text(
                        "One per-user entry opens Kaelis hidden in the tray. No service and no delay.",
                    )
                    .changed()
                {
                    match platform::set_startup(self.launch_at_startup) {
                        Ok(()) => save_automation = true,
                        Err(error) => {
                            self.launch_at_startup = !self.launch_at_startup;
                            self.set_status(
                                StatusKind::Error,
                                format!("Could not change startup: {error}"),
                            );
                        }
                    }
                }
            });
        }
        ui.add_space(6.0);
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_label(ui, "AUTOMATION");
            if ui
                .checkbox(
                    &mut self.auto_repair_before_launch,
                    "Repair mods before launch",
                )
                .on_hover_text("Installs missing required mods before the JVM starts.")
                .changed()
            {
                save_automation = true;
            }
            if ui
                .checkbox(&mut self.auto_resolve_duplicates, "Disable duplicate mods")
                .on_hover_text("Keeps the newest jar when two mods share an id.")
                .changed()
            {
                save_automation = true;
            }
            ui.horizontal(|ui| {
                if ui.button("Repair now").clicked() {
                    self.repair_mod_environment(ui.ctx().clone());
                }
                if ui
                    .button("Clean now")
                    .on_hover_text("Erase known junk now. Unknown folders stay.")
                    .clicked()
                {
                    self.clean_junk_now(ui.ctx().clone());
                }
            });
            self.show_automation_commands(ui);
        });
        if save_automation {
            self.persist_settings();
        }
    }

    fn settings_lower(&mut self, ui: &mut egui::Ui) {
        // The zero-setup card is gone. It described the absence of a setting
        // nobody was looking for, and the account tab already says where a
        // sign-in lives.
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_label(ui, "COMMUNITY & ABOUT");
            ui.horizontal(|ui| {
                draw_brand_mark(ui, 36.0);
                ui.add_space(8.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(kaelis_core::brand::COMPANY)
                            .size(14.0)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new(kaelis_core::brand::TAGLINE)
                            .size(9.5)
                            .color(MUTED),
                    );
                    ui.label(
                        egui::RichText::new(kaelis_core::brand::build_identity())
                            .size(8.5)
                            .color(MUTED),
                    );
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button("Website")
                        .on_hover_text(kaelis_core::brand::WEBSITE)
                        .clicked()
                    {
                        ui.ctx()
                            .open_url(egui::OpenUrl::new_tab(kaelis_core::brand::WEBSITE));
                    }
                    if discord_button(ui, "Join the Discord")
                        .on_hover_text(kaelis_core::brand::DISCORD_HUB)
                        .clicked()
                    {
                        ui.ctx()
                            .open_url(egui::OpenUrl::new_tab(kaelis_core::brand::DISCORD_HUB));
                    }
                });
            });
            ui.horizontal(|ui| {
                if ui
                    .button("Open data folder")
                    .on_hover_text("Instances, settings.json, cached metadata, and runtimes.")
                    .clicked()
                {
                    let instances = Instance::instances_dir();
                    let root = instances
                        .parent()
                        .map(std::path::Path::to_path_buf)
                        .unwrap_or(instances);
                    let result = std::fs::create_dir_all(&root)
                        .map_err(|error| error.to_string())
                        .and_then(|()| open_instance_path(&root, false));
                    if let Err(error) = result {
                        self.set_status(
                            StatusKind::Error,
                            format!("Could not open the data folder: {error}"),
                        );
                    }
                }
                if ui
                    .button("Copy diagnostics")
                    .on_hover_text("Version, OS, paths, and a summary. No secrets.")
                    .clicked()
                {
                    let text = diagnostics_text(&self.instances, self.selected_instance());
                    ui.ctx().copy_text(text);
                    self.set_status(StatusKind::Success, "Diagnostics copied. No secrets.");
                }
                ui.hyperlink_to(
                    egui::RichText::new("Website ↗").size(10.5),
                    kaelis_core::brand::WEBSITE,
                );
                ui.hyperlink_to(
                    egui::RichText::new("Source & releases ↗").size(10.5),
                    kaelis_core::brand::GITHUB_REPOSITORY,
                );
                ui.hyperlink_to(
                    egui::RichText::new(format!("{} ↗", kaelis_core::brand::FOUNDER)).size(10.5),
                    kaelis_core::brand::FOUNDER_URL,
                );
            });
            ui.label(
                egui::RichText::new("Logs stay in memory and vanish on exit. No telemetry.")
                    .size(9.5)
                    .color(MUTED),
            );
        });
    }
}

impl KaelisApp {
    /// Development-only visual check: `KAELIS_SCREENSHOT=path.png` captures
    /// the rendered viewport through egui after a few warm-up frames and then
    /// closes the window. Native OpenGL window grabs are blank on some hosts,
    /// while egui's own framebuffer readback works everywhere.
    fn capture_screenshot(&mut self, context: &egui::Context) {
        let Some(path) = self.screenshot_path.clone() else {
            return;
        };
        self.screenshot_frame = self.screenshot_frame.saturating_add(1);
        if let Some(image) = context.input(|input| {
            input.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        }) {
            let [width, height] = image.size;
            let mut rgba = Vec::with_capacity(width * height * 4);
            for pixel in &image.pixels {
                rgba.extend_from_slice(&pixel.to_array());
            }
            let saved = image::RgbaImage::from_raw(width as u32, height as u32, rgba)
                .ok_or_else(|| "screenshot dimensions overflowed".to_owned())
                .and_then(|buffer| buffer.save(&path).map_err(|error| error.to_string()));
            match saved {
                Ok(()) => eprintln!("kaelis screenshot saved to {}", path.display()),
                Err(error) => eprintln!("kaelis screenshot failed: {error}"),
            }
            self.screenshot_path = None;
            context.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.screenshot_frame == self.screenshot_at {
            context.send_viewport_cmd(egui::ViewportCommand::Screenshot);
            // The screenshot arrives on a later frame; keep the loop alive so
            // an idle window cannot stall between the request and the capture.
            context.request_repaint();
        }
    }
}

impl eframe::App for KaelisApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        // The title-bar close button takes the same instant path as Quit. A
        // graceful toolkit shutdown would first paint a frame, then join every
        // worker; settings are already on disk, so there is nothing to gain by
        // waiting.
        if context.input(|input| input.viewport().close_requested()) {
            self.quit_now();
        }
        self.apply_interface_scale(context);
        self.handle_keyboard_navigation(context);
        if self.editing.is_none()
            && std::env::var_os("KAELIS_SCREENSHOT_EDIT").is_some()
            && let Some(instance) = self.instances.first().cloned()
        {
            self.begin_edit(instance);
        }
        // Development-only fixture: a clipboard paste cannot be scripted from a
        // capture run, so the same code path can be asked for on one frame.
        if self.screenshot_path.is_some()
            && self.screenshot_frame == 1
            && std::env::var_os("KAELIS_SCREENSHOT_PASTE").is_some()
        {
            self.paste_clipboard_artwork(context);
        }
        // Development-only fixture: a capture window cannot reliably own
        // keyboard focus, so a Content query can be requested here and still
        // travel the real search path.
        if self.screenshot_path.is_some()
            && self.screenshot_frame == 1
            && let Ok(query) = std::env::var("KAELIS_SCREENSHOT_QUERY")
            && !query.trim().is_empty()
        {
            self.mod_query = query;
            self.start_mod_search(context.clone());
        }
        // Development-only fixture: a native file picker cannot be scripted
        // from a capture run, so the icon it would return can be supplied here
        // and still travels the real read → store → refresh path.
        if self.screenshot_path.is_some()
            && self.screenshot_frame == 1
            && let Some(path) = std::env::var_os("KAELIS_SCREENSHOT_ICON")
            && let Ok(bytes) = std::fs::read(PathBuf::from(path))
            && let Some(instance) = self.instances.first().cloned()
        {
            self.select_instance(&instance.id);
            self.apply_instance_artwork(&instance.id, &bytes, context);
        }
        while let Ok(event) = self.receiver.try_recv() {
            self.handle_event(context, event);
        }
        self.run_automation_tick(context);
        self.handle_paste_fallback(context);
        // A drop works on every tab: the file extension decides the destination
        // and the active tab only breaks the .zip tie.
        if let Some(paths) = dropped_paths(context) {
            self.handle_drop(paths, context);
        }
        // Development-only fixture: a signed-in Microsoft identity, dispatched
        // through the real completion handler so a captured frame proves the
        // account actually lands in the list. Debug builds only.
        #[cfg(debug_assertions)]
        if self.screenshot_path.is_some()
            && self.screenshot_frame == 1
            && self.microsoft_session.is_none()
            && let Ok(name) = std::env::var("KAELIS_SCREENSHOT_SESSION")
            && !name.trim().is_empty()
        {
            let session = AuthSession::fixture(kaelis_api::auth::MinecraftProfile {
                id: "069a79f444e94726a5befca90e38aaf5".to_owned(),
                name: name.trim().to_owned(),
            });
            self.handle_event(context, GuiEvent::SignedIn(Ok(session)));
        }
        // Development-only fixture: show the device-code panel without asking
        // Microsoft for a real code, so a captured frame proves its layout.
        if self.screenshot_path.is_some()
            && self.screenshot_frame == 1
            && self.auth_device.is_none()
            && std::env::var_os("KAELIS_SCREENSHOT_DEVICE").is_some()
        {
            self.auth_device = Some(DeviceCodeResponse {
                user_code: "K7Q4M-2XRB".to_owned(),
                device_code: String::new(),
                verification_uri: "https://www.microsoft.com/link".to_owned(),
                verification_uri_complete: None,
                interval: 5,
                expires_in: 840,
                message: None,
            });
            self.auth_device_until =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(840));
            self.auth_code_copied = true;
        }
        // Development-only fixture: seed the update panel so a single captured
        // frame can prove its layout without automating a click.
        if self.screenshot_path.is_some()
            && self.screenshot_frame == 1
            && self.update_report.is_none()
            && !self.updates_busy
            && std::env::var_os("KAELIS_SCREENSHOT_UPDATES").is_some()
        {
            self.start_update_check(context.clone());
        }
        // Debounced username → skin lookup: one request after typing settles,
        // not one per keystroke.
        if let Some(due) = self.offline_skin_due {
            let now = std::time::Instant::now();
            if now >= due {
                self.offline_skin_due = None;
                let name = self.offline_name.trim().to_owned();
                self.request_offline_skin(context, &name);
            } else {
                context.request_repaint_after(due - now);
            }
        }
        // A capture window may start unfocused; it still has to advance its
        // warm-up frames, so screenshot mode is always treated as focused.
        let focused = context.input(|input| input.focused) || self.screenshot_path.is_some();
        // Idle hibernation: ninety seconds after focus loss the decoded
        // textures and fonts have not been looked at once, so the OS is asked
        // to page them out. The single timer is armed on the focus-loss frame
        // and never re-armed, so an unfocused launcher costs one wake, not a
        // polling loop. Regaining focus cancels the state and pages fault back
        // in on demand.
        if focused {
            self.unfocused_since = None;
            self.idle_trimmed = false;
        } else if self.unfocused_since.is_none() {
            self.unfocused_since = Some(std::time::Instant::now());
            context.request_repaint_after(std::time::Duration::from_secs(90));
        } else if !self.idle_trimmed
            && self
                .unfocused_since
                .is_some_and(|since| since.elapsed() >= std::time::Duration::from_secs(90))
        {
            platform::trim_working_set();
            self.idle_trimmed = true;
        }
        self.governor.focused = focused;
        // A launcher nobody is looking at, with nothing in flight, drops to the
        // bottom of the schedule: lowest CPU priority, disk and memory priority
        // at their minimums, no priority boosts, EcoQoS. A hidden window always
        // qualifies — focus can still be reported by the toolkit for a window
        // that was shown for one frame — while work in progress keeps the normal
        // schedule, so a download or a repair the user started is never slowed
        // down by a focus change.
        platform::set_idle_mode(platform::window_is_hidden() || (!focused && !self.is_busy()));
        // Screenshot mode walks a fixed number of frames, so it must keep
        // repainting even when nothing is animating.
        self.governor.animating = self.is_busy() || self.screenshot_path.is_some();
        self.governor.apply(context);
        // Remembered switches and window geometry: one comparison and one
        // pointer read per frame, nothing that touches the disk until a value
        // actually changed.
        self.persist_switches_if_changed();
        self.track_window_size(context);
        #[cfg(windows)]
        if !self.window_rounded {
            // The compositor needs the real handle, which exists once the
            // window does. A retry is bounded so a system that never answers
            // cannot turn this into a repaint loop, and a system that answers
            // with an error (Windows 10) is done on the first try.
            self.window_rounded = platform::round_window_corners();
            if !self.window_rounded && self.window_round_tries < 20 {
                self.window_round_tries += 1;
                context.request_repaint();
            }
        }
        Self::window_resize_grips(context);
        Self::paint_backdrop(context);
        self.show_header(context);
        self.show_sidebar(context);
        self.show_footer(context);
        // On a wide monitor the surplus becomes margin, so text lines and
        // form rows stay a readable width instead of stretching edge to edge.
        let screen = context.screen_rect().width();
        let sidebar = if screen < RAIL_WIDTH {
            RAIL_SIDEBAR_WIDTH
        } else {
            SIDEBAR_WIDTH
        };
        let base = if screen < RAIL_WIDTH { 12.0 } else { 16.0 };
        let gutter = ((screen - sidebar - CONTENT_MAX) * 0.5).max(0.0) + base;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::TRANSPARENT)
                    .inner_margin(egui::Margin {
                        left: gutter,
                        right: gutter,
                        top: 8.0,
                        bottom: 6.0,
                    }),
            )
            .show(context, |ui| self.show_tab(ui, context));
        self.show_content_preview(context);
        self.show_drop_overlay(context);
        self.capture_screenshot(context);
    }
}

/// Join an instance's own hook with the enabled user commands for the same
/// event. The instance line runs first, so a rule never hides it, and the two
/// share one shell so ordering is exact.
fn merge_launch_command(existing: Option<String>, chain: Option<String>) -> Option<String> {
    let existing = existing.filter(|command| !command.trim().is_empty());
    match (existing, chain) {
        (Some(existing), Some(chain)) => Some(format!("{} && {chain}", existing.trim())),
        (Some(existing), None) => Some(existing),
        (None, chain) => chain,
    }
}

/// File paths from one drag, consumed by the frame that received them.
fn dropped_paths(context: &egui::Context) -> Option<Vec<PathBuf>> {
    let paths = context.input(|input| {
        input
            .raw
            .dropped_files
            .iter()
            .filter_map(|file| file.path.clone())
            .collect::<Vec<_>>()
    });
    (!paths.is_empty()).then_some(paths)
}

/// A search field drawn as one quiet pill: a painted magnifier, the text, one
/// paste action, and a clear action that appears only when there is something
/// to clear. The painted glyphs keep the same shape on every platform, unlike
/// a font icon, and the whole pill lights up while the field has focus.
fn search_pill(
    ui: &mut egui::Ui,
    buffer: &mut String,
    id: &str,
    hint: &str,
    width: f32,
) -> egui::Response {
    let field = egui::Id::new(id);
    let focused = ui.memory(|memory| memory.has_focus(field));
    // A fixed request that no longer fits would push the row past the window
    // edge, so the pill claims what is left instead of overflowing.
    let width = width.min((ui.available_width() - 4.0).max(140.0));
    egui::Frame::none()
        .fill(if focused { SURFACE_HOVER } else { SURFACE_ALT })
        .stroke(egui::Stroke::new(
            if focused { 1.2_f32 } else { 1.0 },
            if focused { accent() } else { BORDER },
        ))
        .rounding(egui::Rounding::same(13.0))
        .inner_margin(egui::Margin::symmetric(9.0, 3.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 7.0;
                let (mark, magnifier) =
                    ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::click());
                let glyph = if focused { accent_soft() } else { MUTED };
                let stroke = egui::Stroke::new(1.3_f32, glyph);
                let centre = mark.center() - egui::vec2(0.8, 0.8);
                ui.painter().circle_stroke(centre, 4.0, stroke);
                ui.painter().line_segment(
                    [centre + egui::vec2(3.0, 3.0), centre + egui::vec2(5.6, 5.6)],
                    stroke,
                );
                if magnifier
                    .on_hover_cursor(egui::CursorIcon::Text)
                    .on_hover_text("Click to type, or use Paste for a link or file name.")
                    .clicked()
                {
                    ui.memory_mut(|memory| memory.request_focus(field));
                }
                let text_width = (width - 126.0).max(70.0);
                let response = ui
                    .add_sized(
                        [text_width, 20.0],
                        egui::TextEdit::singleline(buffer)
                            .id(field)
                            .frame(false)
                            .hint_text(hint),
                    )
                    .on_hover_text(
                        "Type a name, a link, or the name of a file copied in Explorer.",
                    );
                // Paste lives inside the pill, where the user is already
                // looking, as a word rather than a glyph nobody has to decode.
                let paste = ui
                    .add(
                        egui::Button::new(egui::RichText::new("Paste").size(11.0).color(
                            if buffer.is_empty() {
                                MUTED
                            } else {
                                accent_soft()
                            },
                        ))
                        .frame(false),
                    )
                    .on_hover_text("Paste text, a link, or a copied file name.")
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
                if paste.clicked()
                    && let Some(pasted) = clipboard_query_text()
                {
                    buffer.clear();
                    buffer.push_str(&pasted);
                    ui.memory_mut(|memory| memory.request_focus(field));
                }
                let clear = egui::Button::new(egui::RichText::new("×").size(13.0).color(MUTED))
                    .frame(false)
                    .min_size(egui::vec2(16.0, 16.0));
                if !buffer.is_empty() && ui.add(clear).clicked() {
                    buffer.clear();
                    ui.memory_mut(|memory| memory.request_focus(field));
                }
                response
            })
            .inner
        })
        .inner
}

/// One coarse scale step per display size. Below a 640-point-tall window the
/// whole shell steps down so a 1366×768 laptop still fits the footer; large
/// canvases gain a little reading size instead of leaving the UI tiny.
fn auto_scale_for(logical_height: f32) -> f32 {
    [0.9, 1.0, 1.05, 1.12][auto_scale_band(logical_height)]
}

fn auto_scale_band(height: f32) -> usize {
    if height < 640.0 {
        0
    } else if height < 900.0 {
        1
    } else if height < 1150.0 {
        2
    } else {
        3
    }
}

/// Resolve a step only after the display has clearly left the current band.
///
/// The threshold in each direction needs 24 points of travel, so a window
/// dragged or resized onto 900 pt cannot alternate between 1.0 and 1.05. A
/// jump across several bands is accepted immediately in the direction of
/// travel, which keeps a move between very different monitors responsive.
fn auto_scale_for_settled(height: f32, current: f32) -> f32 {
    const HYSTERESIS: f32 = 24.0;
    let current_band = if current < 0.95 {
        0
    } else if current < 1.02 {
        1
    } else if current < 1.08 {
        2
    } else {
        3
    };
    let next_band = auto_scale_band(height);
    if next_band == current_band {
        return current;
    }
    let crossed = if next_band > current_band {
        let upper = [640.0_f32, 900.0, 1150.0, f32::INFINITY][current_band];
        height >= upper + HYSTERESIS
    } else {
        let lower = [f32::NEG_INFINITY, 640.0_f32, 900.0, 1150.0][current_band];
        height <= lower - HYSTERESIS
    };
    if crossed {
        auto_scale_for(height)
    } else {
        current
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .is_some_and(|value| value.to_string_lossy().eq_ignore_ascii_case(extension))
}

/// What a drag is allowed to carry. A stray file in the batch is ignored
/// before it reaches the installer, so it cannot fail the whole drop.
const DROP_EXTENSIONS: [&str; 10] = [
    "jar", "zip", "mrpack", "png", "jpg", "jpeg", "webp", "gif", "bmp", "ico",
];
/// Upper bound on files expanded from one drag, so dropping a disk root is
/// cheap instead of an unbounded crawl.
const MAX_DROPPED_FILES: usize = 400;

fn has_drop_extension(path: &Path) -> bool {
    DROP_EXTENSIONS
        .iter()
        .any(|extension| has_extension(path, extension))
}

/// Files from one drag, with folders expanded.
///
/// People keep mods in folders (`mods-1.21/`, a downloaded pack directory), so
/// a dropped folder is read as the files inside it instead of being rejected
/// with "drop a file". Traversal is bounded and skips the folders the cleaner
/// already treats as disposable, so it never walks logs or a git checkout.
fn expand_dropped(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        if path.is_dir() {
            collect_dropped_dir(&path, 0, &mut files, &mut seen);
        } else if has_drop_extension(&path) && seen.insert(path.clone()) {
            files.push(path);
        }
    }
    files
}

fn collect_dropped_dir(
    directory: &Path,
    depth: usize,
    files: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
) {
    if depth > 2 || files.len() >= MAX_DROPPED_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    // Sorted so a folder drop installs in the same order every time.
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        if files.len() >= MAX_DROPPED_FILES {
            return;
        }
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if !matches!(
                name.as_str(),
                "logs" | "crash-reports" | ".cache" | ".git" | "temp" | "natives"
            ) {
                collect_dropped_dir(&path, depth + 1, files, seen);
            }
        } else if has_drop_extension(&path) && seen.insert(path.clone()) {
            files.push(path);
        }
    }
}

fn send_event(sender: &mpsc::UnboundedSender<GuiEvent>, context: &egui::Context, event: GuiEvent) {
    if sender.send(event).is_ok() {
        context.request_repaint();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HandoffRequest {
    Activate,
    Play {
        instance_id: String,
        offline_name: Option<String>,
    },
    Unsupported,
}

/// Interpret the subset of CLI arguments that makes sense for an already
/// running GUI. The primary process validates and bounds the payload in
/// `kaelis-core::handoff`; this only decides what to do with it.
fn parse_handoff(arguments: &[String]) -> HandoffRequest {
    let mut instance_id = None;
    let mut offline_name = None;
    let mut iter = arguments.iter();
    while let Some(argument) = iter.next() {
        match argument.as_str() {
            "--play" => match iter.next() {
                Some(value) if !value.starts_with('-') => instance_id = Some(value.clone()),
                _ => return HandoffRequest::Unsupported,
            },
            "--offline" => match iter.next() {
                Some(value) if !value.starts_with('-') => offline_name = Some(value.clone()),
                _ => return HandoffRequest::Unsupported,
            },
            _ => {}
        }
    }
    match instance_id {
        Some(instance_id) => HandoffRequest::Play {
            instance_id,
            offline_name,
        },
        None if arguments.is_empty() => HandoffRequest::Activate,
        None => HandoffRequest::Unsupported,
    }
}

fn show_window(context: &egui::Context) {
    // A hidden window paints no frames, so egui's own command alone cannot
    // bring the window back. The platform path restores it natively, raises it
    // above a running game, and requests the frame that reapplies the toolkit
    // state.
    platform::wake(context);
}

/// Folder-safe id for an instance name.
///
/// Only ASCII survives into the directory name, but names that lose anything in
/// that step (accents, CJK, emoji, punctuation-only) get a stable hash suffix.
/// Without it, `日本語` collapsed to an empty id and a brand-new instance was
/// rejected as "already in use"; different lossy names could also collide.
fn slugify(value: &str) -> String {
    let mut output = String::with_capacity(64);
    let mut separator = false;
    let mut lossy = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() || character == '_' {
            if separator && !output.is_empty() {
                output.push('-');
            }
            separator = false;
            output.push(character);
        } else {
            if !(character.is_ascii() || character == '-' || character.is_whitespace()) {
                lossy = true;
            }
            separator = true;
        }
        if output.len() >= 48 {
            break;
        }
    }
    let mut id = output.trim_matches('-').to_owned();
    if id.is_empty() {
        id.push_str("instance");
        lossy = true;
    }
    if lossy {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in value.trim().as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        id.push_str(&format!("-{hash:08x}", hash = hash as u32));
    }
    id
}

/// The instance that already owns this name, if any.
///
/// Both the folder id and the display name are compared (case-insensitively),
/// so the warning only appears for a genuine collision and can name the
/// instance that is in the way.
/// Wording for a real collision, naming the instance that holds the name.
fn name_conflict_text(name: &str, existing: &Instance) -> String {
    if existing.name.trim().eq_ignore_ascii_case(name.trim()) {
        format!("An instance named “{name}” already exists.")
    } else {
        format!(
            "“{name}” needs a different name: “{}” already uses it.",
            existing.name
        )
    }
}

fn instance_name_conflict<'a>(instances: &'a [Instance], name: &str) -> Option<&'a Instance> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }
    let id = slugify(trimmed);
    let lowered = trimmed.to_lowercase();
    instances
        .iter()
        .find(|instance| instance.id == id || instance.name.trim().to_lowercase() == lowered)
}

fn significant_tokens(text: &str) -> Vec<String> {
    const GENERIC: [&str; 12] = [
        "fabric",
        "forge",
        "neoforge",
        "quilt",
        "minecraft",
        "mods",
        "mod",
        "api",
        "client",
        "common",
        "lib",
        "library",
    ];
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .filter(|token| {
            token.len() >= 3
                && !token.bytes().all(|byte| byte.is_ascii_digit())
                && !GENERIC.contains(&token.as_str())
        })
        .collect()
}

/// Tokens for interactive filtering. Unlike [`significant_tokens`], numbers and
/// words such as `fabric` or `api` stay meaningful: a user filtering installed
/// jars for `1.21` expects that to narrow the list.
fn filter_tokens(text: &str) -> Vec<String> {
    text.split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect()
}

fn mod_search_text(item: &ModSearchItem) -> String {
    format!("{} {} {}", item.title, item.slug, item.description).to_ascii_lowercase()
}

/// Local relevance for the query that produced the current result set. The
/// provider ranking stays as a tie-break; this only surfaces exact and prefix
/// matches instead of trusting two differently tuned remote rankers.
fn relevance_score(query: &str, item: &ModSearchItem, search_text: &str) -> u8 {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return 0;
    }
    let needle = normalize_mod_id(trimmed);
    let title = normalize_mod_id(&item.title);
    let slug = normalize_mod_id(&item.slug);
    if !needle.is_empty() && (title == needle || slug == needle) {
        return 7;
    }
    if !needle.is_empty() && (title.starts_with(&needle) || slug.starts_with(&needle)) {
        return 6;
    }
    let tokens = significant_tokens(trimmed);
    if tokens.is_empty() {
        return 0;
    }
    // A title that starts every query word is a much better answer than one
    // that merely mentions the words somewhere in its description.
    let title_words = item
        .title
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    if tokens.iter().all(|token| {
        title_words
            .iter()
            .any(|word| word.starts_with(token.as_str()))
    }) {
        return 5;
    }
    if tokens.iter().all(|token| title.contains(token.as_str())) {
        return 4;
    }
    if !needle.is_empty() && title.contains(&needle) {
        return 3;
    }
    if tokens
        .iter()
        .all(|token| search_text.contains(token.as_str()))
    {
        return 2;
    }
    if tokens
        .iter()
        .any(|token| search_text.contains(token.as_str()))
    {
        return 1;
    }
    0
}

fn build_mod_rows(items: &[ModSearchItem], query: &str) -> Vec<ModRow> {
    items
        .iter()
        .map(|item| {
            let tokens = significant_tokens(&item.slug);
            let match_tokens = if tokens.is_empty() {
                significant_tokens(&item.title)
            } else {
                tokens
            };
            let search_text = mod_search_text(item);
            let author_line = if item.author.trim().is_empty() {
                format!("by {}", item.provider)
            } else {
                format!("by {}", item.author.trim())
            };
            let mut meta = format!(
                "{} downloads · {} followers",
                compact_count(item.downloads),
                compact_count(item.follows)
            );
            if let Some(date) = short_date(&item.updated) {
                meta.push_str(" · updated ");
                meta.push_str(date);
            }
            let categories_line = item
                .categories
                .iter()
                .take(4)
                .map(|category| title_case(category))
                .collect::<Vec<_>>()
                .join("  ·  ");
            let compatibility_line = if item.latest_version.trim().is_empty() {
                item.kind.label().to_owned()
            } else {
                format!("{} · {}", item.kind.singular(), item.latest_version.trim())
            };
            ModRow {
                key: mod_item_key(item),
                project_url: item.kind.project_url(item.provider, &item.slug),
                author_line,
                meta_line: meta,
                categories_line,
                compatibility_line,
                kind: item.kind,
                match_tokens,
                relevance: relevance_score(query, item, &search_text),
                search_text,
            }
        })
        .collect()
}

fn short_date(value: &str) -> Option<&str> {
    let date = value.get(..10)?;
    date.as_bytes().get(4).filter(|byte| **byte == b'-')?;
    Some(date)
}

fn title_case(value: &str) -> String {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    let mut text = String::with_capacity(value.len());
    text.extend(first.to_uppercase());
    text.push_str(&characters.as_str().replace(['-', '_'], " "));
    text
}

fn visible_mod_indices(
    items: &[ModSearchItem],
    rows: &[ModRow],
    refine: &str,
    view: ModView,
) -> Vec<usize> {
    let refine_tokens = filter_tokens(refine);
    let mut visible = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let row = rows.get(index)?;
            if (item.provider == ModProvider::Modrinth && !view.show_modrinth)
                || (item.provider == ModProvider::CurseForge && !view.show_curseforge)
            {
                return None;
            }
            if !refine_tokens.is_empty()
                && !refine_tokens
                    .iter()
                    .all(|token| row.search_text.contains(token.as_str()))
            {
                return None;
            }
            Some(index)
        })
        .collect::<Vec<_>>();
    visible.sort_by(|left, right| {
        let left_item = &items[*left];
        let right_item = &items[*right];
        // Modrinth is the primary catalogue and stays above the CurseForge
        // mirror in every order. Download totals are not comparable between
        // the two (the mirror counts every pack that ever embedded a project),
        // so mixing them put CurseForge on top even when the query matched a
        // Modrinth project exactly. Within each provider the chosen order is
        // untouched.
        left_item
            .provider
            .cmp(&right_item.provider)
            .then_with(|| match view.sort {
                ModSort::Relevance => rows[*right]
                    .relevance
                    .cmp(&rows[*left].relevance)
                    .then_with(|| right_item.downloads.cmp(&left_item.downloads))
                    .then_with(|| left_item.title.cmp(&right_item.title)),
                ModSort::Downloads => right_item
                    .downloads
                    .cmp(&left_item.downloads)
                    .then_with(|| left_item.title.cmp(&right_item.title)),
                ModSort::Name => left_item
                    .title
                    .to_ascii_lowercase()
                    .cmp(&right_item.title.to_ascii_lowercase())
                    .then_with(|| left_item.title.cmp(&right_item.title)),
            })
    });
    visible
}

fn instance_matches(instance: &Instance, filter: &str) -> bool {
    let tokens = filter_tokens(filter);
    if tokens.is_empty() {
        return true;
    }
    let haystack = format!(
        "{} {} {} {} {}",
        instance.name, instance.group, instance.mc_version, instance.loader, instance.id
    )
    .to_ascii_lowercase();
    tokens.iter().all(|token| haystack.contains(token.as_str()))
}
fn scan_installed(instance: &Instance) -> InstalledScan {
    scan_mods_dir(&instance.root)
}

fn scan_mods_dir(root: &Path) -> InstalledScan {
    let mut scan = InstalledScan::default();
    let entries = std::fs::read_dir(root.join("mods"))
        .into_iter()
        .flatten()
        .flatten();
    let mut loader_counts: HashMap<Option<&'static str>, usize> = HashMap::new();
    for entry in entries {
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let lower = name.to_ascii_lowercase();
        let (stem, disabled) = if let Some(stem) = lower.strip_suffix(".jar.disabled") {
            (stem, true)
        } else if let Some(stem) = lower.strip_suffix(".jar") {
            (stem, false)
        } else {
            continue;
        };
        let metadata = kaelis_api::modmeta::read_jar_cached(&entry.path());
        loader_counts
            .entry(metadata.loader)
            .and_modify(|count| *count += 1)
            .or_insert(1);
        scan.files.push(InstalledFile {
            name,
            path: entry.path(),
            tokens: significant_tokens(stem),
            ids: metadata.ids,
            primary_ids: metadata.primary_ids,
            requires: metadata.requires,
            disabled,
        });
    }
    // The loader is implied by the jars themselves; a stale instance label
    // should not filter searches into an empty result set.
    scan.detected_loader = loader_counts
        .into_iter()
        .filter_map(|(loader, count)| loader.map(|loader| (loader, count)))
        .max_by_key(|(_, count)| *count)
        .map(|(loader, _)| loader);
    scan.files.sort_by(|a, b| a.name.cmp(&b.name));
    let disabled = scan.files.iter().filter(|file| file.disabled).count();
    let total = scan.files.len();
    let noun = if total == 1 { "mod" } else { "mods" };
    scan.summary = if disabled == 0 {
        format!("{total} {noun} installed")
    } else {
        format!("{total} {noun} installed · {disabled} disabled")
    };
    let mut provided = HashMap::new();
    let mut required = std::collections::BTreeSet::new();
    let mut owned = HashMap::new();
    for file in scan.files.iter().filter(|file| !file.disabled) {
        for id in &file.ids {
            *provided.entry(id.clone()).or_insert(0_usize) += 1;
        }
        for id in &file.primary_ids {
            *owned.entry(id.clone()).or_insert(0_usize) += 1;
        }
        for id in &file.requires {
            required.insert(id.clone());
        }
    }
    for (id, count) in &owned {
        if *count > 1 {
            scan.conflicts.push(format!("{id} ({count} files)"));
        }
    }
    for id in required {
        if !provided.contains_key(&id) && !kaelis_api::modmeta::is_builtin_id(&id) {
            scan.missing.push(id);
        }
    }
    if !scan.missing.is_empty() {
        scan.summary
            .push_str(&format!(" · {} missing", scan.missing.len()));
    }
    if !scan.conflicts.is_empty() {
        scan.summary
            .push_str(&format!(" · {} duplicate id(s)", scan.conflicts.len()));
    }
    scan
}

fn toggle_mod_file(path: &std::path::Path, disabled: bool) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .ok_or_else(|| "mod file has no name".to_owned())?;
    let mut target_name = name.to_os_string();
    if disabled {
        let lower = name.to_string_lossy();
        let stem = lower
            .strip_suffix(".disabled")
            .ok_or_else(|| "disabled mod name is malformed".to_owned())?;
        target_name = std::ffi::OsString::from(stem);
    } else {
        target_name.push(".disabled");
    }
    let target = path.with_file_name(target_name);
    std::fs::rename(path, &target).map_err(|error| error.to_string())?;
    Ok(target)
}

fn parse_jvm_args(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn valid_quick_join(address: &str) -> bool {
    if address.is_empty()
        || address
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        || address.contains(['/', '\\'])
    {
        return false;
    }
    let (host, port) = match address.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (address, None),
    };
    if host.is_empty() {
        return false;
    }
    port.is_none_or(|port| port.parse::<u16>().is_ok_and(|port| port != 0))
}

fn valid_instance_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 64
        && version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn draw_instance_avatar(
    ui: &mut egui::Ui,
    texture: Option<&egui::TextureHandle>,
    instance: &Instance,
) -> egui::Rect {
    let size = egui::vec2(40.0, 40.0);
    if let Some(texture) = texture {
        let response =
            ui.add(egui::Image::new((texture.id(), size)).rounding(egui::Rounding::same(10.0)));
        return response.rect;
    }
    let (avatar, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter()
        .circle_filled(avatar.center(), 19.0, GLASS_LIGHT);
    ui.painter().text(
        avatar.center(),
        egui::Align2::CENTER_CENTER,
        instance.name.chars().next().unwrap_or('K'),
        egui::FontId::proportional(17.0),
        accent(),
    );
    avatar
}

/// Reveal an instance folder or its metadata file with the platform's file
/// manager. The process is detached, so a missing shell never blocks the UI.
fn open_instance_path(path: &std::path::Path, reveal: bool) -> Result<(), String> {
    // Linux desktops do not all ship `xdg-open`; try the freedesktop portals,
    // then the KDE helper, before reporting a failure.
    let candidates: &[(&str, &[&str])] = if cfg!(windows) {
        &[("explorer", &[])]
    } else if cfg!(target_os = "macos") {
        &[("open", &[])]
    } else {
        &[("xdg-open", &[]), ("gio", &["open"]), ("kde-open5", &[])]
    };
    let mut last_error = String::new();
    for (program, leading) in candidates {
        let mut command = std::process::Command::new(program);
        command
            .args(*leading)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        if cfg!(windows) && reveal {
            command.arg(format!("/select,{}", path.display()));
        } else {
            command.arg(path);
        }
        match command.spawn() {
            Ok(_) => return Ok(()),
            Err(error) => last_error = format!("{program}: {error}"),
        }
    }
    Err(last_error)
}

/// Small vector glyphs keep the navigation legible even when the platform font
/// lacks geometric Unicode symbols. Every glyph is drawn on the same 16 px
/// grid with one stroke weight, so the rail reads as a set rather than icons
/// borrowed from five different families.
fn paint_tab_icon(painter: &egui::Painter, center: egui::Pos2, tab: Tab, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.4_f32, color);
    let at = |x: f32, y: f32| center + egui::vec2(x, y);
    match tab {
        Tab::Overview => {
            for x in [-3.4_f32, 1.0] {
                for y in [-3.4_f32, 1.0] {
                    painter.rect_filled(
                        egui::Rect::from_min_size(at(x, y), egui::vec2(4.4, 4.4)),
                        egui::Rounding::same(1.3),
                        color,
                    );
                }
            }
        }
        Tab::Instances => {
            painter.rect_stroke(
                egui::Rect::from_center_size(at(0.0, 0.0), egui::vec2(12.0, 10.0)),
                egui::Rounding::same(2.0),
                stroke,
            );
            painter.line_segment([at(-3.2, -2.4), at(3.2, -2.4)], stroke);
        }
        Tab::Content => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    at(0.0, -6.0),
                    at(1.7, -1.7),
                    at(6.0, 0.0),
                    at(1.7, 1.7),
                    at(0.0, 6.0),
                    at(-1.7, 1.7),
                    at(-6.0, 0.0),
                    at(-1.7, -1.7),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        Tab::Account => {
            painter.circle_stroke(at(0.0, -2.8), 2.9, stroke);
            let shoulders = (0..=12)
                .map(|step| {
                    let angle = std::f32::consts::PI + step as f32 * std::f32::consts::PI / 12.0;
                    at(angle.cos() * 5.6, 4.4 + angle.sin() * 3.6)
                })
                .collect::<Vec<_>>();
            painter.add(egui::Shape::line(shoulders, stroke));
        }
        Tab::Settings => {
            for (y, knob) in [(-4.0_f32, 2.2_f32), (0.0, -2.6), (4.0, 1.2)] {
                painter.line_segment([at(-5.6, y), at(5.6, y)], stroke);
                painter.circle_filled(at(knob, y), 1.7, color);
            }
        }
    }
}

pub fn window_icon() -> egui::IconData {
    mark::primary_icon(64)
}

/// The mark, drawn from the same geometry as the window icon so the sidebar,
/// About card, taskbar, and tray never drift apart.
fn draw_brand_mark(ui: &mut egui::Ui, size: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        mark::paint(ui.painter(), rect);
    }
    response
}

fn instance_content_summary(root: &Path) -> String {
    let mods = count_dir_files(&root.join("mods"), &[".jar", ".jar.disabled"]);
    let packs = count_dir_files(&root.join("resourcepacks"), &[".zip", ".zip.disabled"]);
    let shaders = count_dir_files(&root.join("shaderpacks"), &[".zip", ".zip.disabled"]);
    let worlds = std::fs::read_dir(root.join("saves"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .count();
    format!(
        "{} · {} · {} · {}",
        count_noun(mods, "mod", "mods"),
        count_noun(packs, "pack", "packs"),
        count_noun(shaders, "shader", "shaders"),
        count_noun(worlds, "world", "worlds")
    )
}

/// `1 mod` and `31 mods`, never `1 mods`. The card line is the first thing on
/// an instance, so a plural that only fits zero or many is a visible typo.
fn count_noun(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

fn count_dir_files(directory: &Path, suffixes: &[&str]) -> usize {
    std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && suffixes.iter().any(|suffix| name.ends_with(suffix))
        })
        .count()
}

/// One compact line for the instance card. The full note stays available on
/// hover and is rebuilt only when the card cache refreshes.
fn note_preview(notes: &str) -> String {
    const LIMIT: usize = 110;
    let flattened = notes.split_whitespace().collect::<Vec<_>>().join(" ");
    if flattened.chars().count() <= LIMIT {
        return flattened;
    }
    let mut preview = flattened.chars().take(LIMIT).collect::<String>();
    preview.push('…');
    preview
}

fn relative_last_played(unix_seconds: i64) -> String {
    // An instance nobody has started yet has nothing to say about its history.
    // An empty string keeps the card free of a chip that only repeats "never".
    if unix_seconds <= 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64);
    let elapsed = now.saturating_sub(unix_seconds).max(0);
    match elapsed {
        0..=59 => "Played just now".to_owned(),
        60..=3_599 => format!("Played {}m ago", elapsed / 60),
        3_600..=86_399 => format!("Played {}h ago", elapsed / 3_600),
        _ => format!("Played {}d ago", elapsed / 86_400),
    }
}

fn kind_color(kind: ContentKind) -> egui::Color32 {
    match kind {
        ContentKind::Mod => accent(),
        ContentKind::ResourcePack => egui::Color32::from_rgb(126, 231, 165),
        ContentKind::Shader => VIOLET,
        ContentKind::Modpack => egui::Color32::from_rgb(255, 195, 120),
    }
}

fn compact_count(value: u64) -> String {
    if value >= 1_000_000 {
        format!("{:.1}M", value as f64 / 1_000_000.0)
    } else if value >= 1_000 {
        format!("{:.1}K", value as f64 / 1_000.0)
    } else {
        value.to_string()
    }
}

/// The default Minecraft skin head, taken pixel-for-pixel from the 8×8 face of
/// the vanilla `steve.png` texture. Offline profiles are rendered in game with
/// exactly this skin, so the account card shows the same face instead of an
/// invented portrait, and no network request is needed.
const STEVE_FACE: [[[u8; 3]; 8]; 8] = [
    [
        [0x2F, 0x20, 0x0D],
        [0x2B, 0x1E, 0x0D],
        [0x2F, 0x1F, 0x0F],
        [0x28, 0x1C, 0x0B],
        [0x24, 0x18, 0x08],
        [0x26, 0x1A, 0x0A],
        [0x2B, 0x1E, 0x0D],
        [0x2A, 0x1D, 0x0D],
    ],
    [
        [0x2B, 0x1E, 0x0D],
        [0x2B, 0x1E, 0x0D],
        [0x2B, 0x1E, 0x0D],
        [0x33, 0x24, 0x11],
        [0x42, 0x2A, 0x12],
        [0x3F, 0x2A, 0x15],
        [0x2C, 0x1E, 0x0E],
        [0x28, 0x1C, 0x0B],
    ],
    [
        [0x2B, 0x1E, 0x0D],
        [0xB6, 0x89, 0x6C],
        [0xBD, 0x8E, 0x72],
        [0xC6, 0x96, 0x80],
        [0xBD, 0x8B, 0x72],
        [0xBD, 0x8E, 0x74],
        [0xAC, 0x76, 0x5A],
        [0x34, 0x25, 0x12],
    ],
    [
        [0xAA, 0x7D, 0x67],
        [0xB4, 0x84, 0x6D],
        [0xAA, 0x7D, 0x66],
        [0xAD, 0x80, 0x6D],
        [0x9C, 0x72, 0x5C],
        [0xBB, 0x89, 0x72],
        [0x9C, 0x69, 0x4C],
        [0x9C, 0x69, 0x4C],
    ],
    [
        [0xB4, 0x84, 0x6D],
        [0xFF, 0xFF, 0xFF],
        [0x52, 0x3D, 0x89],
        [0xB5, 0x7B, 0x67],
        [0xBB, 0x89, 0x72],
        [0x52, 0x3D, 0x89],
        [0xFF, 0xFF, 0xFF],
        [0xAA, 0x7D, 0x66],
    ],
    [
        [0x9C, 0x63, 0x46],
        [0xB3, 0x7B, 0x62],
        [0xB7, 0x82, 0x72],
        [0x6A, 0x40, 0x30],
        [0x6A, 0x40, 0x30],
        [0xBE, 0x88, 0x6C],
        [0xA2, 0x6A, 0x47],
        [0x80, 0x53, 0x34],
    ],
    [
        [0x90, 0x5E, 0x43],
        [0x96, 0x5F, 0x40],
        [0x40, 0x20, 0x0A],
        [0x87, 0x4A, 0x3A],
        [0x87, 0x4A, 0x3A],
        [0x40, 0x20, 0x0A],
        [0x8F, 0x5E, 0x3E],
        [0x81, 0x53, 0x39],
    ],
    [
        [0x6F, 0x45, 0x2C],
        [0x6D, 0x43, 0x2B],
        [0x40, 0x20, 0x0A],
        [0x40, 0x20, 0x0A],
        [0x40, 0x20, 0x0A],
        [0x40, 0x20, 0x0A],
        [0x83, 0x55, 0x3B],
        [0x7A, 0x4E, 0x33],
    ],
];

/// Offline profiles get the default Steve skin in game, so the same 8×8 face is
/// loaded as a nearest-neighbour texture and scales to any avatar size.
fn steve_head() -> egui::ColorImage {
    let pixels = STEVE_FACE
        .iter()
        .flatten()
        .map(|[red, green, blue]| egui::Color32::from_rgb(*red, *green, *blue))
        .collect();
    egui::ColorImage {
        size: [8, 8],
        pixels,
    }
}

/// Decode a 64 px head PNG into a texture-ready image. Bounded by the image
/// crate's own limits; a bad response yields `None` instead of panicking.
fn decode_head(bytes: &[u8]) -> Option<egui::ColorImage> {
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        image.as_raw(),
    ))
}

fn mod_item_key(item: &ModSearchItem) -> String {
    format!("{}-{:?}-{}", item.provider, item.kind, item.id)
}

fn https_icon_url(value: &str) -> Option<String> {
    let parsed = reqwest::Url::parse(value).ok()?;
    (parsed.scheme() == "https").then(|| parsed.to_string())
}
/// A painted status dot followed by its label. The bundled font has no bullet
/// glyph that renders everywhere, so the marker is drawn instead of typed.
fn dot_label(ui: &mut egui::Ui, text: &str, color: egui::Color32) -> egui::Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let (rect, _) = ui.allocate_exact_size(egui::vec2(5.0, 5.0), egui::Sense::hover());
        ui.painter().circle_filled(rect.center(), 2.2, color);
        ui.add(
            egui::Label::new(egui::RichText::new(text).size(10.0).color(color))
                .sense(egui::Sense::click()),
        )
    })
    .inner
}

fn check_chip(
    ui: &mut egui::Ui,
    state: CheckState,
    label: &'static str,
    detail: &'static str,
) -> egui::Response {
    dot_label(ui, label, state.color()).on_hover_text(detail)
}

/// A status pill with exactly the control height of the buttons, combo boxes,
/// and checkboxes beside it.
///
/// A `Frame` hugs its text, so a one-line status used to be shorter than its
/// neighbours and sat at the top of the row. Allocating the control height up
/// front and painting the dot and text on its centre line keeps every toolbar
/// on one baseline without adding another widget to the paint path.
/// Which window control a header button is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WindowControl {
    Minimize,
    Maximize,
    Restore,
    Close,
}

/// One window control on the header strip, drawn with strokes.
///
/// Strokes instead of glyphs: the last controls on the window should not depend
/// on which icons a font happens to carry, and lines stay crisp at every scale
/// factor. The shapes are the system's — a rule, a frame, two frames, and an
/// X — so the strip reads like window chrome rather than app buttons. The
/// close button keeps the system's red hover, which is the one place a window
/// says “this ends me”.
fn window_control(ui: &mut egui::Ui, control: WindowControl, tooltip: &str) -> egui::Response {
    const WIDTH: f32 = 34.0;
    /// Half the glyph's width: every shape below is drawn inside 12 × 12 points.
    const HALF: f32 = 6.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(WIDTH, CONTROL_HEIGHT), egui::Sense::click());
    let hovered = response.hovered();
    let painter = ui.painter();
    if hovered {
        let fill = if control == WindowControl::Close {
            egui::Color32::from_rgb(150, 34, 26)
        } else {
            egui::Color32::from_rgb(25, 29, 36)
        };
        painter.rect_filled(rect, egui::Rounding::same(6.0), fill);
    }
    let color = if hovered {
        egui::Color32::from_rgb(255, 255, 255)
    } else {
        MUTED
    };
    let stroke = egui::Stroke::new(1.25_f32, color);
    let center = rect.center();
    let size = egui::vec2(HALF * 2.0, HALF * 2.0);
    match control {
        WindowControl::Minimize => {
            painter.hline(
                (center.x - HALF)..=(center.x + HALF),
                center.y + HALF - 1.0,
                stroke,
            );
        }
        WindowControl::Maximize => {
            painter.rect_stroke(
                egui::Rect::from_center_size(center, size),
                egui::Rounding::same(1.5),
                stroke,
            );
        }
        WindowControl::Restore => {
            // The system's restore shape: a near frame, then the front one.
            let offset = egui::vec2(2.0, -2.0);
            let front = egui::Rect::from_center_size(center + egui::vec2(-1.0, 1.0), size);
            let back = front.translate(offset);
            painter.line_segment([back.left_top(), back.right_top()], stroke);
            painter.line_segment([back.right_top(), back.right_bottom()], stroke);
            painter.rect_stroke(front, egui::Rounding::same(1.5), stroke);
        }
        WindowControl::Close => {
            let half = egui::vec2(HALF, HALF);
            painter.line_segment([center - half, center + half], stroke);
            painter.line_segment(
                [
                    center + egui::vec2(-HALF, HALF),
                    center + egui::vec2(HALF, -HALF),
                ],
                stroke,
            );
        }
    }
    response
        .on_hover_text(tooltip)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

fn status_pill(ui: &mut egui::Ui, text: &str, color: egui::Color32) -> egui::Response {
    let galley =
        ui.painter()
            .layout_no_wrap(text.to_owned(), egui::FontId::proportional(9.5), color);
    let width = galley.size().x + 30.0;
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, CONTROL_HEIGHT), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(
        rect,
        egui::Rounding::same(CORNER_MD),
        egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 14),
    );
    painter.rect_stroke(
        rect,
        egui::Rounding::same(CORNER_MD),
        egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 34),
        ),
    );
    let dot = egui::pos2(rect.left() + 11.0, rect.center().y);
    painter.circle_filled(dot, 2.6, color);
    painter.galley(
        egui::pos2(dot.x + 9.0, rect.center().y - galley.size().y * 0.5),
        galley,
        color,
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
    response
}

/// Compact section heading for settings groups: one muted line instead of a
/// full heading, so four groups fit without scrolling.
fn section_label(ui: &mut egui::Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(egui::RichText::new(text).size(9.0).strong().color(MUTED));
}

/// Discord's blurple as a real [`egui::Button`], so keyboard focus and the
/// accessibility tree reach the community link like any other control.
fn discord_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let blurple = egui::Color32::from_rgb(88, 101, 242);
    let tint = |alpha: u8| egui::Color32::from_rgba_unmultiplied(88, 101, 242, alpha);
    let previous = ui.style().visuals.widgets.clone();
    {
        let widgets = &mut ui.style_mut().visuals.widgets;
        for (widget, fill_alpha, stroke_alpha) in [
            (&mut widgets.inactive, 34, 120),
            (&mut widgets.hovered, 255, 255),
            (&mut widgets.active, 220, 220),
        ] {
            widget.weak_bg_fill = tint(fill_alpha);
            widget.bg_fill = if fill_alpha == 255 {
                blurple
            } else {
                tint(fill_alpha)
            };
            widget.bg_stroke = egui::Stroke::new(1.0_f32, tint(stroke_alpha));
        }
    }
    let response = ui.add(
        egui::Button::new(
            egui::RichText::new(label)
                .size(11.0)
                .color(egui::Color32::WHITE),
        )
        .rounding(egui::Rounding::same(CONTROL_RADIUS)),
    );
    ui.style_mut().visuals.widgets = previous;
    response
}

/// Short, secret-free support summary. Account ids, tokens, and provider keys
/// are deliberately absent so a pasted report stays safe to share publicly.
fn diagnostics_text(instances: &[Instance], selected: Option<&Instance>) -> String {
    let instances_dir = Instance::instances_dir();
    let root = instances_dir
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| instances_dir.clone());
    let mut text = String::with_capacity(384);
    text.push_str(&format!("Kaelis {}\n", env!("CARGO_PKG_VERSION")));
    text.push_str(&format!(
        "OS: {} {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
    text.push_str(&format!("Data: {}\n", root.display()));
    text.push_str(&format!("Instances: {}\n", instances.len()));
    if let Some(instance) = selected {
        text.push_str(&format!(
            "Selected: {} · Minecraft {} · {} · heap {} MiB\n",
            instance.name, instance.mc_version, instance.loader, instance.ram_mb
        ));
        text.push_str(&format!("Root: {}\n", instance.root.display()));
        if let Some(java) = &instance.java_path {
            text.push_str(&format!("Java: {}\n", java.display()));
        }
    }
    text
}

/// Destructive action styled as an outlined red button that fills in on hover.
/// It stays a real [`egui::Button`] so keyboard focus and the accessibility
/// tree keep working; only the widget colours are swapped for the call.
fn danger_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let (r, g, b) = DANGER_TINT;
    let tint = |alpha: u8| egui::Color32::from_rgba_unmultiplied(r, g, b, alpha);
    let previous = ui.style().visuals.widgets.clone();
    {
        let widgets = &mut ui.style_mut().visuals.widgets;
        for (widget, fill_alpha, stroke_alpha) in [
            (&mut widgets.inactive, 8, 78),
            (&mut widgets.hovered, 36, 168),
            (&mut widgets.active, 54, 200),
        ] {
            widget.weak_bg_fill = tint(fill_alpha);
            widget.bg_fill = tint(fill_alpha);
            widget.bg_stroke = egui::Stroke::new(1.0_f32, tint(stroke_alpha));
        }
    }
    let response = ui.add(
        egui::Button::new(egui::RichText::new(label).size(11.0).color(DANGER))
            .min_size(egui::vec2(0.0, CONTROL_HEIGHT))
            .rounding(egui::Rounding::same(CONTROL_RADIUS)),
    );
    ui.style_mut().visuals.widgets = previous;
    response
}

/// One installed file: name on the left, two deliberate actions on the right.
/// Plain-text inventory, so an instance can be shared or diffed outside
/// Kaelis without scraping the UI.
fn content_list_text(header: &str, entries: &[(&str, bool)]) -> String {
    let disabled = entries.iter().filter(|(_, off)| *off).count();
    let mut text = String::with_capacity(header.len() + 32 + entries.len() * 40);
    text.push_str(header);
    text.push('\n');
    text.push_str(&format!(
        "{} item(s) · {} disabled\n",
        entries.len(),
        disabled
    ));
    for (name, off) in entries {
        text.push_str(if *off { "- [off] " } else { "- " });
        text.push_str(name);
        text.push('\n');
    }
    text
}

struct InstalledRow<'a> {
    name: &'a str,
    detail: &'a str,
    path: &'a Path,
    disabled: bool,
    is_dir: bool,
}

fn installed_row(
    ui: &mut egui::Ui,
    row: InstalledRow<'_>,
    confirm: &Option<String>,
    request: &mut ContentRequest,
) {
    let InstalledRow {
        name,
        detail,
        path,
        disabled,
        is_dir,
    } = row;
    ui.horizontal(|ui| {
        let mut enabled = !disabled;
        if !is_dir
            && ui
                .checkbox(&mut enabled, "")
                .on_hover_text(if disabled {
                    "Enable this item"
                } else {
                    "Disable this item"
                })
                .changed()
        {
            request.file_action = Some((
                ModFileAction::Toggle { disabled },
                path.to_path_buf(),
                false,
            ));
        }
        let width = (ui.available_width() - 72.0).max(60.0);
        ui.allocate_ui_with_layout(
            egui::vec2(width, 20.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(name).size(10.5).color(if disabled {
                        MUTED
                    } else {
                        TEXT
                    }))
                    .truncate(),
                )
                .on_hover_text(detail);
            },
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // A world is a whole save directory, so it keeps a confirmation
            // step. Individual files are one click, matching instance erase.
            if !is_dir {
                if danger_button(ui, "Erase")
                    .on_hover_text("Delete this file now. No confirmation.")
                    .clicked()
                {
                    request.file_action = Some((ModFileAction::Delete, path.to_path_buf(), false));
                }
            } else if confirm.as_deref() == Some(name) {
                if ui.small_button("Erase").clicked() {
                    request.file_action = Some((ModFileAction::Delete, path.to_path_buf(), true));
                }
                if ui.small_button("Keep").clicked() {
                    request.cancel_delete = true;
                }
            } else if ui.small_button("Erase").clicked() {
                request.arm_delete = Some(name.to_owned());
            }
        });
    });
}

fn short_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// "Added 2 mods, 1 resource pack" — one precise line for a mixed batch.
fn added_summary(counts: &[(ContentKind, usize)]) -> String {
    let mut parts = Vec::with_capacity(counts.len());
    for (kind, count) in counts {
        let noun = match (count, kind) {
            (1, ContentKind::Mod) => "mod",
            (_, ContentKind::Mod) => "mods",
            (1, ContentKind::ResourcePack) => "resource pack",
            (_, ContentKind::ResourcePack) => "resource packs",
            (1, ContentKind::Shader) => "shader pack",
            (_, ContentKind::Shader) => "shader packs",
            (1, ContentKind::Modpack) => "modpack",
            (_, ContentKind::Modpack) => "modpacks",
        };
        parts.push(format!("{count} {noun}"));
    }
    format!("Added {}", parts.join(", "))
}

fn chip(ui: &mut egui::Ui, text: &str, color: egui::Color32) -> egui::Response {
    egui::Frame::none()
        .fill(egui::Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            24,
        ))
        .rounding(egui::Rounding::same(CORNER_SM))
        .inner_margin(egui::Margin::symmetric(6.0, 1.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(9.5).strong().color(color))
        })
        .inner
}

/// A quiet action for banners and secondary rows: no box at rest, a faint
/// surface on hover, and a compact text height. Keeping it a real Button means
/// focus, tooltips, and accessibility still work.
fn ghost_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let previous = ui.style().visuals.widgets.clone();
    {
        let widgets = &mut ui.style_mut().visuals.widgets;
        for (widget, fill_alpha, foreground) in [
            (&mut widgets.inactive, 0, MUTED),
            (&mut widgets.hovered, 10, TEXT),
            (&mut widgets.active, 18, TEXT),
        ] {
            widget.weak_bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, fill_alpha);
            widget.bg_fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, fill_alpha);
            widget.bg_stroke = egui::Stroke::NONE;
            widget.fg_stroke = egui::Stroke::new(1.0_f32, foreground);
        }
    }
    let response = ui
        .add(
            egui::Button::new(egui::RichText::new(text).size(10.5))
                .min_size(egui::vec2(0.0, CONTROL_HEIGHT))
                .rounding(egui::Rounding::same(CONTROL_RADIUS)),
        )
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.style_mut().visuals.widgets = previous;
    response
}

/// The two portable-instance directions, drawn as one icon family: a tray
/// with an arrow entering (import) or leaving (export). Strokes instead of
/// glyphs keep both crisp at any DPI and free of font fallback surprises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PortableGlyph {
    Import,
    Export,
}

/// Width of the square the tray-and-arrow icon is drawn in.
const GLYPH_BOX: f32 = 11.0;

fn draw_portable_glyph(
    painter: &egui::Painter,
    rect: egui::Rect,
    glyph: PortableGlyph,
    ink: egui::Color32,
) {
    let stroke = egui::Stroke::new(1.4_f32, ink);
    let centre = rect.center();
    let half = GLYPH_BOX / 2.0;
    let tray_top = centre.y + 0.5;
    let tray_bottom = centre.y + half;
    painter.line_segment(
        [
            egui::pos2(centre.x - half, tray_top),
            egui::pos2(centre.x - half, tray_bottom),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(centre.x - half, tray_bottom),
            egui::pos2(centre.x + half, tray_bottom),
        ],
        stroke,
    );
    painter.line_segment(
        [
            egui::pos2(centre.x + half, tray_top),
            egui::pos2(centre.x + half, tray_bottom),
        ],
        stroke,
    );
    // Arrow tip: down into the tray for import, up and out for export.
    let (tail, tip, wing) = match glyph {
        PortableGlyph::Import => (
            egui::pos2(centre.x, centre.y - half),
            egui::pos2(centre.x, centre.y + 0.5),
            1.0_f32,
        ),
        PortableGlyph::Export => (
            egui::pos2(centre.x, centre.y + 0.5),
            egui::pos2(centre.x, centre.y - half),
            -1.0_f32,
        ),
    };
    painter.line_segment([tail, tip], stroke);
    let head = 3.2_f32;
    painter.line_segment([tip, tip + egui::vec2(-head, -wing * head)], stroke);
    painter.line_segment([tip, tip + egui::vec2(head, -wing * head)], stroke);
}

/// One compact icon action. With a label it reads as a named control; without
/// one it shrinks to a square, which is how the density-heavy instance card
/// keeps its footer to a single row.
fn glyph_button(
    ui: &mut egui::Ui,
    glyph: PortableGlyph,
    label: Option<&str>,
    enabled: bool,
    detail: &str,
) -> egui::Response {
    let font = egui::FontId::proportional(11.0);
    let text_width = label.map_or(0.0, |label| {
        ui.fonts(|fonts| {
            fonts
                .layout_no_wrap(label.to_owned(), font.clone(), TEXT)
                .size()
                .x
        })
    });
    let gap = if label.is_some() { 5.0 } else { 0.0 };
    let padding = if label.is_some() { 10.0 } else { 6.5 };
    let width = padding + GLYPH_BOX + gap + text_width + padding;
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    // Height follows the theme's interaction size, so the pair lines up with
    // the text buttons in the same row instead of floating a pixel short.
    let height = ui.spacing().interact_size.y;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), sense);
    if ui.is_rect_visible(rect) {
        let widgets = ui.style().visuals.widgets.clone();
        let widget = if !enabled {
            widgets.noninteractive
        } else if response.is_pointer_button_down_on() {
            widgets.active
        } else if response.hovered() {
            widgets.hovered
        } else {
            widgets.inactive
        };
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            egui::Rounding::same(CONTROL_RADIUS),
            widget.weak_bg_fill,
        );
        painter.rect_stroke(rect, egui::Rounding::same(CONTROL_RADIUS), widget.bg_stroke);
        let ink = if !enabled {
            MUTED.gamma_multiply(0.55)
        } else if response.hovered() {
            TEXT
        } else {
            MUTED
        };
        let icon = egui::Rect::from_center_size(
            egui::pos2(rect.left() + padding + GLYPH_BOX / 2.0, rect.center().y),
            egui::vec2(GLYPH_BOX, GLYPH_BOX),
        );
        draw_portable_glyph(painter, icon, glyph, ink);
        if let Some(label) = label {
            painter.text(
                egui::pos2(icon.right() + gap, rect.center().y),
                egui::Align2::LEFT_CENTER,
                label,
                font,
                if enabled { TEXT } else { ink },
            );
        }
    }
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, enabled, label.unwrap_or(detail))
    });
    if enabled {
        response
            .on_hover_text(detail)
            .on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        response.on_hover_text(detail)
    }
}

fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    // Drive the accent through the widget states instead of `Button::fill`, so
    // hover and press actually answer the pointer.
    let previous = ui.style().visuals.widgets.clone();
    {
        let widgets = &mut ui.style_mut().visuals.widgets;
        for (widget, fill, stroke) in [
            (&mut widgets.inactive, accent(), accent()),
            (
                &mut widgets.hovered,
                egui::Color32::from_rgb(112, 158, 252),
                accent_soft(),
            ),
            (
                &mut widgets.active,
                egui::Color32::from_rgb(74, 123, 228),
                accent_soft(),
            ),
        ] {
            widget.weak_bg_fill = fill;
            widget.bg_fill = fill;
            widget.bg_stroke = egui::Stroke::new(1.0_f32, stroke);
        }
    }
    let response = ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .size(11.0)
                .strong()
                .color(accent_on()),
        )
        .rounding(egui::Rounding::same(CONTROL_RADIUS))
        .min_size(egui::vec2(98.0, CONTROL_HEIGHT)),
    );
    ui.style_mut().visuals.widgets = previous;
    response
}

/// The action that ends a running game: the same states and geometry as
/// [`primary_button`], in the one colour that means "this stops something".
/// Driving the widget states keeps hover and press feedback while the fill
/// stays red, which a fixed `Button::fill` cannot do.
fn quit_button(ui: &mut egui::Ui, text: &str, min_size: egui::Vec2) -> egui::Response {
    let previous = ui.style().visuals.widgets.clone();
    {
        let widgets = &mut ui.style_mut().visuals.widgets;
        for (widget, fill, stroke) in [
            (&mut widgets.inactive, DANGER, DANGER),
            (
                &mut widgets.hovered,
                egui::Color32::from_rgb(255, 138, 142),
                egui::Color32::from_rgb(255, 190, 192),
            ),
            (
                &mut widgets.active,
                egui::Color32::from_rgb(226, 88, 94),
                egui::Color32::from_rgb(255, 190, 192),
            ),
        ] {
            widget.weak_bg_fill = fill;
            widget.bg_fill = fill;
            widget.bg_stroke = egui::Stroke::new(1.0_f32, stroke);
        }
    }
    let response = ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .size(11.0)
                .strong()
                .color(accent_on()),
        )
        .rounding(egui::Rounding::same(CONTROL_RADIUS))
        .min_size(min_size),
    );
    ui.style_mut().visuals.widgets = previous;
    response
}

/// Secondary outlined action: visible at rest, brighter on hover. Same
/// geometry as [`primary_button`], so a pair of actions lines up exactly.
fn outline_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let previous = ui.style().visuals.widgets.clone();
    {
        let widgets = &mut ui.style_mut().visuals.widgets;
        for (widget, fill_alpha) in [
            (&mut widgets.inactive, 10),
            (&mut widgets.hovered, 24),
            (&mut widgets.active, 34),
        ] {
            let fill = egui::Color32::from_rgba_unmultiplied(255, 255, 255, fill_alpha);
            widget.weak_bg_fill = fill;
            widget.bg_fill = fill;
            widget.bg_stroke = egui::Stroke::new(1.0_f32, BORDER_BRIGHT);
        }
    }
    let response = ui.add(
        egui::Button::new(egui::RichText::new(text).size(11.0).color(TEXT))
            .min_size(egui::vec2(0.0, CONTROL_HEIGHT))
            .rounding(egui::Rounding::same(CONTROL_RADIUS)),
    );
    ui.style_mut().visuals.widgets = previous;
    response
}

/// One clickable tally on the Overview board. Returns whether it was clicked;
/// the caller owns the navigation because the board is rebuilt every frame.
fn content_stat_card(ui: &mut egui::Ui, kind: ContentKind, count: usize) -> bool {
    let card = KaelisApp::glass_frame().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.label(
            egui::RichText::new(kind.label().to_ascii_uppercase())
                .size(8.0)
                .strong()
                .color(kind_color(kind)),
        );
        ui.label(egui::RichText::new(count.to_string()).size(20.0).strong());
        ui.label(
            egui::RichText::new("open in Content")
                .size(9.0)
                .color(MUTED),
        );
    });
    card.response
        .interact(egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(format!("Manage {}", kind.label().to_ascii_lowercase()))
        .clicked()
}

fn account_summary_card(ui: &mut egui::Ui, name: &str, kind: &str) -> bool {
    let card = KaelisApp::glass_frame().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(
            egui::RichText::new("ACCOUNT")
                .size(8.0)
                .strong()
                .color(MUTED),
        );
        ui.label(egui::RichText::new(name).size(15.0).strong());
        ui.label(egui::RichText::new(kind).size(9.5).color(MUTED));
        ui.add_space(2.0);
        // A top-down layout stretches a bare button to the card width; the
        // horizontal wrapper keeps the action the size of its label.
        ui.horizontal(|ui| ui.small_button("Manage account"))
            .inner
            .clicked()
    });
    card.inner
}

fn release_summary_card(ui: &mut egui::Ui, latest: &str) {
    KaelisApp::glass_frame().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        // Sits beside the account card, whose action button makes it taller.
        // Matching the inner height keeps the two bottoms on one line.
        ui.set_min_height(71.0);
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.label(
            egui::RichText::new("LATEST RELEASE")
                .size(8.0)
                .strong()
                .color(MUTED),
        );
        ui.label(egui::RichText::new(latest).size(15.0).strong());
        ui.label(
            egui::RichText::new("Official Mojang version manifest")
                .size(9.5)
                .color(MUTED),
        );
    });
}

fn editor_section(ui: &mut egui::Ui, title: &str, hint: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(SURFACE_ALT)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .rounding(egui::Rounding::same(CORNER_MD))
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(title).size(10.0).strong().color(MUTED));
            if !hint.is_empty() {
                ui.label(egui::RichText::new(hint).size(9.5).color(MUTED));
            }
            ui.add_space(5.0);
            add(ui);
        });
    ui.add_space(8.0);
}

fn editor_row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [64.0, 20.0],
            egui::Label::new(egui::RichText::new(label).size(10.0).color(MUTED)),
        );
        add(ui);
    });
    ui.add_space(3.0);
}

fn draw_project_icon(
    ui: &mut egui::Ui,
    texture: Option<&egui::TextureHandle>,
    item: &ModSearchItem,
    size: f32,
) {
    let dimensions = egui::vec2(size, size);
    if let Some(texture) = texture {
        ui.add(
            egui::Image::new((texture.id(), dimensions)).rounding(egui::Rounding::same(CORNER_MD)),
        );
        return;
    }
    let (rect, _) = ui.allocate_exact_size(dimensions, egui::Sense::hover());
    ui.painter().rect_filled(rect, CORNER_MD, SURFACE_HOVER);
    ui.painter().rect_stroke(
        rect,
        CORNER_MD,
        egui::Stroke::new(1.0_f32, kind_color(item.kind)),
    );
    let initial = item
        .title
        .chars()
        .find(|character| !character.is_whitespace())
        .unwrap_or('?');
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        initial,
        egui::FontId::proportional(size * 0.42),
        kind_color(item.kind),
    );
}

fn draw_mod_icon(ui: &mut egui::Ui, texture: Option<&egui::TextureHandle>, item: &ModSearchItem) {
    let size = egui::vec2(MOD_ICON_SIZE, MOD_ICON_SIZE);
    if let Some(texture) = texture {
        ui.add(egui::Image::new((texture.id(), size)).rounding(egui::Rounding::same(CORNER_SM)));
        return;
    }
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let fill = match item.provider {
        ModProvider::Modrinth => egui::Color32::from_rgb(35, 160, 90),
        ModProvider::CurseForge => egui::Color32::from_rgb(214, 118, 42),
    };
    ui.painter().rect_filled(rect, 8.0, fill);
    let initial = item
        .title
        .chars()
        .find(|character| !character.is_whitespace())
        .unwrap_or('?');
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        initial,
        egui::FontId::proportional(20.0),
        egui::Color32::WHITE,
    );
}

/// Fetch a pack's project picture for the new instance card. Bounded and
/// best-effort: a missing or slow icon never delays a successful import.
async fn fetch_pack_icon(client: &reqwest::Client, url: &str) -> Option<Vec<u8>> {
    let parsed = reqwest::Url::parse(url).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    let response = client
        .get(parsed)
        .timeout(std::time::Duration::from_secs(6))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    if response
        .content_length()
        .is_some_and(|length| length > INSTANCE_ICON_BYTES)
    {
        return None;
    }
    let mut bytes = Vec::new();
    let mut response = response;
    while let Some(chunk) = response.chunk().await.ok()? {
        if bytes.len() + chunk.len() > INSTANCE_ICON_BYTES as usize {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    Some(bytes)
}

async fn fetch_mod_icon(client: &reqwest::Client, url: String) -> Result<egui::ColorImage, String> {
    let response = client
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    if response
        .content_length()
        .is_some_and(|length| length > MOD_ICON_BYTES as u64)
    {
        return Err("mod icon exceeds the 1 MiB limit".to_owned());
    }
    let bytes = response.bytes().await.map_err(|error| error.to_string())?;
    if bytes.len() > MOD_ICON_BYTES {
        return Err("mod icon exceeds the 1 MiB limit".to_owned());
    }
    tokio::task::spawn_blocking(move || decode_mod_icon(&bytes))
        .await
        .map_err(|error| error.to_string())?
}

/// Instance cards are square, so crop wide or tall artwork through the centre
/// instead of stretching it into a distorted avatar.
fn center_square(image: egui::ColorImage) -> egui::ColorImage {
    let [width, height] = image.size;
    let side = width.min(height);
    if width == height || side == 0 {
        return image;
    }
    let left = (width - side) / 2;
    let top = (height - side) / 2;
    let mut pixels = Vec::with_capacity(side * side);
    for row in 0..side {
        let start = (row + top) * width + left;
        pixels.extend_from_slice(&image.pixels[start..start + side]);
    }
    egui::ColorImage {
        size: [side, side],
        pixels,
    }
}

fn is_image_path(path: &Path) -> bool {
    ["png", "jpg", "jpeg", "webp", "gif", "bmp", "ico", "svg"]
        .iter()
        .any(|extension| has_extension(path, extension))
}

/// SVG is text, so it is recognised by its tags instead of a binary signature.
/// Compressed `.svgz` stays out: a second decoder buys nothing over saving the
/// picture as PNG.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = bytes.get(..1024.min(bytes.len())).unwrap_or(bytes);
    String::from_utf8_lossy(head)
        .to_ascii_lowercase()
        .contains("<svg")
}

/// Rasterise SVG artwork once, at import size. Only the visible box is
/// rendered, so a document-sized drawing cannot allocate a canvas.
fn render_svg_artwork(bytes: &[u8], edge: u32) -> Result<image::RgbaImage, String> {
    use resvg::tiny_skia;
    let options = resvg::usvg::Options {
        fontdb: svg_fonts(),
        ..Default::default()
    };
    let tree = resvg::usvg::Tree::from_data(bytes, &options)
        .map_err(|error| format!("The SVG could not be read: {error}"))?;
    let size = tree.size();
    let (width, height) = (size.width(), size.height());
    if !(width.is_finite() && height.is_finite()) || width <= 0.0 || height <= 0.0 {
        return Err("The SVG has no visible size.".to_owned());
    }
    let edge = edge.max(1);
    let scale = (edge as f32 / width.max(height)).min(64.0);
    let pixel_width = ((width * scale).ceil() as u32).clamp(1, edge);
    let pixel_height = ((height * scale).ceil() as u32).clamp(1, edge);
    let mut pixmap = tiny_skia::Pixmap::new(pixel_width, pixel_height)
        .ok_or_else(|| "The SVG is too large to render.".to_owned())?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    image::RgbaImage::from_raw(pixel_width, pixel_height, pixmap.data().to_vec())
        .ok_or_else(|| "The SVG produced no pixels.".to_owned())
}

/// System fonts for SVG text, loaded once per process and then shared.
fn svg_fonts() -> std::sync::Arc<resvg::usvg::fontdb::Database> {
    static FONTS: std::sync::OnceLock<std::sync::Arc<resvg::usvg::fontdb::Database>> =
        std::sync::OnceLock::new();
    std::sync::Arc::clone(FONTS.get_or_init(|| {
        let mut database = resvg::usvg::fontdb::Database::new();
        database.load_system_fonts();
        std::sync::Arc::new(database)
    }))
}

/// Decode any accepted picture (raster or SVG) into RGBA with bounded memory.
fn decode_any_image(bytes: &[u8], edge: u32) -> Result<image::RgbaImage, String> {
    if looks_like_svg(bytes) {
        return render_svg_artwork(bytes, edge);
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "That file is not a readable image.".to_owned())?;
    let mut limits = image::Limits::no_limits();
    limits.max_image_width = Some(MOD_ICON_MAX_EDGE);
    limits.max_image_height = Some(MOD_ICON_MAX_EDGE);
    limits.max_alloc = Some(MOD_ICON_MAX_ALLOC);
    reader.limits(limits);
    reader
        .decode()
        .map_err(|error| format!("The image could not be decoded: {error}"))
        .map(|image| image.to_rgba8())
}

/// Decode an inline image: a `data:` URL, a bare base64 payload, or pasted SVG
/// markup. Text that looks like a path is left alone, so a normal file path is
/// never decoded.
fn decode_image_payload(text: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    let trimmed = text.trim();
    // Pasted SVG markup is already the picture; nothing to decode.
    if looks_like_svg(trimmed.as_bytes()) {
        return Some(trimmed.as_bytes().to_vec());
    }
    let payload = if let Some(rest) = trimmed.strip_prefix("data:") {
        let (meta, body) = rest.split_once(',')?;
        if meta.contains(";base64") {
            body
        } else if meta.contains("svg") {
            // Only text formats can travel unencoded; a binary data URL that
            // is not base64 is a broken link, not a picture.
            return percent_decode(body).filter(|bytes| looks_like_svg(bytes));
        } else {
            return None;
        }
    } else {
        // A filename always has a separator or a dot; base64 never does.
        if trimmed.len() < 64 || trimmed.contains(['/', '\\', '.', '<']) {
            return None;
        }
        trimmed
    };
    let compact = payload
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect::<String>();
    use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
    [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD]
        .into_iter()
        .find_map(|engine| engine.decode(&compact).ok())
        .filter(|bytes| bytes.len() > 8)
}

/// Decode `%2F`-style escapes in an unencoded `data:` URL.
fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = bytes.get(index + 1..index + 3)?;
            let value = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            out.push(value);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    Some(out)
}

/// Read the picture on the system clipboard as PNG bytes.
fn clipboard_picture() -> Result<Vec<u8>, String> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|error| format!("The clipboard is not available: {error}"))?;
    let picture = clipboard
        .get_image()
        .map_err(|_| "The clipboard holds no picture. Copy an image first.".to_owned())?;
    let width = u32::try_from(picture.width).ok().filter(|width| *width > 0);
    let height = u32::try_from(picture.height)
        .ok()
        .filter(|height| *height > 0);
    let (Some(width), Some(height)) = (width, height) else {
        return Err("The clipboard picture is empty.".to_owned());
    };
    if u64::from(width) * u64::from(height) > u64::from(MOD_ICON_MAX_EDGE).pow(2) {
        return Err(format!(
            "The clipboard picture is too large ({width} x {height})."
        ));
    }
    let buffer = image::RgbaImage::from_raw(width, height, picture.bytes.into_owned())
        .ok_or_else(|| "The clipboard picture is malformed.".to_owned())?;
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| format!("Could not read the clipboard picture: {error}"))?;
    Ok(png)
}

/// Read clipboard text, for pasting a shared instance.
fn clipboard_text() -> Result<String, String> {
    let mut clipboard = arboard::Clipboard::new()
        .map_err(|error| format!("The clipboard is not available: {error}"))?;
    clipboard
        .get_text()
        .map_err(|_| "The clipboard holds no text.".to_owned())
}

/// Files copied in Explorer or Finder. Windows exposes them as `CF_HDROP`, which
/// is not text: a text field would otherwise paste nothing at all.
#[cfg(windows)]
fn clipboard_files() -> Vec<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows_sys::Win32::UI::Shell::{DragQueryFileW, HDROP};
    const CF_HDROP: u32 = 15;
    // SAFETY: every Win32 call is checked, the clipboard is closed on every
    // path after a successful open, and DragQueryFileW writes into a buffer
    // sized from the length it just reported.
    unsafe {
        if IsClipboardFormatAvailable(CF_HDROP) == 0 {
            return Vec::new();
        }
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return Vec::new();
        }
        let mut files = Vec::new();
        let handle = GetClipboardData(CF_HDROP);
        if !handle.is_null() {
            let drop = handle as HDROP;
            let count = DragQueryFileW(drop, u32::MAX, std::ptr::null_mut(), 0);
            for index in 0..count {
                let length = DragQueryFileW(drop, index, std::ptr::null_mut(), 0);
                if length == 0 {
                    continue;
                }
                let mut buffer = vec![0_u16; length as usize + 1];
                let written = DragQueryFileW(drop, index, buffer.as_mut_ptr(), length + 1) as usize;
                if written == 0 {
                    continue;
                }
                buffer.truncate(written);
                files.push(PathBuf::from(std::ffi::OsString::from_wide(&buffer)));
            }
        }
        CloseClipboard();
        files
    }
}

#[cfg(not(windows))]
fn clipboard_files() -> Vec<PathBuf> {
    // X11 and Wayland expose file lists as `text/uri-list`; a text paste already
    // carries the path, so nothing extra is read here.
    Vec::new()
}

/// What a pasted clipboard should become in a text field: its text, or the
/// name of a copied file.
fn clipboard_query_text() -> Option<String> {
    if let Ok(text) = clipboard_text() {
        let text = text.trim();
        if !text.is_empty() {
            return Some(text.to_owned());
        }
    }
    clipboard_files().first().map(|path| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    })
}

/// The first image among the copied files, if any.
fn clipboard_artwork_file() -> Option<PathBuf> {
    let files = clipboard_files();
    files.into_iter().find(|path| is_image_path(path))
}

/// Read an instance record pasted from a chat, a ticket, or another machine.
/// Markdown fences are stripped because that is how JSON usually arrives.
fn parse_instance_json(text: &str) -> Option<Instance> {
    let trimmed = text.trim();
    let trimmed = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed)
        .trim();
    let trimmed = trimmed.strip_suffix("```").unwrap_or(trimmed).trim();
    serde_json::from_str(trimmed).ok()
}

/// One size for stored artwork: a 256 px square PNG. Every accepted source is
/// cropped to its centre and resized once, so the launcher never decodes a
/// multi-megabyte picture at paint time.
fn store_instance_artwork(root: &Path, bytes: &[u8]) -> Result<PathBuf, String> {
    if bytes.len() as u64 > INSTANCE_ICON_BYTES {
        return Err("Artwork is larger than 8 MiB.".to_owned());
    }
    let decoded = decode_any_image(bytes, MOD_ICON_MAX_EDGE)?;
    let (width, height) = decoded.dimensions();
    if width == 0 || height == 0 {
        return Err("The image has no pixels.".to_owned());
    }
    let side = width.min(height);
    let cropped = image::imageops::crop_imm(
        &decoded,
        (width - side) / 2,
        (height - side) / 2,
        side,
        side,
    )
    .to_image();
    let square = image::imageops::resize(
        &cropped,
        ARTWORK_EDGE,
        ARTWORK_EDGE,
        image::imageops::FilterType::Lanczos3,
    );
    let directory = root.join(".kaelis");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not create the artwork folder: {error}"))?;
    let path = directory.join("artwork.png");
    square
        .save(&path)
        .map_err(|error| format!("Could not write the artwork: {error}"))?;
    Ok(path)
}

/// Store a pack's picture as the new instance's card artwork. A bad or
/// oversized picture never fails the pack import; the monogram stays.
fn apply_pack_artwork(instance: &mut Instance, bytes: &[u8]) -> bool {
    match store_instance_artwork(&instance.root, bytes) {
        Ok(path) => {
            instance.icon = Some(path);
            true
        }
        Err(_) => false,
    }
}

fn decode_instance_icon(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    decode_mod_icon(bytes).map(center_square)
}

fn decode_mod_icon(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let image = decode_any_image(bytes, MOD_ICON_MAX_EDGE)?;
    let thumbnail = image::imageops::thumbnail(&image, 96, 96);
    let size = [thumbnail.width() as usize, thumbnail.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(
        size,
        thumbnail.as_raw(),
    ))
}

fn show_qr(ui: &mut egui::Ui, device: &DeviceCodeResponse) {
    let uri = device
        .verification_uri_complete
        .as_deref()
        .unwrap_or(&device.verification_uri);
    let Ok(code) = qrcode::QrCode::new(uri.as_bytes()) else {
        return;
    };
    let colors = code.to_colors();
    let modules = code.width();
    let side = 132.0;
    let quiet_zone = 4.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    ui.painter().rect_filled(rect, 5.0, egui::Color32::WHITE);
    let cell = side / (modules as f32 + quiet_zone * 2.0);
    for y in 0..modules {
        for x in 0..modules {
            if colors[y * modules + x] == qrcode::types::Color::Dark {
                let min = rect.min
                    + egui::vec2(
                        (x as f32 + quiet_zone) * cell,
                        (y as f32 + quiet_zone) * cell,
                    );
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(min, egui::vec2(cell, cell)),
                    0.0,
                    egui::Color32::from_rgb(12, 18, 29),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ARTWORK_EDGE, CheckState, HandoffRequest, InstalledFile, InstalledScan, InstanceHealth,
        KaelisApp, ModSort, ModView, StatusKind, apply_pack_artwork, auto_scale_for,
        auto_scale_for_settled, build_mod_rows, center_square, clipboard_picture,
        content_list_text, count_noun, decode_image_payload, decode_instance_icon, decode_mod_icon,
        draw_instance_avatar, draw_mod_icon, expand_dropped, filter_tokens, has_extension,
        https_icon_url, install_state_key, instance_content_summary, instance_matches,
        instance_name_conflict, looks_like_svg, merge_launch_command, mod_item_key, note_preview,
        parse_handoff, parse_instance_json, parse_jvm_args, relative_last_played, scan_installed,
        significant_tokens, slugify, store_instance_artwork, valid_instance_version,
        visible_mod_indices, window_icon,
    };
    use kaelis_api::accounts::LauncherSettings;
    use kaelis_api::mods::{ContentKind, ModProvider, ModSearchItem};
    use kaelis_core::instance::Instance;
    use std::path::PathBuf;

    fn item(provider: ModProvider, id: &str) -> ModSearchItem {
        ModSearchItem {
            provider,
            id: id.to_owned(),
            slug: String::new(),
            title: "Example".to_owned(),
            description: String::new(),
            downloads: 0,
            icon_url: None,
            ..ModSearchItem::default()
        }
    }

    #[test]
    fn instance_content_counts_read_naturally() {
        assert_eq!(count_noun(0, "mod", "mods"), "0 mods");
        assert_eq!(count_noun(1, "mod", "mods"), "1 mod");
        assert_eq!(count_noun(2, "world", "worlds"), "2 worlds");

        let root = scratch_dir("content-summary");
        std::fs::create_dir_all(root.join("mods")).unwrap();
        std::fs::create_dir_all(root.join("resourcepacks")).unwrap();
        std::fs::create_dir_all(root.join("shaderpacks")).unwrap();
        std::fs::create_dir_all(root.join("saves/one")).unwrap();
        std::fs::write(root.join("mods/sodium.jar"), b"jar").unwrap();
        std::fs::write(root.join("resourcepacks/a.zip"), b"zip").unwrap();
        std::fs::write(root.join("resourcepacks/b.zip.disabled"), b"zip").unwrap();
        std::fs::write(root.join("mods/notes.txt"), b"not content").unwrap();
        assert_eq!(
            instance_content_summary(&root),
            "1 mod · 2 packs · 0 shaders · 1 world"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn an_unplayed_instance_has_no_played_label() {
        // A brand-new instance stays visually quiet: the card shows its content
        // and nothing about a session that never happened.
        assert!(relative_last_played(0).is_empty());
        assert!(relative_last_played(-1).is_empty());
        // Once it has been played the relative label is still there.
        assert!(!relative_last_played(1).is_empty());
    }

    fn search_item(
        provider: ModProvider,
        slug: &str,
        title: &str,
        downloads: u64,
    ) -> ModSearchItem {
        ModSearchItem {
            provider,
            id: format!("id-{slug}"),
            slug: slug.to_owned(),
            title: title.to_owned(),
            description: format!("{title} description"),
            downloads,
            icon_url: None,
            ..ModSearchItem::default()
        }
    }

    fn test_view(sort: ModSort, modrinth: bool, curseforge: bool) -> ModView {
        ModView {
            sort,
            show_modrinth: modrinth,
            show_curseforge: curseforge,
        }
    }

    fn installed_scan(file_name: &str, id: &str) -> InstalledScan {
        InstalledScan {
            files: vec![InstalledFile {
                name: file_name.to_owned(),
                path: PathBuf::from(file_name),
                tokens: significant_tokens(file_name),
                ids: vec![id.to_owned()],
                primary_ids: vec![id.to_owned()],
                requires: Vec::new(),
                disabled: false,
            }],
            summary: String::new(),
            missing: Vec::new(),
            conflicts: Vec::new(),
            detected_loader: None,
        }
    }

    #[test]
    fn empty_launch_restores_the_window() {
        assert_eq!(parse_handoff(&[]), HandoffRequest::Activate);
    }

    #[test]
    fn play_argument_selects_the_instance() {
        let arguments = ["--play".to_owned(), "fabulous-1-21".to_owned()];
        assert_eq!(
            parse_handoff(&arguments),
            HandoffRequest::Play {
                instance_id: "fabulous-1-21".to_owned(),
                offline_name: None,
            }
        );
    }

    #[test]
    fn play_argument_carries_an_offline_identity() {
        let arguments = [
            "--play".to_owned(),
            "fabulous-1-21".to_owned(),
            "--offline".to_owned(),
            "Alex".to_owned(),
        ];
        assert_eq!(
            parse_handoff(&arguments),
            HandoffRequest::Play {
                instance_id: "fabulous-1-21".to_owned(),
                offline_name: Some("Alex".to_owned()),
            }
        );
    }

    #[test]
    fn offline_without_a_name_is_rejected() {
        let arguments = [
            "--play".to_owned(),
            "fabulous-1-21".to_owned(),
            "--offline".to_owned(),
        ];
        assert_eq!(parse_handoff(&arguments), HandoffRequest::Unsupported);
    }

    #[test]
    fn play_without_an_instance_is_rejected() {
        assert_eq!(
            parse_handoff(&["--play".to_owned()]),
            HandoffRequest::Unsupported
        );
    }

    #[test]
    fn decodes_and_thumbnails_png_icons() {
        let source = image::RgbaImage::from_pixel(128, 128, image::Rgba([12, 34, 56, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        source.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let icon = decode_mod_icon(png.get_ref()).unwrap();
        assert_eq!(icon.size, [96, 96]);
        assert_eq!(
            icon.pixels[0],
            egui::Color32::from_rgba_unmultiplied(12, 34, 56, 255)
        );
    }

    #[test]
    fn installed_scan_matches_slug_sequences() {
        let scan = InstalledScan {
            files: vec![InstalledFile {
                name: "sodium-fabric-0.5.11.jar".to_owned(),
                path: PathBuf::from("sodium-fabric-0.5.11.jar"),
                tokens: significant_tokens("sodium-fabric-0.5.11"),
                ids: vec!["sodium".to_owned()],
                primary_ids: vec!["sodium".to_owned()],
                requires: Vec::new(),
                disabled: false,
            }],
            summary: String::new(),
            missing: Vec::new(),
            conflicts: Vec::new(),
            detected_loader: None,
        };
        assert_eq!(
            scan.match_file("sodium", &significant_tokens("sodium"))
                .map(|file| file.name.as_str()),
            Some("sodium-fabric-0.5.11.jar")
        );
        assert!(
            scan.match_file("sodium-extra", &significant_tokens("sodium-extra"))
                .is_none()
        );
        assert!(
            scan.match_file("lithium", &significant_tokens("lithium"))
                .is_none()
        );
    }

    #[test]
    fn installed_scan_prefers_enabled_files() {
        let file = |name: &str, disabled| InstalledFile {
            name: name.to_owned(),
            path: PathBuf::from(name),
            tokens: significant_tokens("sodium"),
            ids: vec!["sodium".to_owned()],
            primary_ids: vec!["sodium".to_owned()],
            requires: Vec::new(),
            disabled,
        };
        let scan = InstalledScan {
            files: vec![file("sodium-disabled.jar", true), file("sodium.jar", false)],
            summary: String::new(),
            missing: Vec::new(),
            conflicts: Vec::new(),
            detected_loader: None,
        };
        let matched = scan
            .match_file("sodium", &significant_tokens("sodium"))
            .unwrap();
        assert_eq!(matched.name, "sodium.jar");
        assert!(!matched.disabled);
    }

    #[test]
    fn files_matching_returns_every_jar_with_the_project_id() {
        let mut scan = installed_scan("sodium-0.5.jar", "sodium");
        scan.files.push(InstalledFile {
            name: "sodium-0.4.jar".to_owned(),
            path: PathBuf::from("sodium-0.4.jar"),
            tokens: significant_tokens("sodium-0.4.jar"),
            ids: vec!["sodium".to_owned()],
            primary_ids: vec!["sodium".to_owned()],
            requires: Vec::new(),
            disabled: true,
        });
        scan.files.push(InstalledFile {
            name: "sodium-extra.jar".to_owned(),
            path: PathBuf::from("sodium-extra.jar"),
            tokens: significant_tokens("sodium-extra.jar"),
            ids: vec!["sodiumextra".to_owned()],
            primary_ids: vec!["sodiumextra".to_owned()],
            requires: Vec::new(),
            disabled: false,
        });
        let matched: Vec<&str> = scan
            .files_matching("sodium", &significant_tokens("sodium"))
            .into_iter()
            .map(|file| file.name.as_str())
            .collect();
        assert_eq!(matched, ["sodium-0.5.jar", "sodium-0.4.jar"]);
    }

    #[test]
    fn installed_scan_reads_fabric_metadata_ids() {
        use std::io::Write;

        let root = std::env::temp_dir().join(format!(
            "kaelis-mod-scan-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mods = root.join("mods");
        std::fs::create_dir_all(&mods).unwrap();
        let jar_path = mods.join("unrelated-filename.jar");
        let file = std::fs::File::create(&jar_path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("fabric.mod.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer
            .write_all(
                br#"{"id":"sodium","name":"Sodium","provides":["sodium-api"],"depends":{"minecraft":"1.21.x","fabricloader":">=0.14","ukulib":">=1.0"}}"#,
            )
            .unwrap();
        writer.finish().unwrap();

        let scan = scan_installed(&Instance::new(
            "scan-test",
            "Scan test",
            "1.21",
            "fabric",
            root.clone(),
        ));
        assert_eq!(
            scan.match_file("sodium", &significant_tokens("sodium"))
                .map(|file| file.name.as_str()),
            Some("unrelated-filename.jar")
        );
        assert!(
            scan.match_file("sodium-extra", &significant_tokens("sodium-extra"))
                .is_none()
        );
        assert!(scan.summary.contains("1 mod installed"));
        assert_eq!(scan.missing, vec!["ukulib".to_owned()]);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn search_rows_rank_local_matches_and_filters() {
        let items = vec![
            search_item(ModProvider::Modrinth, "sodium-extra", "Sodium Extra", 900),
            search_item(ModProvider::Modrinth, "sodium", "Sodium", 10),
            search_item(ModProvider::Modrinth, "lithium", "Lithium", 500),
        ];
        let rows = build_mod_rows(&items, "sodium");
        // Prefix match, exact match, then no match at all.
        assert_eq!(rows[0].relevance, 6);
        assert_eq!(rows[1].relevance, 7);
        assert_eq!(rows[2].relevance, 0);

        // The refine text is the only filter left: everything shows without it.
        let all = visible_mod_indices(&items, &rows, "", test_view(ModSort::Relevance, true, true));
        assert_eq!(all.len(), 3);

        let refined = visible_mod_indices(
            &items,
            &rows,
            "extra",
            test_view(ModSort::Relevance, true, true),
        );
        assert_eq!(refined, vec![0]);
    }

    #[test]
    fn automatic_scale_steps_are_coarse_monotonic_and_bounded() {
        // One step per display band, and the interface never shrinks as the
        // window grows.
        let mut previous = 0.0;
        for height in [320.0, 639.0, 640.0, 899.0, 900.0, 1149.0, 1150.0, 2160.0] {
            let scale = auto_scale_for(height);
            assert!(scale >= previous, "{height} px picked {scale}");
            assert!((0.85..=1.5).contains(&scale));
            previous = scale;
        }
        assert_eq!(auto_scale_for(520.0), 0.9);
        assert_eq!(auto_scale_for(720.0), 1.0);
        assert_eq!(auto_scale_for(1080.0), 1.05);
        assert_eq!(auto_scale_for(1440.0), 1.12);
    }

    #[test]
    fn automatic_scale_does_not_flap_around_a_boundary() {
        // Rising needs 24 points of clearance above the 900 boundary.
        assert_eq!(auto_scale_for_settled(910.0, 1.0), 1.0);
        assert_eq!(auto_scale_for_settled(924.0, 1.0), 1.05);
        // Falling needs the same clearance below it.
        assert_eq!(auto_scale_for_settled(890.0, 1.05), 1.05);
        assert_eq!(auto_scale_for_settled(876.0, 1.05), 1.0);
        // A jump across bands is accepted in the direction of travel.
        assert_eq!(auto_scale_for_settled(1400.0, 0.9), 1.12);
        assert_eq!(auto_scale_for_settled(500.0, 1.12), 0.9);
    }

    #[test]
    fn interface_scale_survives_settings_json() {
        let settings = LauncherSettings {
            ui_scale: 1.25,
            ..LauncherSettings::default()
        };
        let json = serde_json::to_string(&settings).expect("serialize settings");
        let restored: LauncherSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.ui_scale, 1.25);
        // Older files have no scale and must mean automatic, not 0%.
        let legacy: LauncherSettings =
            serde_json::from_str("{\"last_account\":\"\"}").expect("legacy settings");
        assert_eq!(legacy.ui_scale, 0.0);
    }

    #[test]
    fn launcher_settings_round_trip_the_content_channel() {
        let settings = LauncherSettings {
            content_kind: kaelis_api::mods::ContentKind::Shader,
            ..LauncherSettings::default()
        };
        let json = serde_json::to_string(&settings).expect("serialize settings");
        assert!(json.contains("\"content_kind\":\"shader\""));
        let restored: LauncherSettings = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.content_kind, kaelis_api::mods::ContentKind::Shader);
        // Files written before the channel was persisted must still load.
        let legacy: LauncherSettings =
            serde_json::from_str("{\"ms_client_id\":\"\",\"last_account\":\"\"}")
                .expect("legacy settings");
        assert_eq!(legacy.content_kind, kaelis_api::mods::ContentKind::Mod);
        // Files written before the interface zoom was removed still load; the
        // unknown key is ignored rather than rejecting every other setting.
        let with_zoom: LauncherSettings = serde_json::from_str(
            "{\"ui_zoom\":1.25,\"content_kind\":\"modpack\",\"auto_clean\":false}",
        )
        .expect("settings with the retired zoom key");
        assert_eq!(
            with_zoom.content_kind,
            kaelis_api::mods::ContentKind::Modpack
        );
    }

    #[test]
    fn content_list_marks_disabled_items() {
        let text = content_list_text(
            "Mods — test · Minecraft 26.3 · fabric",
            &[("sodium.jar", false), ("old.jar", true)],
        );
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("Mods — test · Minecraft 26.3 · fabric"));
        assert_eq!(lines.next(), Some("2 item(s) · 1 disabled"));
        assert_eq!(lines.next(), Some("- sodium.jar"));
        assert_eq!(lines.next(), Some("- [off] old.jar"));
        assert_eq!(lines.next(), None);
        assert_eq!(
            content_list_text("empty", &[]),
            "empty\n0 item(s) · 0 disabled\n"
        );
    }

    #[test]
    fn note_preview_collapses_whitespace_and_truncates() {
        assert_eq!(note_preview("  one\n two\tthree "), "one two three");
        let long = "word ".repeat(40);
        let preview = note_preview(&long);
        assert_eq!(preview.chars().count(), 111);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn drop_extensions_ignore_case_and_reject_partial_names() {
        use std::path::Path;
        assert!(has_extension(Path::new("Fabulous.MRPACK"), "mrpack"));
        assert!(has_extension(Path::new("pack.ZIP"), "zip"));
        assert!(!has_extension(Path::new("notes.txt"), "jar"));
        assert!(!has_extension(Path::new("jar"), "jar"));
    }

    #[test]
    fn classifies_picked_files_by_extension_and_channel() {
        use std::path::Path;
        assert_eq!(
            KaelisApp::classify_local_file(Path::new("sodium.jar"), ContentKind::Shader),
            Some(ContentKind::Mod)
        );
        assert_eq!(
            KaelisApp::classify_local_file(Path::new("pack.zip"), ContentKind::ResourcePack),
            Some(ContentKind::ResourcePack)
        );
        assert_eq!(
            KaelisApp::classify_local_file(Path::new("pack.zip"), ContentKind::Shader),
            Some(ContentKind::Shader)
        );
        assert_eq!(
            KaelisApp::classify_local_file(Path::new("fabulous.mrpack"), ContentKind::Mod),
            Some(ContentKind::Modpack)
        );
        // A disabled jar would be re-copied as `foo.jar.disabled.jar`, so it is
        // reported instead of silently producing a broken file name.
        assert_eq!(
            KaelisApp::classify_local_file(Path::new("foo.jar.disabled"), ContentKind::Mod),
            None
        );
        assert_eq!(
            KaelisApp::classify_local_file(Path::new("notes.txt"), ContentKind::Mod),
            None
        );
    }

    #[test]
    fn window_icon_is_square_with_a_transparent_corner() {
        let icon = window_icon();
        assert_eq!((icon.width, icon.height), (64, 64));
        assert_eq!(icon.rgba.len(), 64 * 64 * 4);
        assert_eq!(icon.rgba[3], 0, "corner must stay transparent");
        // The mark has no plate, so the point inside a plane is opaque and the
        // margin beside it stays clear.
        let plane = (26 * 64 + 32) * 4;
        assert_eq!(icon.rgba[plane + 3], 255, "glyph must be opaque");
        let margin = (32 * 64 + 2) * 4;
        assert_eq!(icon.rgba[margin + 3], 0, "no background plate");
    }

    #[test]
    fn provider_toggles_hide_their_cards() {
        let items = vec![
            search_item(ModProvider::Modrinth, "sodium", "Sodium", 10),
            search_item(ModProvider::CurseForge, "jei", "Just Enough Items", 20),
        ];
        let rows = build_mod_rows(&items, "");
        assert_eq!(
            visible_mod_indices(
                &items,
                &rows,
                "",
                test_view(ModSort::Downloads, true, false)
            ),
            vec![0]
        );
        assert_eq!(
            visible_mod_indices(
                &items,
                &rows,
                "",
                test_view(ModSort::Downloads, false, true)
            ),
            vec![1]
        );
    }

    #[test]
    fn modrinth_cards_are_listed_before_curseforge() {
        // Deliberately hostile order and counts: the mirror numbers are larger
        // and its rows arrive first. Modrinth still has to lead every sort.
        let items = vec![
            search_item(ModProvider::CurseForge, "jei", "Just Enough Items", 20),
            search_item(ModProvider::CurseForge, "create", "Create", 9_000),
            search_item(ModProvider::Modrinth, "sodium", "Sodium", 10),
            search_item(ModProvider::Modrinth, "lithium", "Lithium", 500),
        ];
        let rows = build_mod_rows(&items, "");
        for sort in [ModSort::Relevance, ModSort::Downloads, ModSort::Name] {
            let visible = visible_mod_indices(&items, &rows, "", test_view(sort, true, true));
            assert_eq!(visible, vec![3, 2, 1, 0], "{sort:?}");
        }
    }

    #[test]
    fn instance_filter_matches_every_token() {
        let mut instance = Instance::new(
            "fabulous-1-21",
            "Fabulous",
            "1.21.1",
            "fabric",
            PathBuf::from("kaelis-instance-filter-test"),
        );
        instance.group = "Favourites".to_owned();
        assert!(instance_matches(&instance, "fabulous fabric"));
        assert!(instance_matches(&instance, "favourites 1.21"));
        assert!(!instance_matches(&instance, "forge"));
        assert!(instance_matches(&instance, "   "));
    }

    #[test]
    fn interactive_filter_tokens_keep_versions() {
        let tokens = filter_tokens("Sodium 1.21 fabric");
        assert!(tokens.contains(&"sodium".to_owned()));
        // One-character fragments are noise; the version still narrows through
        // its `21` token, which matches 1.21 and 1.21.1.
        assert!(!tokens.contains(&"1".to_owned()));
        assert!(tokens.contains(&"21".to_owned()));
        assert!(filter_tokens(" ").is_empty());
    }

    #[test]
    fn install_state_keys_are_instance_scoped() {
        assert_ne!(
            install_state_key("a", "Modrinth-1"),
            install_state_key("b", "Modrinth-1")
        );
        assert_eq!(
            install_state_key("a", "Modrinth-1"),
            install_state_key("a", "Modrinth-1")
        );
    }

    #[test]
    fn jvm_args_ignore_blank_lines() {
        assert_eq!(
            parse_jvm_args(" -Xmx4G \n\n  -Dfoo=1\n"),
            vec!["-Xmx4G".to_owned(), "-Dfoo=1".to_owned()]
        );
    }

    #[test]
    fn instance_version_names_stay_path_safe() {
        for version in ["1.21.1", "1.7.10", "24w14a", "1.20-pre1"] {
            assert!(
                valid_instance_version(version),
                "unexpected rejection: {version}"
            );
        }
        for version in ["", "../escape", "1.21/evil", "1.21\\evil", "1.21 mod"] {
            assert!(
                !valid_instance_version(version),
                "unexpected acceptance: {version}"
            );
        }
        assert!(!valid_instance_version(&"1".repeat(65)));
    }

    #[test]
    fn health_flags_missing_files_and_system_java() {
        let instance = Instance::new(
            "missing",
            "Missing",
            "1.21.1",
            "vanilla",
            PathBuf::from("kaelis-health-test-does-not-exist"),
        );
        let health = InstanceHealth::check(&instance);
        assert_eq!(health.metadata, CheckState::Missing);
        assert_eq!(health.client, CheckState::Missing);
        assert_eq!(health.java, CheckState::Warn);
        assert_eq!(health.ram, CheckState::Ok);
    }

    #[test]
    fn health_rejects_unusable_version_names() {
        let mut instance = Instance::new(
            "bad-version",
            "Bad",
            "1.21.1",
            "vanilla",
            PathBuf::from("kaelis-health-test-does-not-exist"),
        );
        instance.mc_version = "../escape".to_owned();
        let health = InstanceHealth::check(&instance);
        assert_eq!(health.metadata, CheckState::Missing);
        assert_eq!(health.client, CheckState::Missing);
    }

    #[test]
    fn automation_defaults_on_for_a_fresh_profile() {
        // A missing settings file must not silently disable dependency repair.
        assert!(LauncherSettings::default().auto_repair);
    }

    #[test]
    fn status_kinds_have_distinct_colors() {
        let colors: std::collections::HashSet<[u8; 4]> = [
            StatusKind::Info,
            StatusKind::Busy,
            StatusKind::Success,
            StatusKind::Warning,
            StatusKind::Error,
        ]
        .into_iter()
        .map(|kind| kind.color().to_array())
        .collect();
        assert_eq!(colors.len(), 5);
    }

    #[test]
    fn progress_fraction_is_bounded() {
        assert_eq!(KaelisApp::fraction(Some((5, 10))), Some(0.5));
        assert_eq!(KaelisApp::fraction(Some((20, 10))), Some(1.0));
        assert_eq!(KaelisApp::fraction(Some((0, 0))), None);
        assert_eq!(KaelisApp::fraction(None), None);
    }

    #[test]
    fn paints_the_decoded_texture_in_the_mod_card() {
        let context = egui::Context::default();
        let texture = context.load_texture(
            "mod-icon-test",
            egui::ColorImage::new([4, 4], egui::Color32::RED),
            egui::TextureOptions::LINEAR,
        );
        let output = context.run(Default::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                draw_mod_icon(ui, Some(&texture), &item(ModProvider::Modrinth, "x"));
            });
        });
        // A single textured quad is painted either as a textured rect or as a
        // mesh depending on the representation the painter chooses.
        let painted = output.shapes.iter().any(|clipped| match &clipped.shape {
            egui::Shape::Mesh(mesh) => mesh.texture_id == texture.id(),
            egui::Shape::Rect(rect) => rect.fill_texture_id == texture.id(),
            _ => false,
        });
        assert!(painted, "the mod icon texture was not painted");
    }

    #[test]
    fn instance_artwork_is_centre_cropped_to_a_square() {
        let mut pixels = Vec::new();
        for row in 0..2_u8 {
            for column in 0..4_u8 {
                pixels.push(egui::Color32::from_rgb(column * 10, row * 10, 0));
            }
        }
        let cropped = center_square(egui::ColorImage {
            size: [4, 2],
            pixels,
        });
        assert_eq!(cropped.size, [2, 2]);
        assert_eq!(
            cropped.pixels,
            vec![
                egui::Color32::from_rgb(10, 0, 0),
                egui::Color32::from_rgb(20, 0, 0),
                egui::Color32::from_rgb(10, 10, 0),
                egui::Color32::from_rgb(20, 10, 0),
            ]
        );
        let source = image::RgbaImage::from_pixel(128, 64, image::Rgba([12, 34, 56, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        source.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let icon = decode_instance_icon(png.get_ref()).unwrap();
        assert_eq!(icon.size[0], icon.size[1]);
    }

    #[test]
    fn paints_the_instance_artwork_texture() {
        let context = egui::Context::default();
        let texture = context.load_texture(
            "instance-artwork-test",
            egui::ColorImage::new([4, 4], egui::Color32::from_rgb(255, 0, 255)),
            egui::TextureOptions::LINEAR,
        );
        let instance = Instance::new(
            "art-test",
            "Art Test",
            "1.21",
            "vanilla",
            PathBuf::from("kaelis-artwork-test"),
        );
        let output = context.run(Default::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                draw_instance_avatar(ui, Some(&texture), &instance);
            });
        });
        let painted = output.shapes.iter().any(|clipped| match &clipped.shape {
            egui::Shape::Mesh(mesh) => mesh.texture_id == texture.id(),
            egui::Shape::Rect(rect) => rect.fill_texture_id == texture.id(),
            _ => false,
        });
        assert!(painted, "the instance artwork texture was not painted");
    }

    #[test]
    fn rejects_icons_larger_than_the_decode_limit() {
        let source = image::RgbaImage::from_pixel(1100, 1100, image::Rgba([12, 34, 56, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        source.write_to(&mut png, image::ImageFormat::Png).unwrap();
        assert!(decode_mod_icon(png.get_ref()).is_err());
        assert!(decode_mod_icon(b"not an image").is_err());
    }

    #[test]
    fn icon_urls_require_https() {
        assert_eq!(
            https_icon_url("https://cdn.example.net/icon.png").as_deref(),
            Some("https://cdn.example.net/icon.png")
        );
        assert!(https_icon_url("http://cdn.example.net/icon.png").is_none());
        assert!(https_icon_url("file:///C:/icon.png").is_none());
        assert!(https_icon_url("not a url").is_none());
    }

    #[test]
    fn mod_icon_keys_are_provider_scoped() {
        let modrinth = item(ModProvider::Modrinth, "abc");
        let curseforge = item(ModProvider::CurseForge, "abc");
        assert_ne!(mod_item_key(&modrinth), mod_item_key(&curseforge));
    }

    #[test]
    fn unrelated_arguments_do_not_trigger_a_launch() {
        assert_eq!(
            parse_handoff(&["--offline".to_owned(), "Alex".to_owned()]),
            HandoffRequest::Unsupported
        );
    }

    fn named(name: &str) -> Instance {
        Instance::new(
            &slugify(name),
            name,
            "1.21.1",
            "vanilla",
            PathBuf::from("instance"),
        )
    }

    #[test]
    fn plain_names_keep_a_readable_folder_id() {
        assert_eq!(slugify("Fabulously Optimized"), "fabulously-optimized");
        assert_eq!(slugify("  Vanilla_1.21  "), "vanilla_1-21");
    }

    #[test]
    fn names_that_lose_characters_still_get_a_unique_safe_id() {
        let cjk = slugify("日本語");
        let accented = slugify("Café");
        assert!(cjk.starts_with("instance-"), "{cjk}");
        assert_ne!(accented, slugify("Cafe"));
        for id in [&cjk, &accented] {
            assert!(id.len() <= 64);
            assert!(
                id.bytes().all(|byte| byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || byte == b'-'
                    || byte == b'_'),
                "{id}"
            );
        }
    }

    #[test]
    fn a_name_only_conflicts_with_an_instance_that_actually_owns_it() {
        let instances = vec![named("Vanilla 1.21")];
        assert!(instance_name_conflict(&instances, "Vanilla 1.21").is_some());
        assert!(instance_name_conflict(&instances, "vanilla 1.21").is_some());
        assert!(instance_name_conflict(&instances, "Vanilla 1.22").is_none());
        assert!(instance_name_conflict(&instances, "日本語").is_none());
        assert!(instance_name_conflict(&instances, "   ").is_none());
    }

    fn scratch_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kaelis-drop-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn touch(path: &PathBuf) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(path, b"x").expect("file");
    }

    #[test]
    fn a_dropped_folder_contributes_the_files_inside_it() {
        let root = scratch_dir("folder");
        touch(&root.join("alpha.jar"));
        touch(&root.join("nested/beta.zip"));
        touch(&root.join("deeper/one/two/gamma.jar"));
        touch(&root.join("notes.txt"));
        let files = expand_dropped(vec![root.clone()]);
        let names = files
            .iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["alpha.jar", "nested/beta.zip"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_dropped_folder_never_walks_disposable_or_hidden_directories() {
        let root = scratch_dir("junk");
        touch(&root.join("mods/keep.jar"));
        touch(&root.join("logs/drop.jar"));
        touch(&root.join(".cache/drop.jar"));
        touch(&root.join("crash-reports/drop.jar"));
        touch(&root.join(".git/drop.jar"));
        let files = expand_dropped(vec![root.clone()]);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("keep.jar"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn launch_commands_keep_the_instance_hook_ahead_of_user_rules() {
        assert_eq!(
            merge_launch_command(Some("echo one".to_owned()), Some("echo two".to_owned())),
            Some("echo one && echo two".to_owned())
        );
        assert_eq!(
            merge_launch_command(Some("  ".to_owned()), Some("echo two".to_owned())),
            Some("echo two".to_owned())
        );
        assert_eq!(
            merge_launch_command(Some("echo one".to_owned()), None),
            Some("echo one".to_owned())
        );
        assert_eq!(merge_launch_command(None, None), None);
        assert_eq!(
            merge_launch_command(None, Some("echo two".to_owned())),
            Some("echo two".to_owned())
        );
    }

    #[test]
    fn artwork_payloads_accept_data_urls_and_bare_base64() {
        use base64::Engine;
        // A real payload is long; the 64-character floor keeps short words
        // that happen to be valid base64 from being decoded as pictures.
        let bytes = vec![0x89_u8; 120];
        let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
        assert_eq!(decode_image_payload(&encoded).as_deref(), Some(&bytes[..]));
        let data_url = format!("data:image/png;base64,{encoded}");
        assert_eq!(decode_image_payload(&data_url).as_deref(), Some(&bytes[..]));
        // Paths, plain words, and text URLs are never decoded.
        assert!(decode_image_payload("C:/art/icon.png").is_none());
        assert!(decode_image_payload("https://example.com/a.png").is_none());
        assert!(decode_image_payload("data:image/png,not-base64").is_none());
    }

    #[test]
    fn a_modpack_icon_becomes_the_instance_artwork() {
        let root = scratch_dir("pack-artwork");
        let mut instance = Instance::new("pack", "Pack", "1.21.1", "fabric", root.clone());
        let mut icon = image::RgbaImage::new(12, 8);
        for (x, _, pixel) in icon.enumerate_pixels_mut() {
            *pixel = image::Rgba([x as u8 * 20, 90, 200, 255]);
        }
        let mut encoded = Vec::new();
        icon.write_to(
            &mut std::io::Cursor::new(&mut encoded),
            image::ImageFormat::Png,
        )
        .expect("encode");
        assert!(apply_pack_artwork(&mut instance, &encoded));
        let stored = instance.icon.clone().expect("icon path");
        assert!(stored.ends_with(".kaelis/artwork.png"));
        assert_eq!(
            image::open(&stored).expect("open").to_rgba8().dimensions(),
            (ARTWORK_EDGE, ARTWORK_EDGE)
        );
        // A bad picture leaves the instance untouched rather than failing an
        // otherwise complete import.
        assert!(!apply_pack_artwork(&mut instance, b"not an image"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn artwork_is_cropped_to_one_square_png() {
        let root = scratch_dir("artwork");
        let mut wide = image::RgbaImage::new(80, 40);
        for (x, _, pixel) in wide.enumerate_pixels_mut() {
            *pixel = image::Rgba([x as u8, 40, 90, 255]);
        }
        let mut encoded = Vec::new();
        wide.write_to(
            &mut std::io::Cursor::new(&mut encoded),
            image::ImageFormat::Png,
        )
        .expect("encode");
        let stored = store_instance_artwork(&root, &encoded).expect("store");
        assert!(stored.ends_with(".kaelis/artwork.png"));
        let decoded = image::open(&stored).expect("reopen").to_rgba8();
        assert_eq!(decoded.dimensions(), (ARTWORK_EDGE, ARTWORK_EDGE));
        assert!(store_instance_artwork(&root, b"not an image").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn svg_artwork_is_rasterised_instead_of_rejected() {
        let root = scratch_dir("artwork-svg");
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 8 4"><rect width="8" height="4" fill="#3d82f6"/></svg>"##;
        assert!(looks_like_svg(svg));
        let stored = store_instance_artwork(&root, svg).expect("store svg");
        let decoded = image::open(&stored).expect("reopen").to_rgba8();
        assert_eq!(decoded.dimensions(), (ARTWORK_EDGE, ARTWORK_EDGE));
        // A centre pixel proves the rectangle rendered, not a blank canvas.
        let pixel = decoded.get_pixel(ARTWORK_EDGE / 2, ARTWORK_EDGE / 2);
        assert!(pixel[2] > 200 && pixel[0] < 120, "pixel: {pixel:?}");
        // Markup pasted as text is used directly.
        assert!(decode_image_payload(std::str::from_utf8(svg).unwrap()).is_some());
        // An unencoded data URL decodes its escapes.
        let data_url = "data:image/svg+xml,%3Csvg%20xmlns%3D%22http%3A%2F%2Fwww.w3.org%2F2000%2Fsvg%22%3E%3C%2Fsvg%3E";
        assert!(decode_image_payload(data_url).is_some());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[ignore = "needs a desktop clipboard; run locally with --ignored"]
    fn clipboard_picture_round_trips_as_png() {
        let source = image::RgbaImage::from_pixel(4, 3, image::Rgba([10, 120, 240, 255]));
        let mut clipboard = arboard::Clipboard::new().expect("clipboard");
        clipboard
            .set_image(arboard::ImageData {
                width: 4,
                height: 3,
                bytes: source.as_raw().clone().into(),
            })
            .expect("set image");
        let png = clipboard_picture().expect("read clipboard picture");
        let decoded = image::load_from_memory(&png).expect("png").to_rgba8();
        assert_eq!(decoded.dimensions(), (4, 3));
        assert_eq!(decoded.get_pixel(1, 1).0, [10, 120, 240, 255]);
    }

    #[test]
    fn pasted_instance_json_accepts_fences_and_rejects_other_text() {
        let instance = Instance::new("demo", "Demo", "1.21.1", "fabric", PathBuf::from("C:/x"));
        let json = serde_json::to_string_pretty(&instance).expect("encode");
        assert_eq!(
            parse_instance_json(&json).map(|instance| instance.name),
            Some("Demo".to_owned())
        );
        let fenced = format!("```json\n{json}\n```");
        assert_eq!(
            parse_instance_json(&fenced).map(|instance| instance.id),
            Some("demo".to_owned())
        );
        assert!(parse_instance_json("hello").is_none());
        // A record missing required fields is not an instance.
        assert!(parse_instance_json("{\"id\":\"x\"}").is_none());
    }

    #[test]
    fn a_drop_keeps_supported_files_and_ignores_the_rest() {
        let root = scratch_dir("files");
        let jar = root.join("one.jar");
        let pack = root.join("two.MRPACK");
        let readme = root.join("readme.md");
        for path in [&jar, &pack, &readme] {
            touch(path);
        }
        // The same path twice is one install, not two.
        let files = expand_dropped(vec![jar.clone(), readme.clone(), jar.clone(), pack.clone()]);
        assert_eq!(files, vec![jar, pack]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
