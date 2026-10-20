//! Kaelis desktop interface: translucent glass panels, async mod discovery,
//! Microsoft/offline accounts, profile-head textures, and idle frame control.

mod tray;

use eframe::egui;
use kaelis_api::auth::{
    AuthSession, DeviceCodeResponse, MicrosoftAuth, OfflineProfile, fetch_crafatar_head,
    offline_profile,
};
use kaelis_api::launch::{LaunchIdentity, run_instance};
use kaelis_api::mods::{
    ContentKind, InstallEvent, InstallReport, ModPlatformClient, ModProvider, ModSearchItem,
    ModSearchResults,
};
use kaelis_api::repair::{
    DuplicateRepair, RepairReport, disable_duplicate_mods, repair_environment,
};
use kaelis_api::runtime::RuntimeInstallProgress;
use kaelis_api::vanilla::InstallProgress;
use kaelis_api::versions::{VersionEntry, VersionManifest, fetch_manifest};
use kaelis_core::ephemeral_log::EphemeralLog;
use kaelis_core::handoff::HandoffServer;
use kaelis_core::instance::Instance;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::runtime::Handle;
use tokio::sync::{Semaphore, mpsc, watch};

const ACCENT: egui::Color32 = egui::Color32::from_rgb(96, 196, 255);
const ACCENT_SOFT: egui::Color32 = egui::Color32::from_rgb(160, 220, 255);
const VIOLET: egui::Color32 = egui::Color32::from_rgb(158, 145, 255);
const AZURE_DEVICE_CODE_DOC: &str =
    "https://learn.microsoft.com/entra/identity-platform/v2-oauth2-device-code";
const AZURE_APP_REGISTRATION: &str =
    "https://portal.azure.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade";
const CURSEFORGE_CONSOLE: &str = "https://console.curseforge.com/";
const ACCENT_DARK: egui::Color32 = egui::Color32::from_rgb(8, 15, 26);
const GLASS: egui::Color32 = egui::Color32::from_rgba_premultiplied(10, 16, 27, 232);
const GLASS_LIGHT: egui::Color32 = egui::Color32::from_rgba_premultiplied(21, 31, 47, 238);
const BORDER: egui::Color32 = egui::Color32::from_rgb(39, 53, 72);
const BORDER_BRIGHT: egui::Color32 = egui::Color32::from_rgb(55, 77, 103);
const MUTED: egui::Color32 = egui::Color32::from_rgb(139, 157, 179);
const TEXT: egui::Color32 = egui::Color32::from_rgb(232, 240, 250);
const SURFACE: egui::Color32 = egui::Color32::from_rgba_premultiplied(12, 19, 31, 247);
const SURFACE_ALT: egui::Color32 = egui::Color32::from_rgba_premultiplied(18, 27, 42, 247);
const SURFACE_HOVER: egui::Color32 = egui::Color32::from_rgba_premultiplied(25, 38, 57, 250);
/// Destructive actions (erase, remove) use this pair so danger reads the same
/// in every surface: outlined when idle, filled when hovered.
const DANGER: egui::Color32 = egui::Color32::from_rgb(255, 126, 132);
const DANGER_TINT: (u8, u8, u8) = (255, 108, 118);
const CORNER_SM: f32 = 8.0;
const CORNER_MD: f32 = 11.0;
const CORNER_LG: f32 = 14.0;
const CORNER_XL: f32 = 18.0;
const MOD_ICON_BYTES: usize = 1024 * 1024;
const MOD_ICON_SIZE: f32 = 44.0;
const MOD_ICON_CONCURRENCY: usize = 6;
/// Provider thumbnails are at most 512 px; refuse larger decodes outright so
/// a hostile or malformed image cannot stall a decoder thread or spike RAM.
const MOD_ICON_MAX_EDGE: u32 = 1024;
const MOD_ICON_MAX_ALLOC: u64 = 32 * 1024 * 1024;
/// Instance artwork is local, so it may be a little larger than a provider
/// thumbnail, but decoding stays bounded to a single 8 MiB read.
const INSTANCE_ICON_BYTES: u64 = 8 * 1024 * 1024;
const INSTANCE_VARIANTS: u8 = 6;
/// Typing pauses this long before the mod browser queries both platforms.
const MOD_SEARCH_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(450);
/// Results requested per provider per page. Paging, not truncation, is what
/// keeps the catalogue browsable.
const MOD_SEARCH_LIMIT: u32 = 30;
const MOD_SEARCH_ID: &str = "kaelis-mod-search";
const MOD_REFINE_ID: &str = "kaelis-mod-refine";
const INSTANCE_FILTER_ID: &str = "kaelis-instance-filter";
const INSTALLED_FILTER_ID: &str = "kaelis-installed-filter";

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
            Self::Busy => ACCENT,
            Self::Success => egui::Color32::from_rgb(126, 231, 165),
            Self::Warning => egui::Color32::from_rgb(255, 195, 120),
            Self::Error => egui::Color32::from_rgb(255, 133, 133),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum InstanceAction {
    Play,
    Select,
    Edit,
    InstallVanilla,
    InstallJava,
    Settings,
    OpenFolder,
    OpenMods,
    ShowJson,
    ManageContent(ContentKind),
    /// Delete the instance's whole tree and record in one step.
    Erase,
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
enum ModFilter {
    All,
    Installed,
    NotInstalled,
}

impl ModFilter {
    const ALL: [Self; 3] = [Self::All, Self::Installed, Self::NotInstalled];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Installed => "Installed",
            Self::NotInstalled => "Not installed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModSort {
    Relevance,
    Downloads,
    Name,
}

#[derive(Debug, Clone, Copy)]
struct ModView {
    filter: ModFilter,
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
    rescan: bool,
    open_folder: bool,
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
struct InstalledScan {
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
struct ContentLibrary {
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

pub(crate) enum GuiEvent {
    ModpackInstalled(Box<Result<ModpackOutcome, String>>),
    DeviceCode(Result<DeviceCodeResponse, String>),
    SignedIn(Result<AuthSession, String>),
    Restored(Result<AuthSession, String>),
    Avatar(Result<egui::ColorImage, String>),
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
    Notice(String),
    GameStarted,
    GameExited(String),
    GameFailed(String),
    Handoff(Vec<String>),
    Tray(tray::TrayAction),
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
    /// Interface zoom applied through egui, `1.0` at 100 %.
    ui_zoom: f32,
    /// True when Play provisioned Java first and must continue into the game.
    launch_after_java: bool,
    current_tab: Tab,
    status: String,
    status_kind: StatusKind,
    instances: Vec<Instance>,
    instance_cards: HashMap<String, InstanceCard>,
    instance_icons: HashMap<String, ModIconState>,
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
    mod_filter: ModFilter,
    mod_sort: ModSort,
    mod_show_modrinth: bool,
    mod_show_curseforge: bool,
    installed: Option<InstalledScan>,
    installed_filter: String,
    installed_missing_only: bool,
    mod_icons: HashMap<String, ModIconState>,
    mod_icon_slots: Arc<Semaphore>,
    mod_icon_generation: u64,
    mod_icon_epoch: Arc<AtomicU64>,
    installing_job: Option<InstallJob>,
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
    auth_cancel: Option<watch::Sender<bool>>,
    microsoft_session: Option<AuthSession>,
    last_account: String,
    offline_account: Option<OfflineProfile>,
    offline_name: String,
    avatar: Option<egui::TextureHandle>,
    game_running: bool,
    settings_ram_mb: u32,
    settings_jvm_args: String,
    ms_client_id: String,
    cf_api_key: String,
    integrations_status: String,
    screenshot_path: Option<PathBuf>,
    screenshot_frame: u32,
    screenshot_at: u32,
    /// Title of the modpack being downloaded and imported, if any.
    modpack_installing: Option<String>,
    modpack_detail: String,
    /// Remove logs, crash reports, and staging files at every launch.
    auto_clean_junk: bool,
}

impl KaelisApp {
    pub fn new(
        creation: &eframe::CreationContext<'_>,
        http: reqwest::Client,
        runtime: Handle,
        sweeper: tokio::task::JoinHandle<()>,
        handoff: Option<HandoffServer>,
    ) -> Self {
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
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::TRANSPARENT;
        visuals.window_fill = GLASS;
        visuals.window_rounding = egui::Rounding::same(CORNER_LG);
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
        visuals.widgets.noninteractive.bg_fill = egui::Color32::TRANSPARENT;
        visuals.widgets.inactive.weak_bg_fill = egui::Color32::from_rgb(24, 35, 50);
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(24, 35, 50);
        visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(34, 48, 67);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(34, 48, 67);
        visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgb(44, 61, 84);
        visuals.widgets.active.bg_fill = egui::Color32::from_rgb(44, 61, 84);
        visuals.extreme_bg_color = egui::Color32::from_rgb(8, 14, 23);
        visuals.faint_bg_color = egui::Color32::from_rgb(18, 27, 41);
        visuals.widgets.inactive.rounding = egui::Rounding::same(CORNER_SM);
        visuals.widgets.hovered.rounding = egui::Rounding::same(CORNER_SM);
        visuals.widgets.active.rounding = egui::Rounding::same(CORNER_SM);
        visuals.selection.bg_fill = egui::Color32::from_rgba_unmultiplied(96, 196, 255, 58);
        visuals.selection.stroke = egui::Stroke::new(1.0_f32, ACCENT_SOFT);
        visuals.hyperlink_color = ACCENT;
        creation.egui_ctx.set_visuals(visuals.clone());
        let mut style = (*creation.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 26.0;
        style.spacing.scroll.bar_width = 6.0;
        style.spacing.scroll.floating = true;
        for widget in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
        ] {
            widget.expansion = 0.0;
        }
        style.visuals.window_stroke = egui::Stroke::new(1.0_f32, BORDER);
        style.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, BORDER_BRIGHT);
        style
            .text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(22.0));
        style
            .text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(12.5));
        style
            .text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(11.5));
        style.visuals = visuals;
        creation.egui_ctx.set_style(style);

        let instances = Instance::load_all().unwrap_or_default();
        let selected_instance = instances.first().map(|instance| instance.id.clone());
        let installed = instances.first().map(scan_installed);
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
        let launcher_settings = load_launcher_settings();
        // Restore the local profile too: an offline account has no token to
        // refresh, but its name and Steve skin should still be there.
        let restored_offline = {
            let name = launcher_settings.offline_account.trim();
            let valid = (3..=16).contains(&name.len())
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
            valid.then(|| offline_profile(name))
        };
        let ui_zoom = if launcher_settings.ui_zoom.is_finite() {
            launcher_settings.ui_zoom.clamp(0.8, 2.0)
        } else {
            1.0
        };
        creation.egui_ctx.set_zoom_factor(ui_zoom);
        let saved_cf_key =
            kaelis_api::secrets::load_secret(kaelis_api::secrets::CURSEFORGE_KEY_ACCOUNT)
                .ok()
                .flatten()
                .unwrap_or_default();
        let integrations_status = if saved_cf_key.is_empty() {
            "CurseForge needs an API key; Microsoft sign-in needs an Azure public client ID."
                .to_owned()
        } else {
            "CurseForge key loaded from the OS credential store.".to_owned()
        };
        let status = if tray.is_some() {
            "Ready".to_owned()
        } else {
            "Ready · system tray unavailable".to_owned()
        };
        // Restore the last Microsoft account from the OS keychain without a
        // device-code round trip. A stale or missing token simply leaves the
        // account signed out; the Account tab still offers a fresh sign-in.
        if !launcher_settings.last_account.is_empty()
            && !launcher_settings.ms_client_id.trim().is_empty()
        {
            let restore_name = launcher_settings.last_account.clone();
            let restore_client_id = launcher_settings.ms_client_id.clone();
            let sender = sender.clone();
            let repaint = creation.egui_ctx.clone();
            runtime.spawn(async move {
                let result = match MicrosoftAuth::with_client_id(restore_client_id) {
                    Ok(auth) => auth
                        .login_with_refresh_token(&restore_name)
                        .await
                        .map_err(|error| error.to_string()),
                    Err(error) => Err(error.to_string()),
                };
                send_event(&sender, &repaint, GuiEvent::Restored(result));
            });
        }
        let mut app = Self {
            platforms: ModPlatformClient::with_curseforge_key(
                http.clone(),
                (!saved_cf_key.is_empty()).then(|| saved_cf_key.clone()),
            ),
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
            ui_zoom,
            launch_after_java: false,
            current_tab,
            status,
            status_kind: StatusKind::Info,
            instances,
            instance_cards: HashMap::new(),
            instance_icons: HashMap::new(),
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
            show_alpha: false,
            show_beta: false,
            show_snapshots: true,
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
            mod_auto_search: true,
            mod_refine: String::new(),
            mod_filter: ModFilter::All,
            mod_sort: ModSort::Relevance,
            mod_show_modrinth: true,
            mod_show_curseforge: true,
            installed,
            installed_filter: String::new(),
            installed_missing_only: false,
            mod_icons: HashMap::new(),
            mod_icon_slots: Arc::new(Semaphore::new(MOD_ICON_CONCURRENCY)),
            mod_icon_generation: 0,
            mod_icon_epoch: Arc::new(AtomicU64::new(0)),
            installing_job: None,
            install_queue: VecDeque::new(),
            install_states: HashMap::new(),
            install_detail: None,
            install_started_at: None,
            install_dependencies: true,
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
            auth_status: "Choose Microsoft sign-in or an offline profile.".to_owned(),
            auth_device: None,
            auth_cancel: None,
            microsoft_session: None,
            last_account: launcher_settings.last_account,
            offline_account: restored_offline.clone(),
            offline_name: restored_offline
                .as_ref()
                .map(|profile| profile.name.clone())
                .unwrap_or_else(|| "Player".to_owned()),
            avatar: restored_offline.as_ref().map(|_| {
                creation.egui_ctx.load_texture(
                    "kaelis-offline-profile-head",
                    steve_head(),
                    egui::TextureOptions::NEAREST,
                )
            }),
            game_running: false,
            settings_ram_mb: 4096,
            settings_jvm_args: String::new(),
            ms_client_id: launcher_settings.ms_client_id,
            cf_api_key: saved_cf_key,
            integrations_status,
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
            auto_clean_junk: launcher_settings.auto_clean,
        };
        if app.auto_clean_junk {
            app.clean_junk_now(creation.egui_ctx.clone());
        }
        app.refresh_instance_cards();
        if let Some(id) = app.selected_instance.clone() {
            app.select_instance(&id);
        }
        app.reload_instance_icons(&creation.egui_ctx);
        app.request_versions(creation.egui_ctx.clone());
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
        let configured_client_id = self.ms_client_id.trim().to_owned();
        if !configured_client_id.is_empty() {
            let settings = self.settings_snapshot();
            if let Err(error) = save_launcher_settings(&settings) {
                self.auth_status = format!("Could not save the Microsoft client ID: {error}");
                self.auth_busy = false;
                self.auth_cancel = None;
                return;
            }
        }
        self.runtime.spawn(async move {
            let auth = match if configured_client_id.is_empty() {
                MicrosoftAuth::new()
            } else {
                MicrosoftAuth::with_client_id(configured_client_id)
            } {
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

    /// Authorization-code + PKCE is the preferred path: it uses a loopback
    /// redirect, works when a tenant blocks device-code authorization, and
    /// never asks the player to copy a code. Device code remains the fallback.
    fn start_browser_sign_in(&mut self, context: egui::Context) {
        if self.auth_busy {
            return;
        }
        let configured_client_id = self.ms_client_id.trim().to_owned();
        if !configured_client_id.is_empty() {
            let settings = self.settings_snapshot();
            if let Err(error) = save_launcher_settings(&settings) {
                self.auth_status = format!("Could not save the Microsoft client ID: {error}");
                return;
            }
        }
        let (cancel, cancel_receiver) = watch::channel(false);
        self.auth_cancel = Some(cancel);
        self.auth_busy = true;
        self.auth_status =
            "Opening Microsoft sign-in. Finish in the browser; this window updates automatically."
                .to_owned();
        let sender = self.sender.clone();
        let repaint = context;
        self.runtime.spawn(async move {
            let auth = match if configured_client_id.is_empty() {
                MicrosoftAuth::new()
            } else {
                MicrosoftAuth::with_client_id(configured_client_id)
            } {
                Ok(auth) => auth,
                Err(error) => {
                    send_event(
                        &sender,
                        &repaint,
                        GuiEvent::SignedIn(Err(error.to_string())),
                    );
                    return;
                }
            };
            let flow = match auth.begin_browser_login().await {
                Ok(flow) => flow,
                Err(error) => {
                    send_event(
                        &sender,
                        &repaint,
                        GuiEvent::SignedIn(Err(error.to_string())),
                    );
                    return;
                }
            };
            repaint.open_url(egui::OpenUrl::new_tab(flow.authorize_url().to_owned()));
            let result = flow
                .finish(&auth, cancel_receiver)
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
        self.modpack_installing = Some(title.clone());
        self.modpack_detail = "Resolving the pack archive…".to_owned();
        self.set_status(StatusKind::Busy, format!("Installing modpack {title}…"));
        let platforms = self.platforms.clone();
        let http = self.http.clone();
        let sender = self.sender.clone();
        let repaint = context;
        let staging = kaelis_core::sweeper::temp_root().join("modpacks");
        self.runtime.spawn(async move {
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
                let pack = kaelis_api::mrpack::read_pack_summary(&pack_path)
                    .map_err(|error| error.to_string())?;
                let root = Instance::instances_dir().join(&id);
                let instance = Instance::new(
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
                instance.save().map_err(|error| error.to_string())?;
                let imported =
                    match kaelis_api::mrpack::install_mrpack(&http, &pack_path, &instance.root)
                        .await
                    {
                        Ok(imported) => imported,
                        Err(error) => {
                            // Never leave a half-imported instance behind: the
                            // pack install is transactional, the instance is not.
                            let _ = instance.erase();
                            return Err(error.to_string());
                        }
                    };
                if downloaded {
                    // Staged downloads are ours to remove; a local .mrpack the
                    // user dropped belongs to them.
                    let _ = std::fs::remove_file(&pack_path);
                }
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
                GuiEvent::ModpackInstalled(Box::new(result)),
            );
        });
    }

    /// Delete logs, crash reports, and staging leftovers from every instance.
    /// Saves, mods, configs, and options are outside the cleaner by design.
    fn clean_junk_now(&mut self, context: egui::Context) {
        let sender = self.sender.clone();
        let repaint = context;
        let instances = Instance::instances_dir();
        self.runtime.spawn_blocking(move || {
            let message = match kaelis_core::junk::clean_instances(&instances) {
                Ok(report) => report.summary(),
                Err(error) => format!("Cleanup skipped: {error}"),
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
            self.runtime.spawn(async move {
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
            return;
        }
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
            self.instance_icons.insert(id, ModIconState::Failed);
            return;
        }
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
        self.instance_icons.clear();
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
                        "Icon image not found. Choose a PNG, JPEG, WebP, GIF or BMP file."
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
                    edit.icon_variant = edit
                        .icon_variant
                        .map(|variant| variant.min(INSTANCE_VARIANTS - 1));
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
        self.editing = Some(saved);
        self.set_status(StatusKind::Success, "Instance properties saved.");
    }

    /// Everything persisted outside instance JSON. One construction point so a
    /// new field can never be dropped by one of the six save paths.
    fn settings_snapshot(&self) -> LauncherSettings {
        LauncherSettings {
            ms_client_id: self.ms_client_id.trim().to_owned(),
            last_account: self.last_account.clone(),
            auto_repair: self.auto_repair_before_launch,
            auto_resolve_duplicates: self.auto_resolve_duplicates,
            auto_clean: self.auto_clean_junk,
            content_kind: self.content_kind,
            ui_zoom: self.ui_zoom,
            offline_account: self
                .offline_account
                .as_ref()
                .map(|profile| profile.name.clone())
                .unwrap_or_default(),
        }
    }

    fn persist_settings(&self) {
        let _ = save_launcher_settings(&self.settings_snapshot());
    }

    /// Apply and persist the interface zoom, snapped to 10 % steps inside a
    /// range that stays readable on both small and large displays.
    fn set_ui_zoom(&mut self, zoom: f32, context: &egui::Context) {
        let zoom = ((zoom.clamp(0.8, 2.0)) * 10.0).round() / 10.0;
        if (zoom - self.ui_zoom).abs() < f32::EPSILON {
            return;
        }
        self.ui_zoom = zoom;
        context.set_zoom_factor(zoom);
        context.request_repaint();
        self.persist_settings();
        self.set_status(
            StatusKind::Info,
            format!("Interface scale {}%.", (zoom * 100.0).round() as u32),
        );
    }

    fn save_integrations(&mut self) {
        let ms_client_id = self.ms_client_id.trim().to_owned();
        let settings = self.settings_snapshot();
        if let Err(error) = save_launcher_settings(&settings) {
            self.set_status(
                StatusKind::Error,
                format!("Could not save launcher settings: {error}"),
            );
            return;
        }
        self.ms_client_id = ms_client_id;
        let key = self.cf_api_key.trim().to_owned();
        let key_result = if key.is_empty() {
            kaelis_api::secrets::delete_secret(kaelis_api::secrets::CURSEFORGE_KEY_ACCOUNT)
        } else {
            kaelis_api::secrets::store_secret(kaelis_api::secrets::CURSEFORGE_KEY_ACCOUNT, &key)
        };
        if let Err(error) = key_result {
            self.set_status(
                StatusKind::Error,
                format!("Could not save the CurseForge key: {error}"),
            );
            return;
        }
        self.cf_api_key = key.clone();
        self.platforms = ModPlatformClient::with_curseforge_key(
            self.http.clone(),
            (!key.is_empty()).then_some(key),
        );
        self.integrations_status = if self.ms_client_id.is_empty() {
            "Saved. Add a Microsoft client ID to enable sign-in.".to_owned()
        } else {
            "Integration settings saved.".to_owned()
        };
        self.set_status(StatusKind::Success, self.integrations_status.clone());
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

    /// Download Fabric's loader libraries for an instance that just finished
    /// its vanilla install, so its mods can actually run.
    fn provision_loader(&mut self, context: egui::Context, instance_id: String) {
        let Some(instance) = self
            .instances
            .iter()
            .find(|instance| instance.id == instance_id)
            .cloned()
        else {
            return;
        };
        if !instance.loader.eq_ignore_ascii_case("fabric")
            || kaelis_api::loader::load_installed(&instance.root, &instance.mc_version).is_some()
        {
            return;
        }
        let client = self.http.clone();
        let sender = self.sender.clone();
        let repaint = context;
        let game_version = instance.mc_version.clone();
        self.runtime.spawn(async move {
            let message =
                match kaelis_api::loader::ensure_installed(&client, &instance.root, &game_version)
                    .await
                {
                    Ok(install) => format!(
                        "Fabric loader {} installed for {} ({} libraries).",
                        install.loader_version, install.game_version, install.libraries
                    ),
                    Err(error) => format!("Fabric loader install failed: {error}"),
                };
            send_event(&sender, &repaint, GuiEvent::Notice(message));
        });
    }

    fn sign_in_offline(&mut self, context: egui::Context) {
        let name = self.offline_name.trim();
        let valid = (3..=16).contains(&name.len())
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        if !valid {
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
        self.avatar = Some(context.load_texture(
            "kaelis-offline-profile-head",
            steve_head(),
            egui::TextureOptions::NEAREST,
        ));
        self.persist_settings();
    }

    fn forget_account(&mut self, context: egui::Context) {
        let Some(session) = self.microsoft_session.take() else {
            self.offline_account = None;
            self.avatar = None;
            self.persist_settings();
            self.auth_status = "Signed out.".to_owned();
            return;
        };
        let name = session.profile.name.clone();
        self.offline_account = None;
        self.avatar = None;
        self.last_account.clear();
        let mut settings = self.settings_snapshot();
        settings.last_account.clear();
        let _ = save_launcher_settings(&settings);
        self.auth_status = "Removing the saved keychain token…".to_owned();
        drop(session);
        let sender = self.sender.clone();
        let repaint = context;
        self.runtime.spawn_blocking(move || {
            let message = match kaelis_api::auth::delete_refresh_token(&name) {
                Ok(()) => "Signed out and removed the saved keychain token.".to_owned(),
                Err(error) => format!("Could not remove the saved keychain token: {error}"),
            };
            send_event(&sender, &repaint, GuiEvent::Notice(message));
        });
    }

    fn launch_selected(&mut self, context: egui::Context) {
        if self.game_running {
            self.set_status(StatusKind::Warning, "Minecraft is already running.");
            return;
        }
        let Some(instance) = self.selected_instance().cloned() else {
            self.set_status(
                StatusKind::Warning,
                "Create or select an instance before launching.",
            );
            return;
        };
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

        // First play on a machine without a system JDK: fetch the
        // version-matched Mojang runtime now instead of failing later inside
        // `Command::spawn` with "program not found".
        if kaelis_api::runtime::resolve_java(&instance).is_none() {
            self.pending_launch = Some((instance.clone(), identity));
            self.launch_after_java = true;
            self.set_status(
                StatusKind::Busy,
                format!(
                    "No Java found — installing the runtime for {} first…",
                    instance.name
                ),
            );
            self.install_runtime_for_instance(context.clone(), instance);
            return;
        }

        // A launch is the moment missing dependencies actually matter, so
        // resolve them first instead of failing inside the game's log.
        let missing = self
            .installed
            .as_ref()
            .map(|scan| !scan.missing.is_empty())
            .unwrap_or(false);
        let loader_missing = instance.loader.eq_ignore_ascii_case("fabric")
            && kaelis_api::loader::load_installed(&instance.root, &instance.mc_version).is_none();
        if (missing || loader_missing) && self.auto_repair_before_launch {
            self.pending_launch = Some((instance, identity));
            self.repair_mod_environment(context);
            return;
        }
        self.start_launch(instance, identity, context);
    }

    fn start_launch(
        &mut self,
        instance: Instance,
        identity: LaunchIdentity,
        context: egui::Context,
    ) {
        self.set_status(StatusKind::Busy, format!("Starting {}…", instance.name));
        let sender = self.sender.clone();
        let repaint = context.clone();
        let started_sender = sender.clone();
        let started_context = repaint.clone();
        let log = EphemeralLog::new(20_000);
        self.runtime.spawn(async move {
            let result = run_instance(instance, identity, log.clone(), move || {
                send_event(&started_sender, &started_context, GuiEvent::GameStarted);
            })
            .await;
            log.clear();
            match result {
                Ok(status) => send_event(
                    &sender,
                    &repaint,
                    GuiEvent::GameExited(format!("Minecraft exited with {status}.")),
                ),
                Err(error) => send_event(
                    &sender,
                    &repaint,
                    GuiEvent::GameFailed(format!("Could not launch Minecraft: {error}")),
                ),
            }
        });
    }

    /// The avatar texture is released while the game runs (the window is
    /// hidden); rebuild it as soon as the launcher comes back.
    fn restore_avatar(&mut self, context: &egui::Context) {
        if self.offline_account.is_some() {
            self.avatar = Some(context.load_texture(
                "kaelis-offline-profile-head",
                steve_head(),
                egui::TextureOptions::NEAREST,
            ));
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
            GuiEvent::ModpackInstalled(result) => {
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
                        // Only Fabric and vanilla can be provisioned end to end
                        // today; anything else imports the files and says so.
                        let launchable = loader.eq_ignore_ascii_case("fabric")
                            || loader.eq_ignore_ascii_case("vanilla");
                        self.instances.insert(0, outcome.instance.clone());
                        self.refresh_instance_cards();
                        self.select_instance(&id);
                        self.refresh_installed();
                        let text = if launchable {
                            format!(
                                "{name} installed · {imported} files · {mods} mods · MC {game_version} · {loader}"
                            )
                        } else {
                            format!(
                                "{name} imported · {imported} files · MC {game_version} · {loader}. Kaelis provisions Fabric automatically; add this loader before playing."
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
                self.auth_status =
                    "Enter the code in your browser to finish signing in.".to_owned();
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
                self.auth_status = format!("Signed in as {}.", session.profile.name);
                let uuid = session.profile.id.clone();
                self.offline_account = None;
                self.avatar = None;
                self.last_account = session.profile.name.clone();
                let client_id = if self.ms_client_id.trim().is_empty() {
                    std::env::var("KAELIS_MS_CLIENT_ID").unwrap_or_default()
                } else {
                    self.ms_client_id.trim().to_owned()
                };
                let mut settings = self.settings_snapshot();
                settings.ms_client_id = client_id;
                let saved = save_launcher_settings(&settings);
                if let Err(error) = saved {
                    self.auth_status =
                        format!("Signed in, but the account preference was not saved: {error}");
                }
                self.microsoft_session = Some(session);
                self.request_avatar(context.clone(), uuid);
            }
            GuiEvent::Restored(Ok(session)) => {
                self.auth_status = format!("Welcome back, {}.", session.profile.name);
                let uuid = session.profile.id.clone();
                self.microsoft_session = Some(session);
                self.request_avatar(context.clone(), uuid);
            }
            GuiEvent::Restored(Err(error)) => {
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
                self.auth_status = error;
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
            GuiEvent::Versions(Ok(manifest)) => {
                if self.new_instance_version.is_empty() {
                    self.new_instance_version = manifest.latest.release.clone();
                }
                self.versions = manifest.versions;
                if self.installing_job.is_none() && !self.repairing_mods {
                    self.set_status(StatusKind::Success, "Minecraft versions refreshed.");
                }
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
            GuiEvent::ModInstallProgress(detail) => {
                self.install_detail = Some(detail.clone());
                if let Some(job) = &self.installing_job {
                    self.set_status(
                        StatusKind::Busy,
                        format!("Installing {} · {detail}", job.item.title),
                    );
                }
            }
            GuiEvent::ManualInstalled(result) => {
                self.manual_busy = false;
                match result {
                    Ok(report) => {
                        let summary = report.summary();
                        self.manual_status = summary.clone();
                        self.last_install = Some(report);
                        self.refresh_installed();
                        let kind = if self
                            .last_install
                            .as_ref()
                            .is_some_and(|report| report.warnings.is_empty())
                        {
                            StatusKind::Success
                        } else {
                            StatusKind::Warning
                        };
                        self.set_status(kind, summary);
                    }
                    Err(error) => {
                        self.manual_status = format!("Import failed: {error}");
                        self.set_status(StatusKind::Error, self.manual_status.clone());
                    }
                }
            }
            GuiEvent::ModInstalled(result) => {
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
                    let java_ready = result.is_ok();
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
                    if !java_ready && self.launch_after_java {
                        self.launch_after_java = false;
                        self.pending_launch = None;
                    }
                    // Play provisioned Java first; continue into the game now
                    // that the runtime is on disk.
                    if java_ready && self.launch_after_java {
                        self.launch_after_java = false;
                        if let Some((pending, identity)) = self
                            .pending_launch
                            .take_if(|(item, _)| item.id == instance_id)
                        {
                            self.start_launch(pending, identity, context.clone());
                        }
                    }
                }
            }
            GuiEvent::GameStarted => {
                self.game_running = true;
                self.set_status(
                    StatusKind::Success,
                    "Minecraft is running. Kaelis is sleeping in the tray.",
                );
                self.avatar = None;
                if let Some(sweeper) = self.sweeper.take() {
                    sweeper.abort();
                }
                if let Some(cancel) = self.auth_cancel.take() {
                    let _ = cancel.send(true);
                }
                context.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
            GuiEvent::GameExited(message) => {
                self.game_running = false;
                self.set_status(StatusKind::Info, message);
                self.restart_sweeper();
                self.restore_avatar(context);
                show_window(context);
            }
            GuiEvent::GameFailed(error) => {
                self.game_running = false;
                self.set_status(StatusKind::Error, error);
                self.restart_sweeper();
                self.restore_avatar(context);
                show_window(context);
            }
            GuiEvent::Notice(message) => self.set_status(StatusKind::Info, message),
            GuiEvent::Handoff(arguments) => self.apply_handoff(context, &arguments),
            GuiEvent::Tray(action) => match action {
                tray::TrayAction::Show => show_window(context),
                tray::TrayAction::LaunchLast => self.launch_selected(context.clone()),
                tray::TrayAction::Quit => context.send_viewport_cmd(egui::ViewportCommand::Close),
            },
        }
    }

    fn apply_handoff(&mut self, context: &egui::Context, arguments: &[String]) {
        match parse_handoff(arguments) {
            HandoffRequest::Activate => {
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
                if self.game_running {
                    self.set_status(StatusKind::Warning, "Minecraft is already running.");
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
                    self.avatar = Some(context.load_texture(
                        "kaelis-offline-profile-head",
                        steve_head(),
                        egui::TextureOptions::NEAREST,
                    ));
                    self.auth_status = format!("Offline profile ready: {}", profile.name);
                    self.start_launch(instance, identity, context.clone());
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
            self.sweeper = Some(kaelis_core::sweeper::spawn_background_sweeper(
                &self.runtime,
                300,
                3600,
            ));
        }
    }

    fn glass_frame() -> egui::Frame {
        egui::Frame::none()
            .fill(SURFACE)
            .stroke(egui::Stroke::new(1.0_f32, BORDER))
            .rounding(egui::Rounding::same(CORNER_LG))
            .shadow(egui::epaint::Shadow {
                offset: egui::vec2(0.0, 6.0),
                blur: 18.0,
                spread: 0.0,
                color: egui::Color32::from_black_alpha(52),
            })
            .inner_margin(egui::Margin::same(14.0))
    }

    /// Tighter sibling of [`Self::glass_frame`] for repeated list rows.
    fn card_frame() -> egui::Frame {
        egui::Frame::none()
            .fill(SURFACE_ALT)
            .stroke(egui::Stroke::new(1.0_f32, BORDER))
            .rounding(egui::Rounding::same(CORNER_MD))
            .inner_margin(egui::Margin::symmetric(10.0, 8.0))
    }

    fn paint_backdrop(context: &egui::Context) {
        let rect = context.screen_rect();
        let painter = context.layer_painter(egui::LayerId::background());
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(rect.left_top(), egui::Color32::from_rgb(9, 15, 25));
        mesh.colored_vertex(rect.right_top(), egui::Color32::from_rgb(12, 19, 32));
        mesh.colored_vertex(rect.right_bottom(), egui::Color32::from_rgb(5, 9, 16));
        mesh.colored_vertex(rect.left_bottom(), egui::Color32::from_rgb(4, 7, 12));
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(egui::Shape::mesh(mesh));
    }

    fn show_sidebar(&mut self, context: &egui::Context) {
        egui::SidePanel::left("kaelis-navigation")
            .resizable(false)
            .default_width(204.0)
            .frame(
                egui::Frame::none()
                    .fill(SURFACE)
                    .inner_margin(egui::Margin::symmetric(12.0, 12.0)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    draw_brand_mark(ui, 34.0);
                    ui.add_space(4.0);
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("KAELIS").strong().size(15.5));
                        ui.label(
                            egui::RichText::new("MINECRAFT LAUNCHER")
                                .size(8.5)
                                .color(MUTED),
                        );
                    });
                });
                ui.add_space(24.0);
                ui.label(
                    egui::RichText::new("WORKSPACE")
                        .size(9.5)
                        .strong()
                        .color(MUTED),
                );
                ui.add_space(4.0);
                for tab in Tab::ALL {
                    let selected = self.current_tab == tab;
                    let fill = if selected {
                        egui::Color32::from_rgba_unmultiplied(96, 196, 255, 18)
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    let text = egui::RichText::new(format!("        {}", tab.title()))
                        .size(11.5)
                        .color(if selected { TEXT } else { MUTED });
                    let response = ui.add_sized(
                        [ui.available_width(), 32.0],
                        egui::Button::new(text)
                            .fill(fill)
                            .stroke(egui::Stroke::NONE)
                            .rounding(egui::Rounding::same(CORNER_MD)),
                    );
                    if selected {
                        // A 2.5 px accent rail anchors the active row without
                        // boxing it in; the fill alone is too quiet to find.
                        ui.painter().rect_filled(
                            egui::Rect::from_min_size(
                                response.rect.left_top() + egui::vec2(0.0, 8.0),
                                egui::vec2(2.5, response.rect.height() - 16.0),
                            ),
                            egui::Rounding::same(1.5),
                            ACCENT,
                        );
                    }
                    paint_tab_icon(
                        ui.painter(),
                        response.rect.left_center() + egui::vec2(19.0, 0.0),
                        tab,
                        if selected { ACCENT } else { MUTED },
                    );
                    if response.clicked() {
                        self.current_tab = tab;
                    }
                    ui.add_space(1.0);
                }
                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.label(
                        egui::RichText::new(format!("{} · Rust 2024", kaelis_core::brand::COMPANY))
                            .size(9.0)
                            .color(MUTED),
                    );
                    ui.horizontal(|ui| {
                        ui.hyperlink_to(
                            egui::RichText::new("Community ↗").size(9.5).color(ACCENT),
                            kaelis_core::brand::DISCORD_HUB,
                        );
                        ui.hyperlink_to(
                            egui::RichText::new("GitHub ↗").size(9.5).color(MUTED),
                            kaelis_core::brand::GITHUB_REPOSITORY,
                        );
                    });
                    ui.hyperlink_to(
                        egui::RichText::new(format!("Founder {}", kaelis_core::brand::FOUNDER))
                            .size(9.0)
                            .color(MUTED),
                        kaelis_core::brand::FOUNDER_URL,
                    );
                    ui.add_space(10.0);
                    Self::glass_frame().show(ui, |ui| {
                        ui.set_min_width(158.0);
                        ui.horizontal(|ui| {
                            if let Some(avatar) = &self.avatar {
                                ui.add(
                                    egui::Image::new((avatar.id(), egui::vec2(34.0, 34.0)))
                                        .rounding(egui::Rounding::same(CORNER_MD)),
                                );
                            } else {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(34.0, 34.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(rect.center(), 16.0, GLASS_LIGHT);
                                ui.painter().text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    if self.microsoft_session.is_some()
                                        || self.offline_account.is_some()
                                    {
                                        self.account_name().chars().next().unwrap_or('K')
                                    } else {
                                        'K'
                                    },
                                    egui::FontId::proportional(15.0),
                                    ACCENT,
                                );
                            }
                            ui.vertical(|ui| {
                                ui.label(
                                    egui::RichText::new(self.account_name()).strong().size(12.5),
                                );
                                ui.label(
                                    egui::RichText::new(self.account_kind())
                                        .size(10.5)
                                        .color(MUTED),
                                );
                            });
                        });
                    });
                    ui.add_space(8.0);
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

    fn account_kind(&self) -> &'static str {
        if self.microsoft_session.is_some() {
            "Microsoft account"
        } else if self.offline_account.is_some() {
            "Offline profile"
        } else {
            "Not signed in"
        }
    }

    fn set_status(&mut self, kind: StatusKind, text: impl Into<String>) {
        self.status_kind = kind;
        self.status = text.into();
    }

    fn refresh_installed(&mut self) {
        let instance = self.selected_instance().cloned();
        self.installed = instance.as_ref().map(scan_installed);
        self.content_library = instance
            .as_ref()
            .map(ContentLibrary::scan)
            .unwrap_or_default();
        self.detected_loader = self
            .installed
            .as_ref()
            .and_then(|scan| scan.detected_loader)
            .map(str::to_owned);
        self.confirm_mod_delete = None;
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
        if self.game_running {
            self.set_status(
                StatusKind::Warning,
                "Close Minecraft before erasing content.",
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
        if self.game_running {
            self.set_status(
                StatusKind::Warning,
                "Close Minecraft before erasing an instance.",
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
            || self.installing_job.is_some()
            || self.installing_instance.is_some()
            || self.installing_runtime.is_some()
            || self.repairing_mods
    }

    fn fraction(progress: Option<(usize, usize)>) -> Option<f32> {
        progress
            .filter(|(_, total)| *total > 0)
            .map(|(completed, total)| (completed as f32 / total as f32).clamp(0.0, 1.0))
    }

    fn progress_fraction(&self) -> Option<f32> {
        Self::fraction(self.install_progress)
    }

    fn show_header(&self, context: &egui::Context) {
        egui::TopBottomPanel::top("kaelis-header")
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgba_premultiplied(10, 15, 24, 242))
                    .inner_margin(egui::Margin::symmetric(18.0, 7.0)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new("KAELIS WORKSPACE")
                                .size(9.0)
                                .color(MUTED),
                        );
                        ui.label(
                            egui::RichText::new(self.current_tab.title())
                                .size(18.0)
                                .strong(),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.game_running {
                            status_pill(ui, "PLAYING", StatusKind::Success.color());
                        }
                        if self.is_busy() {
                            ui.add(egui::Spinner::new().size(12.0));
                        }
                        status_pill(ui, &self.status, self.status_kind.color());
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

    fn show_footer(&self, context: &egui::Context) {
        egui::TopBottomPanel::bottom("kaelis-footer")
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::from_rgba_premultiplied(9, 13, 21, 235))
                    .inner_margin(egui::Margin::symmetric(18.0, 3.0)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(kaelis_core::brand::LAUNCHER_NAME)
                            .size(9.5)
                            .color(MUTED),
                    );
                    ui.label(egui::RichText::new("·").size(9.5).color(MUTED));
                    ui.hyperlink_to(
                        egui::RichText::new("Community").size(9.5).color(MUTED),
                        kaelis_core::brand::DISCORD_HUB,
                    );
                    ui.label(egui::RichText::new("·").size(9.5).color(MUTED));
                    ui.hyperlink_to(
                        egui::RichText::new("GitHub").size(9.5).color(MUTED),
                        kaelis_core::brand::GITHUB_REPOSITORY,
                    );
                    ui.label(egui::RichText::new("·").size(9.5).color(MUTED));
                    ui.label(
                        egui::RichText::new(format!("Founder {}", kaelis_core::brand::FOUNDER))
                            .size(9.5)
                            .color(MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let fps = if self.governor.target_fps() == 0 {
                            "Idle · event-driven"
                        } else {
                            "60 FPS while working"
                        };
                        ui.label(egui::RichText::new(fps).size(9.5).color(MUTED));
                    });
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
        ui.add_space(14.0);
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                draw_brand_mark(ui, 62.0);
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(kaelis_core::brand::TAGLINE)
                            .size(24.0)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new(
                            "One quiet workspace for instances, content, and accounts. Built in Rust, local-first, and fast by default.",
                        )
                        .size(11.5)
                        .color(MUTED),
                    );
                    ui.horizontal(|ui| {
                        if primary_button(ui, "Launch selected").clicked() {
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
        ui.add_space(10.0);
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("ACTIVE INSTANCE")
                        .size(9.0)
                        .strong()
                        .color(MUTED),
                );
                if let Some(instance) = &selected {
                    draw_instance_avatar(ui, selected_icon.as_ref(), instance);
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&instance.name).size(15.0).strong());
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
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Manage content").clicked() {
                        self.current_tab = Tab::Content;
                    }
                });
            });
        });
        ui.add_space(10.0);
        let mut open_kind = None;
        ui.columns(4, |columns| {
            for (column, (kind, count)) in columns.iter_mut().zip(counts.iter().copied()) {
                let card = Self::glass_frame().show(column, |ui| {
                    ui.label(
                        egui::RichText::new(kind.label().to_ascii_uppercase())
                            .size(8.5)
                            .strong()
                            .color(kind_color(kind)),
                    );
                    ui.label(egui::RichText::new(count.to_string()).size(24.0).strong());
                    ui.label(
                        egui::RichText::new("open in Content")
                            .size(9.5)
                            .color(MUTED),
                    );
                });
                if card
                    .response
                    .interact(egui::Sense::click())
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(format!("Manage {}", kind.label().to_ascii_lowercase()))
                    .clicked()
                {
                    open_kind = Some(kind);
                }
            }
        });
        if let Some(kind) = open_kind {
            self.content_kind = kind;
            self.current_tab = Tab::Content;
        }
        ui.add_space(10.0);
        ui.columns(2, |columns| {
            Self::glass_frame().show(&mut columns[0], |ui| {
                ui.label(egui::RichText::new("ACCOUNT").small().weak());
                ui.heading(self.account_name());
                ui.label(self.account_kind());
                if ui.button("Manage account").clicked() {
                    self.current_tab = Tab::Account;
                }
            });
            Self::glass_frame().show(&mut columns[1], |ui| {
                ui.label(egui::RichText::new("LATEST RELEASE").small().weak());
                ui.heading(
                    self.versions
                        .iter()
                        .find(|version| version.kind == "release")
                        .map(|version| version.id.as_str())
                        .unwrap_or("—"),
                );
                ui.label("Official Mojang version manifest");
            });
        });
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
                                .color(ACCENT),
                        );
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&edit.name).size(19.0).strong());
                            chip(ui, &format!("MC {}", edit.mc_version), MUTED);
                            chip(ui, &edit.loader, ACCENT);
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
                                        .desired_width(250.0),
                                );
                            });
                            editor_row(ui, "Group", |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.edit_group)
                                        .hint_text("Optional, e.g. Favourites")
                                        .desired_width(250.0),
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
                                    .desired_width(250.0),
                            );
                        },
                    );
                    editor_section(
                        &mut columns[0],
                        "Appearance",
                        "Card artwork and accent",
                        |ui| {
                            editor_row(ui, "Artwork", |ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.edit_icon)
                                        .hint_text("PNG / JPEG / WebP")
                                        .desired_width(250.0),
                                );
                                if ui.small_button("Clear").clicked() {
                                    self.edit_icon.clear();
                                }
                            });
                            editor_row(ui, "Accent", |ui| {
                                if ui
                                    .selectable_label(edit.icon_variant.is_none(), "Auto")
                                    .on_hover_text("Derive a stable color from the instance id.")
                                    .clicked()
                                {
                                    edit.icon_variant = None;
                                }
                                for index in 0..INSTANCE_VARIANTS {
                                    let selected = edit.icon_variant == Some(index);
                                    if swatch_button(ui, variant_color(index), selected)
                                        .on_hover_text(format!("Accent variant {}", index + 1))
                                        .clicked()
                                    {
                                        edit.icon_variant = Some(index);
                                    }
                                }
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
                                    "Mods run through the loader; resource packs, shaders, and worlds install without touching the mod graph.",
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
                                        .desired_width(250.0),
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
                                        .desired_width(250.0),
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
                                        .desired_width(250.0),
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
                                        .desired_width(250.0),
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
                                        .desired_width(250.0),
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
                                        .desired_width(250.0),
                                );
                                ui.label(
                                    egui::RichText::new("After exit")
                                        .size(10.0)
                                        .color(MUTED),
                                );
                                ui.add(
                                    egui::TextEdit::singleline(&mut self.edit_post_exit)
                                        .hint_text("Optional command")
                                        .desired_width(250.0),
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
                    if primary_button(ui, "Save changes").clicked() {
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
            self.mod_filter = ModFilter::All;
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
        if context.input(|input| input.modifiers.command && input.key_pressed(egui::Key::F)) {
            ui.memory_mut(|memory| memory.request_focus(egui::Id::new(INSTANCE_FILTER_ID)));
        }
        if self.editing.is_none() {
            ui.add_space(10.0);
            Self::glass_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.heading("Create an instance");
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
                            .desired_width(176.0),
                    );
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
                    let install_label = if busy { "Installing…" } else { "Create" };
                    if ui
                        .add_enabled(!busy && ready, egui::Button::new(install_label))
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
                });
                ui.horizontal_wrapped(|ui| {
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
                    let plan = if self.new_instance_name.trim().is_empty() {
                        ("Name the instance to begin.".to_owned(), MUTED)
                    } else if let Some(message) = &conflict {
                        (message.clone(), StatusKind::Warning.color())
                    } else if self.new_instance_version.trim().is_empty() {
                        ("Pick a Minecraft version.".to_owned(), MUTED)
                    } else if self.new_instance_loader.eq_ignore_ascii_case("vanilla") {
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
                            ACCENT_SOFT,
                        )
                    };
                    ui.label(egui::RichText::new(plan.0).size(10.0).color(plan.1));
                });
                ui.label(
                    egui::RichText::new(
                        "Java is provisioned on first play; existing instances are untouched.",
                    )
                    .size(9.5)
                    .color(MUTED),
                );
            });
        }
        self.show_instance_editor(ui, context);
        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.heading("Your instances");
            ui.add(
                egui::TextEdit::singleline(&mut self.instance_filter)
                    .id(egui::Id::new(INSTANCE_FILTER_ID))
                    .hint_text("Filter by name, group, version, or loader…")
                    .desired_width(290.0),
            );
            if !self.instance_filter.is_empty() && ui.small_button("×").clicked() {
                self.instance_filter.clear();
            }
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
                    egui::RichText::new(format!(
                        "{shown} of {} shown · Ctrl+F filters",
                        self.instances.len()
                    ))
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
                ui.label(
                    egui::RichText::new(
                        "Name an instance above, pick a Minecraft version, and Kaelis provisions Java and the client on first play.",
                    )
                    .size(11.0)
                    .color(MUTED),
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
                                instance_color(instance),
                            );
                            ui.add_space(6.0);
                            draw_instance_avatar(ui, icon, instance);
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
                                });
                                ui.label(egui::RichText::new(&card.detail).size(11.0));
                                ui.horizontal(|ui| {
                                    chip(ui, &card.content, instance_color(instance));
                                    chip(ui, &card.last_played, MUTED);
                                });
                                if !card.notes_preview.is_empty() {
                                    ui.label(
                                        egui::RichText::new(&card.notes_preview)
                                            .size(10.0)
                                            .italics()
                                            .color(ACCENT_SOFT),
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
                                        if ui.button("Open instance folder").clicked() {
                                            action =
                                                Some((index, InstanceAction::OpenFolder));
                                            ui.close_menu();
                                        }
                                        if ui.button("Open mods folder").clicked() {
                                            action =
                                                Some((index, InstanceAction::OpenMods));
                                            ui.close_menu();
                                        }
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
                                    if ui
                                        .add_enabled(
                                            !installing,
                                            egui::Button::new(
                                                egui::RichText::new("Play")
                                                    .strong()
                                                    .color(ACCENT_DARK),
                                            )
                                            .fill(ACCENT)
                                            .min_size(egui::vec2(62.0, 26.0)),
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
                InstanceAction::Select => {}
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
                InstanceAction::ManageContent(kind) => {
                    self.current_tab = Tab::Content;
                    self.content_kind = kind;
                    self.mod_results = ModSearchResults::default();
                    self.mod_rows.clear();
                    self.mod_icons.clear();
                    self.mod_results_query.clear();
                    self.mod_refine.clear();
                    self.mod_filter = ModFilter::All;
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
                ui.label(
                    egui::RichText::new(
                        "An instance keeps its modpack, mods, resource packs, and shaders together.",
                    )
                    .size(11.0)
                    .color(MUTED),
                );
                ui.add_space(6.0);
                if primary_button(ui, "Create an instance").clicked() {
                    self.current_tab = Tab::Instances;
                }
            });
            return;
        }
        if context.input(|input| input.modifiers.command && input.key_pressed(egui::Key::F)) {
            ui.memory_mut(|memory| memory.request_focus(egui::Id::new(MOD_SEARCH_ID)));
        }
        let dropped = context.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .find_map(|file| file.path.clone())
        });
        ui.add_space(4.0);
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("INSTANCE")
                        .size(9.0)
                        .strong()
                        .color(MUTED),
                );
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
                if let Some(instance) = self.selected_instance() {
                    chip(
                        ui,
                        &format!("MC {}", instance.mc_version),
                        ACCENT_SOFT,
                    );
                    chip(
                        ui,
                        &instance.loader,
                        if instance.loader.eq_ignore_ascii_case("vanilla") {
                            MUTED
                        } else {
                            ACCENT
                        },
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Open folder").clicked() {
                        request.open_folder = true;
                    }
                    if ui
                        .small_button("Rescan")
                        .on_hover_text("Re-read the instance folders from disk.")
                        .clicked()
                    {
                        request.rescan = true;
                    }
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
                        .size(11.0)
                        .color(if selected { TEXT } else { MUTED });
                    let button = egui::Button::new(text)
                        .fill(if selected { SURFACE_HOVER } else { SURFACE_ALT })
                        .stroke(egui::Stroke::new(
                            if selected { 1.2_f32 } else { 1.0 },
                            if selected { kind_color(kind) } else { BORDER },
                        ))
                        .rounding(egui::Rounding::same(CORNER_MD));
                    if ui.add_sized([tab_width, 28.0], button).clicked() && !selected {
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
                let width = (ui.available_width() - 300.0).max(200.0);
                let query = ui.add(
                    egui::TextEdit::singleline(&mut self.mod_query)
                        .id(egui::Id::new(MOD_SEARCH_ID))
                        .hint_text(hint)
                        .desired_width(width),
                );
                let text = self.mod_query.trim();
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
                let project = kaelis_api::mods::parse_project_link(text);
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
                        import = Some(ManualImport::File(PathBuf::from(text)));
                    } else {
                        request.search = true;
                    }
                }
                if (project.is_some() || link || file_like) && ui.small_button("Search text").clicked()
                {
                    request.search = true;
                }
                if !self.mod_query.is_empty() && ui.small_button("×").clicked() {
                    request.clear_search = true;
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
                    if let Some(path) = &dropped {
                        chip(
                            ui,
                            &format!("Dropped · {}", short_name(path)),
                            ACCENT_SOFT,
                        );
                    } else {
                        ui.label(
                            egui::RichText::new("or drag a .jar / .zip anywhere")
                                .size(9.5)
                                .color(MUTED),
                        );
                    }
                    if self.manual_busy {
                        ui.add(egui::Spinner::new().size(13.0));
                    }
                });
                if !self.manual_status.is_empty() {
                    ui.label(
                        egui::RichText::new(&self.manual_status)
                            .size(9.8)
                            .color(if self.manual_busy { ACCENT } else { MUTED }),
                    );
                }
            }
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                if self.content_kind.uses_loader() {
                    ui.label(egui::RichText::new("Loader").size(9.5).color(MUTED));
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
                        chip(
                            ui,
                            &format!("Instance is {}", instance.loader),
                            StatusKind::Warning.color(),
                        );
                    } else if let Some(detected) = self.detected_loader.as_deref() {
                        chip(
                            ui,
                            &format!("Detected {detected}"),
                            StatusKind::Success.color(),
                        );
                    }
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
                                "CurseForge packs need per-file API resolution; Modrinth .mrpack is supported.",
                            );
                    } else {
                        ui.checkbox(&mut self.mod_show_curseforge, "Search CurseForge");
                    }
                    ui.checkbox(&mut self.mod_auto_search, "Search as I type");
                });
                if self.content_kind != ContentKind::Modpack && self.cf_api_key.trim().is_empty() {
                    chip(ui, "CF key needed", StatusKind::Warning.color()).on_hover_text(
                        "CurseForge results need a free API key from console.curseforge.com (Settings > Integrations).",
                    );
                }
                if self.search_busy {
                    ui.add(egui::Spinner::new().size(13.0));
                }
            });
            if self.content_kind == ContentKind::Modpack {
                ui.horizontal_wrapped(|ui| {
                    chip(ui, "Whole instances", ACCENT_SOFT).on_hover_text(
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
            self.mod_filter = ModFilter::All;
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
                            .color(ACCENT),
                    );
                    if !self.modpack_detail.is_empty() {
                        ui.label(
                            egui::RichText::new(&self.modpack_detail)
                                .size(10.0)
                                .color(MUTED),
                        );
                    }
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
                            .color(ACCENT),
                    );
                    if let Some(detail) = &self.install_detail {
                        ui.label(egui::RichText::new(detail).size(10.0).color(MUTED));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if !self.install_queue.is_empty() {
                            if ui.small_button("Clear queue").clicked() {
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
                    if ui.small_button("Clear").clicked() {
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
                        if ui
                            .small_button("×")
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
        let right_width = (size.x * 0.34).clamp(248.0, 372.0);
        let left_width = (size.x - right_width - gap).max(280.0);
        let body_height = size.y.max(220.0);
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
        if request.load_more {
            self.start_mod_search_page(context.clone(), true);
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
            self.mod_filter = ModFilter::All;
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
        } else if let Some(path) = dropped {
            self.start_manual_import(ManualImport::File(path), context.clone());
        }
    }

    fn show_content_results(&mut self, ui: &mut egui::Ui, request: &mut ContentRequest) {
        let mut install_request: Option<usize> = None;
        let mut preview_request: Option<String> = None;
        let mut erase_request: Option<EraseTarget> = None;
        let visible = visible_mod_indices(
            &self.mod_results.items,
            &self.mod_rows,
            self.installed.as_ref(),
            Some(&self.content_library),
            &self.mod_refine,
            ModView {
                filter: self.mod_filter,
                sort: self.mod_sort,
                show_modrinth: self.mod_show_modrinth,
                show_curseforge: self.mod_show_curseforge,
            },
        );
        let shown = visible.len();
        let loaded = self.mod_results.items.len();
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("BROWSE")
                        .size(9.0)
                        .strong()
                        .color(MUTED),
                );
                let width = (ui.available_width() - 24.0).max(80.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.mod_refine)
                        .id(egui::Id::new(MOD_REFINE_ID))
                        .hint_text("Filter these results…")
                        .desired_width(width),
                );
                if !self.mod_refine.is_empty() && ui.small_button("×").clicked() {
                    self.mod_refine.clear();
                }
            });
            ui.horizontal_wrapped(|ui| {
                for filter in ModFilter::ALL {
                    if ui
                        .selectable_label(self.mod_filter == filter, filter.label())
                        .clicked()
                    {
                        self.mod_filter = filter;
                    }
                }
                egui::ComboBox::from_id_source("kaelis-mod-sort")
                    .selected_text(self.mod_sort.label())
                    .width(128.0)
                    .show_ui(ui, |ui| {
                        for sort in ModSort::ALL {
                            ui.selectable_value(&mut self.mod_sort, sort, sort.label());
                        }
                    });
                if loaded > 0 {
                    ui.label(
                        egui::RichText::new(format!("{shown} of {loaded}"))
                            .size(9.5)
                            .color(MUTED),
                    );
                }
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
                            "Type a name, paste a Modrinth or CurseForge project link, or drop a {} file here. Ctrl+F focuses the box.",
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
        let list_height = (ui.available_height() - 34.0).max(120.0);
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
                            ACCENT,
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
                        Some(ModInstallState::Failed(error)) => {
                            (format!("Failed · {error}"), StatusKind::Error.color())
                        }
                        None => (String::new(), MUTED),
                    };
                    let mut clicked = false;
                    let mut preview_clicked = false;
                    Self::card_frame().show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_top(|ui| {
                            draw_mod_icon(ui, icon, item);
                            ui.add_space(6.0);
                            let text_width = (ui.available_width() - 96.0).max(120.0);
                            ui.vertical(|ui| {
                                ui.set_max_width(text_width);
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(&item.title).strong().size(13.5));
                                    if let Some((_, disabled)) = installed_entry {
                                        chip(
                                            ui,
                                            if disabled {
                                                "Installed · off"
                                            } else {
                                                "Installed"
                                            },
                                            if disabled {
                                                StatusKind::Warning.color()
                                            } else {
                                                StatusKind::Success.color()
                                            },
                                        );
                                    } else if state_label.is_empty() {
                                        chip(ui, &item.provider.to_string(), MUTED);
                                    }
                                });
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} · {}",
                                        row.author_line, row.meta_line
                                    ))
                                    .size(9.5)
                                    .color(MUTED),
                                );
                                ui.label(egui::RichText::new(&item.description).size(10.5));
                                ui.horizontal_wrapped(|ui| {
                                    chip(ui, &row.compatibility_line, MUTED);
                                    if !item.client_side.is_empty()
                                        && item.client_side != "unspecified"
                                    {
                                        chip(
                                            ui,
                                            &format!("Client {}", item.client_side),
                                            StatusKind::Info.color(),
                                        );
                                    }
                                    if !row.categories_line.is_empty() {
                                        chip(ui, &row.categories_line, MUTED);
                                    }
                                });
                                if !state_label.is_empty() {
                                    ui.label(
                                        egui::RichText::new(&state_label)
                                            .size(10.0)
                                            .color(state_color),
                                    );
                                }
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
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
                                    // every state change.
                                    let button = ui
                                        .add_enabled(
                                            !running && !queued,
                                            egui::Button::new(label)
                                                .min_size(egui::vec2(96.0, 0.0)),
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
                    ui.add_space(4.0);
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
                                        .color(if active { ACCENT_SOFT } else { TEXT });
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
                });
            });
            let width = (ui.available_width() - 2.0).max(80.0);
            ui.add(
                egui::TextEdit::singleline(&mut filter)
                    .id(egui::Id::new(INSTALLED_FILTER_ID))
                    .hint_text("Filter installed…")
                    .desired_width(width),
            );
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
                    if kind == ContentKind::Mod {
                        let Some(scan) = self.installed.as_ref() else {
                            return;
                        };
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
                                .color(ACCENT_SOFT),
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
                            "Creates a new instance with this pack's Minecraft version, loader, and mods. {} project metadata shown; no history is stored.",
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

    fn show_account(&mut self, ui: &mut egui::Ui, context: &egui::Context) {
        ui.add_space(10.0);
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if let Some(avatar) = &self.avatar {
                    ui.add(
                        egui::Image::new((avatar.id(), egui::vec2(58.0, 58.0)))
                            .rounding(egui::Rounding::same(CORNER_MD)),
                    );
                } else {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(58.0, 58.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, CORNER_MD, GLASS_LIGHT);
                    let signed_in = self.microsoft_session.is_some()
                        || self.offline_account.is_some();
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        if signed_in {
                            self.account_name().chars().next().unwrap_or('K')
                        } else {
                            'K'
                        },
                        egui::FontId::proportional(24.0),
                        ACCENT,
                    );
                }
                ui.add_space(10.0);
                ui.vertical(|ui| {
                    let signed_in = self.microsoft_session.is_some()
                        || self.offline_account.is_some();
                    ui.label(
                        egui::RichText::new(if signed_in {
                            self.account_name()
                        } else {
                            "Guest session".to_owned()
                        })
                        .size(20.0)
                        .strong(),
                    );
                    ui.label(
                        egui::RichText::new(self.account_kind())
                            .size(10.5)
                            .color(if self.microsoft_session.is_some() {
                                StatusKind::Success.color()
                            } else {
                                MUTED
                            }),
                    );
                    let uuid = self
                        .microsoft_session
                        .as_ref()
                        .map(|session| session.profile.id.as_str())
                        .or_else(|| {
                            self.offline_account
                                .as_ref()
                                .map(|profile| profile.uuid.as_str())
                        })
                        .unwrap_or("");
                    if !uuid.is_empty() {
                        ui.label(egui::RichText::new(uuid).size(9.0).color(MUTED));
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let signed_in = self.microsoft_session.is_some()
                        || self.offline_account.is_some();
                    if signed_in && ui.small_button("Copy name").clicked() {
                        ui.output_mut(|output| {
                            output.copied_text = self.account_name();
                        });
                    }
                });
            });
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "One identity at a time. Offline profiles are local and cannot join online-mode servers.",
                )
                .size(10.0)
                .color(MUTED),
            );
        });
        ui.add_space(10.0);
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.heading("Microsoft account");
            ui.label("The browser flow uses authorization code + PKCE. Device code is available when a tenant blocks it. Refresh tokens never leave the OS credential store.");
            if self.ms_client_id.trim().is_empty()
                && std::env::var_os("KAELIS_MS_CLIENT_ID").is_none()
            {
                ui.colored_label(
                    StatusKind::Warning.color(),
                    "Microsoft sign-in needs a free public-client ID. Add one in Settings > Integrations; offline play works without it.",
                );
                if ui.button("Open Settings > Integrations").clicked() {
                    self.current_tab = Tab::Settings;
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !self.auth_busy,
                        egui::Button::new("Sign in with browser"),
                    )
                    .on_hover_text(
                        "Opens Microsoft in your default browser and returns through a loopback redirect on this machine.",
                    )
                    .clicked()
                {
                    self.start_browser_sign_in(context.clone());
                }
                if ui
                    .add_enabled(!self.auth_busy, egui::Button::new("Use device code"))
                    .on_hover_text(
                        "Shows a short code and opens Microsoft's verification page. Useful when a local browser callback is blocked.",
                    )
                    .clicked()
                {
                    self.start_sign_in(context.clone());
                }
                if ui
                    .add_enabled(self.auth_busy, egui::Button::new("Cancel sign-in"))
                    .clicked()
                {
                    if let Some(cancel) = self.auth_cancel.take() {
                        let _ = cancel.send(true);
                    }
                    self.auth_busy = false;
                    self.auth_device = None;
                    self.auth_status = "Sign-in cancelled.".to_owned();
                }
                if (self.microsoft_session.is_some() || self.offline_account.is_some())
                    && ui.button("Sign out and forget saved token").clicked()
                {
                    self.forget_account(context.clone());
                }
            });
            ui.label(&self.auth_status);
            egui::CollapsingHeader::new("Set up Microsoft sign-in (one time)")
                .default_open(false)
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(
                            "1. Open the Entra app registrations page and choose New registration.\n2. Name it Kaelis and pick “Personal Microsoft accounts only” (or your preferred account type).\n3. Authentication > Add platform > Mobile and desktop applications. Add http://localhost as the redirect URI.\n4. Advanced settings > Allow public client flows > Yes. Save.\n5. Copy the Application (client) ID into Settings > Integrations.",
                        )
                        .size(10.0)
                        .color(MUTED),
                    );
                    ui.hyperlink_to(
                        egui::RichText::new("Open Entra app registrations ↗").size(10.5),
                        AZURE_APP_REGISTRATION,
                    );
                    ui.label(
                        egui::RichText::new(
                            "No client secret is used. PKCE and device code are public-client flows; no Microsoft Graph permission is required beyond XboxLive.signin.",
                        )
                        .size(9.5)
                        .color(MUTED),
                    );
                });
            if let Some(device) = &self.auth_device {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label("Open this page and enter the code:");
                        ui.label(
                            egui::RichText::new(&device.user_code)
                                .size(28.0)
                                .strong()
                                .color(ACCENT),
                        );
                        let uri = device
                            .verification_uri_complete
                            .clone()
                            .unwrap_or_else(|| device.verification_uri.clone());
                        if ui.button("Open sign-in page").clicked() {
                            context.open_url(egui::OpenUrl::new_tab(uri.clone()));
                        }
                        if ui.button("Copy code").clicked() {
                            ui.output_mut(|output| {
                                output.copied_text = device.user_code.clone();
                            });
                        }
                        ui.hyperlink_to(&device.verification_uri, uri);
                        if let Some(message) = &device.message {
                            ui.label(egui::RichText::new(message).small().weak());
                        }
                    });
                    show_qr(ui, device);
                });
            }
        });
        if let Some(session) = &self.microsoft_session {
            ui.add_space(10.0);
            Self::glass_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    if let Some(avatar) = &self.avatar {
                        ui.add(
                            egui::Image::new((avatar.id(), egui::vec2(42.0, 42.0)))
                                .rounding(egui::Rounding::same(9.0)),
                        );
                    }
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(&session.profile.name)
                                .size(15.0)
                                .strong(),
                        );
                        ui.label(
                            egui::RichText::new("Microsoft account · online mode ready")
                                .size(10.5)
                                .color(StatusKind::Success.color()),
                        );
                        ui.label(
                            egui::RichText::new(&session.profile.id)
                                .size(9.5)
                                .color(MUTED),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Copy UUID").clicked() {
                            ui.output_mut(|output| {
                                output.copied_text = session.profile.id.clone();
                            });
                        }
                    });
                });
            });
        }
        ui.add_space(10.0);
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.heading("Offline profile");
            ui.label("Creates the vanilla OfflinePlayer UUID. Offline accounts cannot access online-mode servers.");
            ui.horizontal(|ui| {
                ui.text_edit_singleline(&mut self.offline_name);
                if ui.button("Use offline profile").clicked() {
                    self.sign_in_offline(context.clone());
                }
            });
        });
        if let Some(profile) = &self.offline_account {
            Self::glass_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let image = steve_head();
                    let texture = ui.ctx().load_texture(
                        "kaelis-offline-account-preview",
                        image,
                        egui::TextureOptions::NEAREST,
                    );
                    ui.add(
                        egui::Image::new((texture.id(), egui::vec2(44.0, 44.0)))
                            .rounding(egui::Rounding::same(CORNER_SM)),
                    );
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&profile.name).strong().size(14.0));
                        ui.label(
                            egui::RichText::new("Offline profile · local UUID")
                                .size(9.5)
                                .color(MUTED),
                        );
                        ui.label(egui::RichText::new(&profile.uuid).size(9.0).color(MUTED));
                    });
                });
            });
        }
        ui.add_space(10.0);
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.heading("Security");
            ui.label(
                egui::RichText::new(
                    "Refresh tokens live in the operating-system credential store; access tokens and profile heads stay in memory. Nothing is written to instance JSON, logs, or the vault.",
                )
                .size(10.0)
                .color(MUTED),
            );
            ui.hyperlink_to(
                egui::RichText::new("Microsoft device-code guidance ↗").color(ACCENT),
                AZURE_DEVICE_CODE_DOC,
            );
        });
    }

    fn show_settings(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.spacing_mut().item_spacing.y = 5.0;
        let mut save_automation = false;
        let mut zoom_request: Option<f32> = None;
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_label(ui, "INTERFACE");
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new("Scale").size(10.5).color(MUTED));
                for zoom in [1.0_f32, 1.1, 1.25, 1.5] {
                    let selected = (self.ui_zoom - zoom).abs() < 0.001;
                    if ui
                        .selectable_label(selected, format!("{}%", (zoom * 100.0).round() as u32))
                        .clicked()
                    {
                        zoom_request = Some(zoom);
                    }
                }
                ui.label(
                    egui::RichText::new(
                        "Ctrl +/− zoom · Ctrl+0 resets · window size and position are remembered",
                    )
                    .size(9.5)
                    .color(MUTED),
                );
            });
        });
        if let Some(zoom) = zoom_request {
            self.set_ui_zoom(zoom, ui.ctx());
        }
        ui.add_space(6.0);
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_label(ui, "PERFORMANCE");
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Heap").size(10.5).color(MUTED));
                ui.add(
                    egui::Slider::new(&mut self.settings_ram_mb, 1024..=32768)
                        .step_by(512.0)
                        .suffix(" MiB"),
                );
                let fps = self.governor.target_fps();
                ui.label(
                    egui::RichText::new(if fps == 0 {
                        "UI idle · event-driven".to_owned()
                    } else {
                        format!("UI {fps} FPS · while working")
                    })
                    .size(9.5)
                    .color(MUTED),
                );
            });
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
                        .button(format!("Apply to {name}"))
                        .on_hover_text("Saves the heap and JVM arguments to this instance.")
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
                    ui.add_enabled(false, egui::Button::new("Apply to selected instance"))
                        .on_disabled_hover_text(
                            "Select an instance on the Overview, Instances, or Content tab first.",
                        );
                }
            }
            ui.label(
                egui::RichText::new(
                    "The runtime keeps its own G1 ergonomics; custom arguments are appended verbatim.",
                )
                .size(9.5)
                .color(MUTED),
            );
        });
        ui.add_space(8.0);
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_label(ui, "AUTOMATION");
            if ui
                .checkbox(
                    &mut self.auto_repair_before_launch,
                    "Repair the mod environment before every launch",
                )
                .on_hover_text(
                    "Scans the instance's mods, installs missing required mods (for example ukulib for uku's Armor HUD), then launches.",
                )
                .changed()
            {
                save_automation = true;
            }
            if ui
                .checkbox(
                    &mut self.auto_resolve_duplicates,
                    "Disable older duplicate mods automatically",
                )
                .on_hover_text(
                    "After an install, keeps the newest jar when a top-level mod id is duplicated and every other provided id remains satisfied.",
                )
                .changed()
            {
                save_automation = true;
            }
            if ui
                .checkbox(
                    &mut self.auto_clean_junk,
                    "Clean junk at every launch",
                )
                .on_hover_text(
                    "Deletes logs, crash reports, and interrupted-download staging files from every instance at startup. Saves, mods, configs, and options are never touched.",
                )
                .changed()
            {
                save_automation = true;
            }
            ui.horizontal(|ui| {
                if ui.button("Repair now").clicked() {
                    self.repair_mod_environment(ui.ctx().clone());
                }
                if ui
                    .button("Clean junk now")
                    .on_hover_text("Remove logs, crash reports, and staging leftovers from every instance.")
                    .clicked()
                {
                    self.clean_junk_now(ui.ctx().clone());
                }
            });
            ui.label(
                egui::RichText::new(
                    "Repair resolves each missing mod id on Modrinth, installs the full required dependency graph, provisions Fabric API when its modules are absent, and disables only provably redundant duplicate jars.",
                )
                .size(9.5)
                .color(MUTED),
            );
        });
        if save_automation {
            self.persist_settings();
        }
        ui.add_space(8.0);
        let mut save_integrations = false;
        Self::glass_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section_label(ui, "INTEGRATIONS");
            ui.label(
                egui::RichText::new(
                    "Microsoft uses the device-code flow. Register a public client in Azure with XboxLive.signin and offline_access, then paste its application (client) ID. The browser opens automatically when a code is issued.",
                )
                .small()
                .weak(),
            );
            ui.hyperlink_to(
                egui::RichText::new("Device-code setup guide ↗").size(10.5),
                AZURE_DEVICE_CODE_DOC,
            );
            ui.horizontal(|ui| {
                ui.label("Microsoft client ID");
                ui.add(
                    egui::TextEdit::singleline(&mut self.ms_client_id)
                        .hint_text("00000000-0000-0000-0000-000000000000")
                        .desired_width(330.0),
                );
                if !self.ms_client_id.trim().is_empty() {
                    dot_label(ui, "configured", StatusKind::Success.color());
                }
            });
            ui.horizontal(|ui| {
                ui.label("CurseForge API key");
                ui.add(
                    egui::TextEdit::singleline(&mut self.cf_api_key)
                        .password(true)
                        .hint_text("Stored in the OS credential store")
                        .desired_width(280.0),
                );
                if !self.cf_api_key.trim().is_empty() {
                    dot_label(ui, "configured", StatusKind::Success.color());
                }
                ui.hyperlink_to(
                    egui::RichText::new("Get a key ↗").size(10.5),
                    CURSEFORGE_CONSOLE,
                );
            });
            ui.horizontal(|ui| {
                if ui.button("Save integrations").clicked() {
                    save_integrations = true;
                }
                ui.label(
                    egui::RichText::new(&self.integrations_status)
                        .small()
                        .weak(),
                );
            });
            ui.label(
                egui::RichText::new(
                    "The CurseForge key is never written to instance JSON, logs, or the vault. Environment variables KAELIS_MS_CLIENT_ID and CF_API_KEY remain supported fallbacks.",
                )
                .small()
                .weak(),
            );
        });
        if save_integrations {
            self.save_integrations();
        }
        ui.add_space(8.0);
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
                        egui::RichText::new(format!(
                            "{} {} · {} · built in Rust",
                            kaelis_core::brand::LAUNCHER_NAME,
                            env!("CARGO_PKG_VERSION"),
                            kaelis_core::brand::TAGLINE
                        ))
                        .size(9.5)
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
                    .on_hover_text(
                        "Version, OS, paths, and an instance summary. No tokens, keys, or account identifiers.",
                    )
                    .clicked()
                {
                    let text = diagnostics_text(&self.instances, self.selected_instance());
                    ui.ctx().copy_text(text);
                    self.set_status(
                        StatusKind::Success,
                        "Diagnostics copied. No tokens, keys, or account identifiers included.",
                    );
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
                    egui::RichText::new(format!(
                        "Founder {} ↗",
                        kaelis_core::brand::FOUNDER
                    ))
                    .size(10.5),
                    kaelis_core::brand::FOUNDER_URL,
                );
            });
            ui.label(
                egui::RichText::new(
                    "Logs live in a bounded RAM ring and vanish with the process. No telemetry, analytics, or background upload exists.",
                )
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
        }
    }
}

impl eframe::App for KaelisApp {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        if self.editing.is_none()
            && std::env::var_os("KAELIS_SCREENSHOT_EDIT").is_some()
            && let Some(instance) = self.instances.first().cloned()
        {
            self.begin_edit(instance);
        }
        while let Ok(event) = self.receiver.try_recv() {
            self.handle_event(context, event);
        }
        // Zoom shortcuts work everywhere, like a browser: Ctrl +/- scales the
        // whole interface and Ctrl+0 returns to 100 %.
        let (zoom_in, zoom_out, zoom_reset) = context.input(|input| {
            if !input.modifiers.command {
                return (false, false, false);
            }
            (
                input.key_pressed(egui::Key::Plus) || input.key_pressed(egui::Key::Equals),
                input.key_pressed(egui::Key::Minus),
                input.key_pressed(egui::Key::Num0),
            )
        });
        if zoom_in || zoom_out || zoom_reset {
            let target = if zoom_reset {
                1.0
            } else if zoom_in {
                self.ui_zoom + 0.1
            } else {
                self.ui_zoom - 0.1
            };
            self.set_ui_zoom(target, context);
        }
        self.governor.focused = context.input(|input| input.focused);
        // Screenshot mode walks a fixed number of frames, so it must keep
        // repainting even when nothing is animating.
        self.governor.animating = self.is_busy() || self.screenshot_path.is_some();
        self.governor.apply(context);
        Self::paint_backdrop(context);
        self.show_header(context);
        self.show_sidebar(context);
        self.show_footer(context);
        egui::CentralPanel::default()
            .frame(
                egui::Frame::none()
                    .fill(egui::Color32::TRANSPARENT)
                    .inner_margin(egui::Margin::symmetric(18.0, 8.0)),
            )
            .show(context, |ui| self.show_tab(ui, context));
        self.show_content_preview(context);
        self.capture_screenshot(context);
    }
}

fn default_auto_repair() -> bool {
    true
}

fn default_ui_zoom() -> f32 {
    1.0
}

#[derive(serde::Serialize, serde::Deserialize)]
struct LauncherSettings {
    #[serde(default)]
    ms_client_id: String,
    #[serde(default)]
    last_account: String,
    /// Install missing required mods automatically before the next launch.
    #[serde(default = "default_auto_repair")]
    auto_repair: bool,
    /// Disable older duplicate primary mod ids after an install when the
    /// cleanup is provably safe.
    #[serde(default = "default_auto_repair")]
    auto_resolve_duplicates: bool,
    /// Delete logs, crash reports, and interrupted-download staging files from
    /// every instance at launch. Saves, mods, and configuration are never
    /// included.
    #[serde(default = "default_auto_repair")]
    auto_clean: bool,
    /// Last Content channel, so the tab reopens where the user left it.
    #[serde(default)]
    content_kind: ContentKind,
    /// Interface zoom (`1.0` = 100 %). Persisted so the window reopens at the
    /// same scale on high-DPI and small displays alike.
    #[serde(default = "default_ui_zoom")]
    ui_zoom: f32,
    /// Last offline profile name, so the Steve-headed local account survives a
    /// restart instead of having to be typed again.
    #[serde(default)]
    offline_account: String,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self {
            ms_client_id: String::new(),
            last_account: String::new(),
            auto_repair: default_auto_repair(),
            auto_resolve_duplicates: default_auto_repair(),
            auto_clean: default_auto_repair(),
            content_kind: ContentKind::Mod,
            ui_zoom: default_ui_zoom(),
            offline_account: String::new(),
        }
    }
}

fn launcher_settings_path() -> PathBuf {
    let instances = Instance::instances_dir();
    instances
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| instances.clone())
        .join("settings.json")
}

fn load_launcher_settings() -> LauncherSettings {
    std::fs::read(launcher_settings_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_launcher_settings(settings: &LauncherSettings) -> Result<(), String> {
    let path = launcher_settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let bytes = serde_json::to_vec_pretty(settings).map_err(|error| error.to_string())?;
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, bytes).map_err(|error| error.to_string())?;
    std::fs::rename(&temp, &path).map_err(|error| error.to_string())
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
    context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
    context.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
    context.send_viewport_cmd(egui::ViewportCommand::Focus);
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
    installed: Option<&InstalledScan>,
    content: Option<&ContentLibrary>,
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
            let is_installed = if item.kind == ContentKind::Mod {
                installed
                    .is_some_and(|scan| scan.match_file(&item.slug, &row.match_tokens).is_some())
            } else {
                content.is_some_and(|library| {
                    library
                        .match_entry(item.kind, &item.slug, &row.match_tokens)
                        .is_some()
                })
            };
            match view.filter {
                ModFilter::Installed if !is_installed => return None,
                ModFilter::NotInstalled if is_installed => return None,
                _ => {}
            }
            Some(index)
        })
        .collect::<Vec<_>>();
    visible.sort_by(|left, right| {
        let left_item = &items[*left];
        let right_item = &items[*right];
        match view.sort {
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
        }
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

/// Palette used by instance artwork. A fixed variant makes neighbouring
/// instances easy to tell apart without requiring an image file.
fn variant_color(index: u8) -> egui::Color32 {
    match usize::from(index) % usize::from(INSTANCE_VARIANTS) {
        0 => egui::Color32::from_rgb(112, 190, 255),
        1 => egui::Color32::from_rgb(126, 231, 165),
        2 => egui::Color32::from_rgb(255, 195, 120),
        3 => egui::Color32::from_rgb(202, 166, 255),
        4 => egui::Color32::from_rgb(255, 148, 178),
        _ => egui::Color32::from_rgb(120, 220, 232),
    }
}

fn instance_variant(id: &str) -> u8 {
    let hash = id.bytes().fold(0_u32, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    (hash % u32::from(INSTANCE_VARIANTS)) as u8
}

fn instance_color(instance: &Instance) -> egui::Color32 {
    variant_color(
        instance
            .icon_variant
            .unwrap_or_else(|| instance_variant(&instance.id)),
    )
}

fn draw_instance_avatar(
    ui: &mut egui::Ui,
    texture: Option<&egui::TextureHandle>,
    instance: &Instance,
) {
    let size = egui::vec2(40.0, 40.0);
    if let Some(texture) = texture {
        ui.add(egui::Image::new((texture.id(), size)).rounding(egui::Rounding::same(10.0)));
        return;
    }
    let (avatar, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    ui.painter()
        .circle_filled(avatar.center(), 19.0, GLASS_LIGHT);
    ui.painter().text(
        avatar.center(),
        egui::Align2::CENTER_CENTER,
        instance.name.chars().next().unwrap_or('K'),
        egui::FontId::proportional(17.0),
        instance_color(instance),
    );
}

/// Reveal an instance folder or its metadata file with the platform's file
/// manager. The process is detached, so a missing shell never blocks the UI.
fn open_instance_path(path: &std::path::Path, reveal: bool) -> Result<(), String> {
    let mut command = if cfg!(windows) {
        std::process::Command::new("explorer")
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else {
        std::process::Command::new("xdg-open")
    };
    if cfg!(windows) && reveal {
        command.arg(format!("/select,{}", path.display()));
    } else {
        command.arg(path);
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Small vector glyphs keep the navigation legible even when the platform
/// font lacks geometric Unicode symbols.
fn paint_tab_icon(painter: &egui::Painter, center: egui::Pos2, tab: Tab, color: egui::Color32) {
    let stroke = egui::Stroke::new(1.25_f32, color);
    match tab {
        Tab::Overview => {
            for x in [-1.0_f32, 1.0] {
                for y in [-1.0_f32, 1.0] {
                    painter.rect_filled(
                        egui::Rect::from_center_size(
                            center + egui::vec2(x * 3.2, y * 3.2),
                            egui::vec2(4.6, 4.6),
                        ),
                        egui::Rounding::same(1.2),
                        color,
                    );
                }
            }
        }
        Tab::Instances => {
            painter.rect_stroke(
                egui::Rect::from_center_size(center + egui::vec2(0.0, 1.0), egui::vec2(11.0, 9.0)),
                egui::Rounding::same(2.0),
                stroke,
            );
            painter.line_segment(
                [
                    center + egui::vec2(-4.0, -4.8),
                    center + egui::vec2(4.0, -4.8),
                ],
                stroke,
            );
        }
        Tab::Content => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    center + egui::vec2(0.0, -6.0),
                    center + egui::vec2(2.2, -2.2),
                    center + egui::vec2(6.0, 0.0),
                    center + egui::vec2(2.2, 2.2),
                    center + egui::vec2(0.0, 6.0),
                    center + egui::vec2(-2.2, 2.2),
                    center + egui::vec2(-6.0, 0.0),
                    center + egui::vec2(-2.2, -2.2),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        Tab::Account => {
            painter.circle_stroke(center + egui::vec2(0.0, -2.6), 2.8, stroke);
            painter.add(egui::Shape::line(
                vec![
                    center + egui::vec2(-5.2, 5.4),
                    center + egui::vec2(-3.2, 2.6),
                    center + egui::vec2(3.2, 2.6),
                    center + egui::vec2(5.2, 5.4),
                ],
                stroke,
            ));
        }
        Tab::Settings => {
            painter.circle_stroke(center, 2.8, stroke);
            for index in 0..8 {
                let angle = index as f32 * std::f32::consts::TAU / 8.0;
                let direction = egui::vec2(angle.cos(), angle.sin());
                painter.line_segment([center + direction * 4.4, center + direction * 5.8], stroke);
            }
        }
    }
}

/// Window and taskbar icon generated from the same geometry as the in-app
/// mark. Rendered at 4x and box-filtered, so no image decoder or asset file
/// is needed at startup.
pub fn window_icon() -> egui::IconData {
    const SIZE: usize = 64;
    const SCALE: usize = 4;
    const HIGH: usize = SIZE * SCALE;
    let mut high = vec![[0_u8; 4]; HIGH * HIGH];
    for y in 0..HIGH {
        for x in 0..HIGH {
            let p = egui::pos2(
                (x as f32 + 0.5) / SCALE as f32,
                (y as f32 + 0.5) / SCALE as f32,
            );
            let inset = 3.0;
            let half = 64.0 - inset;
            let radius = 34.0;
            let q = egui::vec2(
                (p.x - 64.0).abs() - (half - radius),
                (p.y - 64.0).abs() - (half - radius),
            );
            let outside = egui::vec2(q.x.max(0.0), q.y.max(0.0)).length();
            let distance = outside + q.x.max(q.y).min(0.0) - radius;
            if distance > 0.5 {
                continue;
            }
            let edge = (0.5 - distance).clamp(0.0, 1.0);
            let blend = ((p.x + p.y) / 256.0).clamp(0.0, 1.0);
            let plate = egui::Color32::from_rgb(
                (17.0 + (7.0 - 17.0) * blend) as u8,
                (28.0 + (12.0 - 28.0) * blend) as u8,
                (45.0 + (22.0 - 45.0) * blend) as u8,
            );
            let mut color = plate;
            if distance > -1.5 {
                color = egui::Color32::from_rgb(55, 80, 109);
            }
            let strokes = [
                (egui::pos2(40.0, 36.0), egui::pos2(40.0, 92.0)),
                (egui::pos2(40.0, 64.0), egui::pos2(76.0, 36.0)),
                (egui::pos2(40.0, 64.0), egui::pos2(76.0, 92.0)),
            ];
            let mut on_stroke = false;
            for (start, end) in strokes {
                let segment = end - start;
                let relative = p - start;
                let t = (relative.dot(segment) / segment.length_sq()).clamp(0.0, 1.0);
                if (relative - segment * t).length() <= 5.6 {
                    on_stroke = true;
                }
            }
            if on_stroke {
                color = egui::Color32::from_rgb(112, 202, 255);
            }
            if (p - egui::pos2(90.0, 36.0)).length() <= 5.0 {
                color = egui::Color32::from_rgb(195, 236, 255);
            }
            high[y * HIGH + x] = [color.r(), color.g(), color.b(), (edge * 255.0) as u8];
        }
    }
    let mut pixels = Vec::with_capacity(SIZE * SIZE * 4);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let mut sums = [0_u32; 4];
            for sy in 0..SCALE {
                for sx in 0..SCALE {
                    let pixel = high[(y * SCALE + sy) * HIGH + x * SCALE + sx];
                    sums[0] += pixel[0] as u32;
                    sums[1] += pixel[1] as u32;
                    sums[2] += pixel[2] as u32;
                    sums[3] += pixel[3] as u32;
                }
            }
            let samples = (SCALE * SCALE) as u32;
            pixels.push((sums[0] / samples) as u8);
            pixels.push((sums[1] / samples) as u8);
            pixels.push((sums[2] / samples) as u8);
            pixels.push((sums[3] / samples) as u8);
        }
    }
    egui::IconData {
        rgba: pixels,
        width: SIZE as u32,
        height: SIZE as u32,
    }
}

fn draw_brand_mark(ui: &mut egui::Ui, size: f32) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, egui::Rounding::same(size * 0.3), ACCENT_DARK);
    painter.rect_stroke(
        rect,
        egui::Rounding::same(size * 0.3),
        egui::Stroke::new(1.0_f32, BORDER_BRIGHT),
    );
    let inset = size * 0.26;
    let left = rect.left() + inset;
    let top = rect.top() + inset;
    let bottom = rect.bottom() - inset;
    let right = rect.right() - inset;
    let stroke = egui::Stroke::new(size * 0.09, ACCENT);
    painter.line_segment([egui::pos2(left, top), egui::pos2(left, bottom)], stroke);
    painter.line_segment(
        [egui::pos2(left, rect.center().y), egui::pos2(right, top)],
        stroke,
    );
    painter.line_segment(
        [egui::pos2(left, rect.center().y), egui::pos2(right, bottom)],
        stroke,
    );
    painter.circle_filled(egui::pos2(right, top), size * 0.045, ACCENT_SOFT);
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
    format!("{mods} mods · {packs} packs · {shaders} shaders · {worlds} worlds")
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
    if unix_seconds <= 0 {
        return "Never played".to_owned();
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
        ContentKind::Mod => ACCENT,
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

/// A round colour swatch for the accent picker, painted rather than typed.
fn swatch_button(ui: &mut egui::Ui, color: egui::Color32, selected: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::click());
    let painter = ui.painter();
    if selected {
        painter.circle_stroke(rect.center(), 9.0, egui::Stroke::new(1.5_f32, TEXT));
    }
    painter.circle_filled(rect.center(), 6.5, color);
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    response
}

fn status_pill(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    // The pill usually sits in a right-to-left row; size it explicitly so the
    // dot-then-label layout works without stretching the header.
    let font = egui::FontId::proportional(10.0);
    let text_width = ui.fonts(|fonts| {
        fonts
            .layout_no_wrap(text.to_owned(), font.clone(), color)
            .size()
            .x
    });
    egui::Frame::none()
        .fill(egui::Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            20,
        ))
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 46),
        ))
        .rounding(egui::Rounding::same(CORNER_MD))
        .inner_margin(egui::Margin::symmetric(9.0, 3.0))
        .show(ui, |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(text_width + 12.0, 13.0),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
                    ui.painter().circle_filled(rect.center(), 2.6, color);
                    ui.label(egui::RichText::new(text).font(font).color(color));
                },
            );
        });
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
                .size(11.5)
                .color(egui::Color32::WHITE),
        )
        .rounding(egui::Rounding::same(CORNER_SM)),
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
        egui::Button::new(egui::RichText::new(label).size(11.5).color(DANGER))
            .rounding(egui::Rounding::same(CORNER_SM)),
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

fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .size(12.0)
                .strong()
                .color(ACCENT_DARK),
        )
        .fill(ACCENT)
        .rounding(egui::Rounding::same(8.0))
        .min_size(egui::vec2(112.0, 28.0)),
    )
}

fn editor_section(ui: &mut egui::Ui, title: &str, hint: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(SURFACE_ALT)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .rounding(egui::Rounding::same(10.0))
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
            [70.0, 20.0],
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
        ui.add(egui::Image::new((texture.id(), size)).rounding(egui::Rounding::same(8.0)));
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

fn decode_instance_icon(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    decode_mod_icon(bytes).map(center_square)
}

fn decode_mod_icon(bytes: &[u8]) -> Result<egui::ColorImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = image::Limits::no_limits();
    limits.max_image_width = Some(MOD_ICON_MAX_EDGE);
    limits.max_image_height = Some(MOD_ICON_MAX_EDGE);
    limits.max_alloc = Some(MOD_ICON_MAX_ALLOC);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|error| error.to_string())?
        .to_rgba8();
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
    let side = 172.0;
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
        CheckState, HandoffRequest, INSTANCE_VARIANTS, InstalledFile, InstalledScan,
        InstanceHealth, KaelisApp, LauncherSettings, ModFilter, ModSort, ModView, StatusKind,
        build_mod_rows, center_square, content_list_text, decode_instance_icon, decode_mod_icon,
        draw_instance_avatar, draw_mod_icon, filter_tokens, https_icon_url, install_state_key,
        instance_color, instance_matches, instance_name_conflict, instance_variant, mod_item_key,
        note_preview, parse_handoff, parse_jvm_args, scan_installed, significant_tokens, slugify,
        valid_instance_version, variant_color, visible_mod_indices, window_icon,
    };
    use kaelis_api::mods::{ModProvider, ModSearchItem};
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

    fn test_view(filter: ModFilter, sort: ModSort, modrinth: bool, curseforge: bool) -> ModView {
        ModView {
            filter,
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

        let installed = installed_scan("lithium-fabric-0.12.jar", "lithium");
        let visible = visible_mod_indices(
            &items,
            &rows,
            Some(&installed),
            None,
            "",
            test_view(ModFilter::Installed, ModSort::Relevance, true, true),
        );
        assert_eq!(visible, vec![2]);

        let visible = visible_mod_indices(
            &items,
            &rows,
            Some(&installed),
            None,
            "sodium",
            test_view(ModFilter::NotInstalled, ModSort::Downloads, true, true),
        );
        assert_eq!(visible, vec![0, 1]);

        let refined = visible_mod_indices(
            &items,
            &rows,
            None,
            None,
            "extra",
            test_view(ModFilter::All, ModSort::Relevance, true, true),
        );
        assert_eq!(refined, vec![0]);
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
    fn window_icon_is_square_with_a_transparent_corner() {
        let icon = window_icon();
        assert_eq!((icon.width, icon.height), (64, 64));
        assert_eq!(icon.rgba.len(), 64 * 64 * 4);
        assert_eq!(icon.rgba[3], 0, "corner must stay transparent");
        let center = (32 * 64 + 32) * 4;
        assert_eq!(icon.rgba[center + 3], 255, "plate center must be opaque");
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
                None,
                None,
                "",
                test_view(ModFilter::All, ModSort::Downloads, true, false)
            ),
            vec![0]
        );
        assert_eq!(
            visible_mod_indices(
                &items,
                &rows,
                None,
                None,
                "",
                test_view(ModFilter::All, ModSort::Downloads, false, true)
            ),
            vec![1]
        );
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
    fn instance_variants_override_and_wrap_the_palette() {
        let mut instance = Instance::new(
            "fabulous-1-21",
            "Fabulous",
            "1.21",
            "fabric",
            PathBuf::from("kaelis-variant-test"),
        );
        instance.icon_variant = Some(0);
        assert_eq!(instance_color(&instance), variant_color(0));
        instance.icon_variant = Some(INSTANCE_VARIANTS + 2);
        assert_eq!(instance_color(&instance), variant_color(2));
        assert_ne!(variant_color(0), variant_color(1));
        // A missing variant falls back to the stable id-derived colour.
        instance.icon_variant = None;
        assert_eq!(
            instance_color(&instance),
            variant_color(instance_variant(&instance.id))
        );
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
}
