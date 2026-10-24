//! Kaelis entry point: claim the OS mutex before creating a runtime, parse a
//! small CLI surface, then run the desktop app or the requested operation.

use kaelis_api::accounts::{
    AccountKind, active_account, load_settings, migrated_accounts, switch_account,
};
use kaelis_api::auth::offline_profile;
use kaelis_api::launch::{LaunchIdentity, run_instance};
use kaelis_core::{
    ephemeral_log::EphemeralLog,
    handoff::{self, HandoffServer},
    instance::{DuplicateOptions, Instance},
    single_instance::{SingleInstance, SingleInstanceError},
};
use std::process::ExitCode;

#[derive(Debug)]
enum Action {
    Gui,
    Help,
    Version {
        json: bool,
    },
    ListVersions {
        show_alpha: bool,
        show_beta: bool,
        show_snapshots: bool,
        json: bool,
    },
    Offline {
        name: String,
        json: bool,
    },
    Play {
        instance_id: String,
        /// Explicit offline name; otherwise the saved active offline account is
        /// used, so the CLI and the GUI launch as the same identity.
        username: Option<String>,
    },
    InstallMod {
        instance_id: String,
        spec: String,
    },
    Repair {
        instance_id: String,
    },
    ImportPack {
        instance_id: String,
        pack_path: String,
    },
    ExportInstance {
        instance_id: String,
        destination: String,
    },
    ImportInstance {
        archive: String,
    },
    /// Copy one instance into a new one, optionally with its world saves.
    Duplicate {
        instance_id: String,
        saves: bool,
        json: bool,
    },
    /// Machine-readable inventory for scripts, MCP servers, and CI.
    ListInstances {
        json: bool,
    },
    /// Machine-readable environment report.
    Status {
        json: bool,
    },
    /// Every saved identity, with the one the next launch will use.
    Accounts {
        json: bool,
    },
    /// Choose the saved identity the next launch uses.
    UseAccount {
        name: String,
        json: bool,
    },
    /// Compare installed mods with the newest compatible Modrinth versions.
    CheckUpdates {
        instance_id: String,
        json: bool,
    },
    /// Download and install every available mod update.
    UpdateMods {
        instance_id: String,
        json: bool,
    },
}

fn main() -> ExitCode {
    let action = match parse_args(std::env::args().skip(1)) {
        Ok(action) => action,
        Err(error) => {
            eprintln!("[Kaelis] {error}\n\n{}", usage());
            return ExitCode::from(2);
        }
    };
    // A double-clicked GUI binary gets its own console window; hide it before
    // the first expensive step so it never flashes over the desktop.
    if matches!(&action, Action::Gui) {
        hide_own_console();
    }
    // Screenshot mode is a development-only visual check. It must be able to
    // run beside an installed launcher without claiming the global mutex or
    // handoff endpoint, and it closes itself after one captured frame.
    let screenshot_mode = std::env::var_os("KAELIS_SCREENSHOT").is_some();
    // Headless commands (help, offline profile, version list, mod install) are
    // safe to run beside an open launcher. Only GUI and game launches use the
    // single-instance guard and forward into the primary window.
    let interactive = matches!(&action, Action::Gui | Action::Play { .. });
    let _single_instance = if interactive && !screenshot_mode {
        match SingleInstance::claim() {
            Ok(guard) => Some(guard),
            Err(SingleInstanceError::AlreadyRunning(_)) => return forward_to_primary(),
            Err(error) => {
                eprintln!("[Kaelis] could not claim single-instance guard: {error}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    // The primary process hosts one bounded local channel so a second launch
    // can hand its arguments over instead of silently exiting.
    let handoff = if matches!(&action, Action::Gui) && !screenshot_mode {
        match HandoffServer::bind() {
            Ok(server) => Some(server),
            Err(error) => {
                eprintln!("[Kaelis] single-instance handoff unavailable: {error}");
                None
            }
        }
    } else {
        None
    };

    match action {
        Action::Help => {
            println!("{}", usage());
            ExitCode::SUCCESS
        }
        Action::Version { json } => {
            if json {
                print_json(serde_json::json!({
                    "name": kaelis_core::brand::LAUNCHER_NAME,
                    "version": env!("CARGO_PKG_VERSION"),
                    "platform": kaelis_core::brand::build_platform(),
                    "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
                    "website": kaelis_core::brand::WEBSITE,
                    "checksums": kaelis_core::brand::CHECKSUMS_URL,
                }));
                return ExitCode::SUCCESS;
            }
            println!("{}", kaelis_core::brand::build_identity());
            println!("site: {}", kaelis_core::brand::WEBSITE);
            println!("checksums: {}", kaelis_core::brand::CHECKSUMS_URL);
            ExitCode::SUCCESS
        }
        Action::Offline { name, json } => {
            let profile = offline_profile(&name);
            let head = kaelis_api::auth::crafatar_head_url(&profile.uuid, 64);
            if json {
                print_json(serde_json::json!({
                    "name": profile.name,
                    "uuid": profile.uuid,
                    "head": head,
                }));
                return ExitCode::SUCCESS;
            }
            println!("offline profile: {} ({})", profile.name, profile.uuid);
            println!("head: {head}");
            ExitCode::SUCCESS
        }
        Action::ListInstances { json } => list_instances(json),
        Action::Status { json } => status(json),
        Action::Accounts { json } => list_accounts(json),
        Action::UseAccount { name, json } => use_account(&name, json),
        Action::CheckUpdates { instance_id, json } => check_mod_updates(&instance_id, json, false),
        Action::UpdateMods { instance_id, json } => check_mod_updates(&instance_id, json, true),
        Action::ListVersions {
            show_alpha,
            show_beta,
            show_snapshots,
            json,
        } => list_versions(show_alpha, show_beta, show_snapshots, json),
        Action::Play {
            instance_id,
            username,
        } => play_instance(&instance_id, username.as_deref()),
        Action::InstallMod { instance_id, spec } => install_mod(&instance_id, &spec),
        Action::Repair { instance_id } => repair_instance(&instance_id),
        Action::ImportPack {
            instance_id,
            pack_path,
        } => import_pack(&instance_id, &pack_path),
        Action::ExportInstance {
            instance_id,
            destination,
        } => export_instance(&instance_id, &destination),
        Action::ImportInstance { archive } => import_instance(&archive),
        Action::Duplicate {
            instance_id,
            saves,
            json,
        } => duplicate_instance(&instance_id, saves, json),
        Action::Gui => run_gui(handoff),
    }
}

/// Hide the console Windows creates for a double-clicked GUI binary.
///
/// The process deliberately keeps the console subsystem so `--help`, `--play`
/// and the headless commands still print when run from a terminal. A console
/// shared with the launching shell has more than one attached process and is
/// left untouched; an explorer-created console belongs only to us and is
/// hidden, so the desktop app opens without a black window behind it.
#[cfg(windows)]
fn hide_own_console() {
    use windows_sys::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
    use windows_sys::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

    // SAFETY: both calls only read the current process console and toggle the
    // visibility of the window handle the OS just returned.
    unsafe {
        let window = GetConsoleWindow();
        if window.is_null() {
            return;
        }
        let mut processes = [0_u32; 2];
        let attached = GetConsoleProcessList(processes.as_mut_ptr(), processes.len() as u32);
        if attached <= 1 {
            ShowWindow(window, SW_HIDE);
        }
    }
}

#[cfg(not(windows))]
fn hide_own_console() {}

fn forward_to_primary() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    match handoff::forward(&arguments) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // The previous behavior was a silent successful exit; keep the exit
            // code stable for scripts and surface why the handoff was lost.
            eprintln!("[Kaelis] another instance is running, but handoff failed: {error}");
            ExitCode::SUCCESS
        }
    }
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Action, String> {
    let mut args = args.peekable();
    let mut help = false;
    let mut version = false;
    let mut list_versions = false;
    let mut show_alpha = false;
    let mut show_beta = false;
    let mut show_snapshots = true;
    let mut offline_name = None;
    let mut instance_id = None;
    let mut repair_instance_id = None;
    let mut install_mod = None;
    let mut import_pack = None;
    let mut export_instance = None;
    let mut import_instance = None;
    let mut duplicate_instance = None;
    let mut duplicate_with_worlds = false;
    let mut list_instances = false;
    let mut status = false;
    let mut accounts_report = false;
    let mut use_account = None;
    let mut check_updates = None;
    let mut update_mods = None;
    let mut json = false;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--help" | "-h" => help = true,
            "--version" | "-V" => version = true,
            "--list-versions" => list_versions = true,
            "--show-alpha" => show_alpha = true,
            "--show-beta" => show_beta = true,
            "--hide-snapshots" => show_snapshots = false,
            "--list-instances" => list_instances = true,
            "--json" => json = true,
            "--status" => status = true,
            "--accounts" => accounts_report = true,
            "--check-updates" => {
                check_updates = Some(
                    args.next()
                        .filter(|value| !value.starts_with('-'))
                        .ok_or_else(|| "--check-updates requires an instance id".to_owned())?,
                );
            }
            "--update-mods" => {
                update_mods = Some(
                    args.next()
                        .filter(|value| !value.starts_with('-'))
                        .ok_or_else(|| "--update-mods requires an instance id".to_owned())?,
                );
            }
            "--use-account" => {
                use_account = Some(
                    args.next()
                        .filter(|value| !value.starts_with('-'))
                        .ok_or_else(|| "--use-account requires an account name".to_owned())?,
                );
            }
            "--offline" => {
                offline_name = Some(
                    args.next()
                        .filter(|value| !value.starts_with('-'))
                        .ok_or_else(|| "--offline requires a username".to_owned())?,
                );
            }
            "--play" => {
                instance_id = Some(
                    args.next()
                        .filter(|value| !value.starts_with('-'))
                        .ok_or_else(|| "--play requires an instance id".to_owned())?,
                );
            }
            "--install-mod" => {
                let target = args
                    .next()
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| "--install-mod requires an instance id".to_owned())?;
                let spec = args
                    .next()
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| {
                        "--install-mod requires modrinth:<project> or curseforge:<id>".to_owned()
                    })?;
                install_mod = Some((target, spec));
            }
            "--repair" => {
                repair_instance_id = Some(
                    args.next()
                        .filter(|value| !value.starts_with('-'))
                        .ok_or_else(|| "--repair requires an instance id".to_owned())?,
                );
            }
            "--import-modpack" => {
                let target = args
                    .next()
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| "--import-modpack requires an instance id".to_owned())?;
                let pack_path = args
                    .next()
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| "--import-modpack requires a .mrpack or .zip path".to_owned())?;
                import_pack = Some((target, pack_path));
            }
            "--export-instance" => {
                let target = args
                    .next()
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| "--export-instance requires an instance id".to_owned())?;
                let destination = args
                    .next()
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| {
                        "--export-instance requires a .zip destination path".to_owned()
                    })?;
                export_instance = Some((target, destination));
            }
            "--import-instance" => {
                import_instance = Some(
                    args.next()
                        .filter(|value| !value.starts_with('-'))
                        .ok_or_else(|| "--import-instance requires an archive path".to_owned())?,
                );
            }
            "--duplicate" => {
                duplicate_instance = Some(
                    args.next()
                        .filter(|value| !value.starts_with('-'))
                        .ok_or_else(|| "--duplicate requires an instance id".to_owned())?,
                );
            }
            "--with-worlds" => duplicate_with_worlds = true,
            unknown => return Err(format!("unknown option: {unknown}")),
        }
    }
    if help {
        return Ok(Action::Help);
    }
    if json
        && !version
        && offline_name.is_none()
        && !list_instances
        && !status
        && !list_versions
        && !accounts_report
        && use_account.is_none()
        && check_updates.is_none()
        && update_mods.is_none()
        && duplicate_instance.is_none()
    {
        return Err(
            "--json refines --version, --offline, --list-versions, --list-instances, --status, --accounts or --use-account"
                .to_owned(),
        );
    }
    if accounts_report && use_account.is_some() {
        return Err("--accounts and --use-account are separate commands".to_owned());
    }
    if accounts_report || use_account.is_some() {
        let mixed = version
            || list_versions
            || list_instances
            || status
            || instance_id.is_some()
            || offline_name.is_some()
            || install_mod.is_some()
            || import_pack.is_some()
            || export_instance.is_some()
            || import_instance.is_some()
            || duplicate_instance.is_some()
            || repair_instance_id.is_some();
        if mixed {
            return Err("--accounts and --use-account are standalone commands".to_owned());
        }
        if accounts_report {
            return Ok(Action::Accounts { json });
        }
        if let Some(name) = use_account {
            return Ok(Action::UseAccount { name, json });
        }
    }
    if version {
        return Ok(Action::Version { json });
    }
    if check_updates.is_some() && update_mods.is_some() {
        return Err("--check-updates and --update-mods are separate operations".to_owned());
    }
    if check_updates.is_some() || update_mods.is_some() {
        let apply = update_mods.is_some();
        let mixed = version
            || list_versions
            || list_instances
            || status
            || accounts_report
            || use_account.is_some()
            || instance_id.is_some()
            || offline_name.is_some()
            || install_mod.is_some()
            || import_pack.is_some()
            || export_instance.is_some()
            || import_instance.is_some()
            || duplicate_instance.is_some()
            || repair_instance_id.is_some();
        if mixed {
            return Err("--check-updates and --update-mods are standalone operations".to_owned());
        }
        let target = check_updates
            .or(update_mods)
            .expect("a checked instance id is present");
        return Ok(if apply {
            Action::UpdateMods {
                instance_id: target,
                json,
            }
        } else {
            Action::CheckUpdates {
                instance_id: target,
                json,
            }
        });
    }
    if list_instances && status {
        return Err("--list-instances and --status are separate reports".to_owned());
    }
    if (list_instances || status)
        && (instance_id.is_some()
            || offline_name.is_some()
            || install_mod.is_some()
            || import_pack.is_some()
            || export_instance.is_some()
            || import_instance.is_some()
            || duplicate_instance.is_some()
            || repair_instance_id.is_some()
            || list_versions)
    {
        return Err("--list-instances and --status are standalone reports".to_owned());
    }
    if list_instances {
        return Ok(Action::ListInstances { json });
    }
    if status {
        return Ok(Action::Status { json });
    }
    if list_versions && (offline_name.is_some() || instance_id.is_some() || install_mod.is_some()) {
        return Err(
            "--list-versions cannot be combined with --offline, --play or --install-mod".to_owned(),
        );
    }
    if install_mod.is_some() && (instance_id.is_some() || offline_name.is_some()) {
        return Err("--install-mod cannot be combined with --play or --offline".to_owned());
    }
    if repair_instance_id.is_some()
        && (instance_id.is_some()
            || offline_name.is_some()
            || install_mod.is_some()
            || import_pack.is_some()
            || list_versions)
    {
        return Err("--repair is a standalone operation".to_owned());
    }
    if import_pack.is_some() && (instance_id.is_some() || offline_name.is_some()) {
        return Err("--import-modpack cannot be combined with --play or --offline".to_owned());
    }
    if export_instance.is_some()
        && (instance_id.is_some()
            || offline_name.is_some()
            || install_mod.is_some()
            || import_pack.is_some()
            || import_instance.is_some()
            || duplicate_instance.is_some()
            || repair_instance_id.is_some()
            || list_versions)
    {
        return Err("--export-instance is a standalone operation".to_owned());
    }
    if import_instance.is_some()
        && (instance_id.is_some()
            || offline_name.is_some()
            || install_mod.is_some()
            || import_pack.is_some()
            || duplicate_instance.is_some()
            || repair_instance_id.is_some()
            || list_versions)
    {
        return Err("--import-instance is a standalone operation".to_owned());
    }
    if install_mod.is_some() && import_pack.is_some() {
        return Err("--install-mod and --import-modpack are separate operations".to_owned());
    }
    if duplicate_with_worlds && duplicate_instance.is_none() {
        return Err("--with-worlds requires --duplicate".to_owned());
    }
    if let Some(duplicate_id) = duplicate_instance {
        let mixed = version
            || list_versions
            || list_instances
            || status
            || instance_id.is_some()
            || offline_name.is_some()
            || install_mod.is_some()
            || import_pack.is_some()
            || export_instance.is_some()
            || import_instance.is_some()
            || repair_instance_id.is_some();
        if mixed {
            return Err("--duplicate is a standalone operation".to_owned());
        }
        return Ok(Action::Duplicate {
            instance_id: duplicate_id,
            saves: duplicate_with_worlds,
            json,
        });
    }
    if list_versions {
        return Ok(Action::ListVersions {
            show_alpha,
            show_beta,
            show_snapshots,
            json,
        });
    }
    if let Some((instance_id, spec)) = install_mod {
        return Ok(Action::InstallMod { instance_id, spec });
    }
    if let Some(instance_id) = repair_instance_id {
        return Ok(Action::Repair { instance_id });
    }
    if let Some((instance_id, destination)) = export_instance {
        return Ok(Action::ExportInstance {
            instance_id,
            destination,
        });
    }
    if let Some(archive) = import_instance {
        return Ok(Action::ImportInstance { archive });
    }
    if let Some((instance_id, pack_path)) = import_pack {
        return Ok(Action::ImportPack {
            instance_id,
            pack_path,
        });
    }
    if let Some(instance_id) = instance_id {
        return Ok(Action::Play {
            instance_id,
            username: offline_name,
        });
    }
    if let Some(name) = offline_name {
        return Ok(Action::Offline { name, json });
    }
    if show_alpha || show_beta || !show_snapshots {
        return Err("version filters require --list-versions".to_owned());
    }
    Ok(Action::Gui)
}

fn usage() -> &'static str {
    "Kaelis — Rust Minecraft Java launcher\n\
     Usage:\n\
       kaelis [--help | --version]\n\
       kaelis --list-versions [--show-alpha] [--show-beta] [--hide-snapshots]\n\
       kaelis --offline <username>\n\
       kaelis --play <instance-id> [--offline <username>]\n\
       kaelis --install-mod <instance-id> <modrinth:project|curseforge:id|slug>\n\
       kaelis --repair <instance-id>\n\
       kaelis --import-modpack <instance-id> <pack.mrpack|pack.zip>\n\
       kaelis --export-instance <instance-id> <destination.kaelis.zip>\n\
       kaelis --import-instance <kaelis.zip|prism.zip|curseforge.zip|pack.zip>\n\
       kaelis --duplicate <instance-id> [--with-worlds] [--json]\n\
       kaelis --list-instances [--json]\n\
       kaelis --status [--json]\n\
       kaelis --accounts [--json]\n\
       kaelis --use-account <name> [--json]\n\
       kaelis --check-updates <instance-id> [--json]\n\
       kaelis --update-mods <instance-id> [--json]\n\
     With no arguments, open the Kaelis desktop launcher.\n\
     --play without --offline uses the saved offline account that is active, so\n\
     a command-line launch and the desktop launch agree.\n\
     --use-account switches the identity the next launch uses; an unknown but\n\
     valid offline username is added to the saved list. It changes no token, so\n\
     a Microsoft row needs a token stored by a previous GUI sign-in.\n\
     --install-mod also installs every required dependency, and --repair installs\n\
     every required mod that the instance is missing.\n\
     --check-updates hashes installed mods and asks Modrinth for the newest\n\
     compatible version of each; --update-mods also downloads and verifies every\n\
     update, keeping disabled jars disabled.\n\
     --export-instance writes a portable archive. --import-instance reads\n\
     Kaelis, Prism/MultiMC, and CurseForge archives and loose game-folder zips,\n\
     creating an instance under a free id.\n\
     --duplicate copies mods, packs, shaders, settings, and notes under a free\n\
     id; add --with-worlds to copy saves too. Logs and crash reports never copy.\n\
     --json prints one machine-readable document instead of prose; every report\n\
     is stable across versions so scripts and MCP servers can parse it."
}

fn effective_loader(instance: &Instance) -> String {
    kaelis_api::modmeta::detect_loader(&instance.root.join("mods"))
        .map(str::to_owned)
        .unwrap_or_else(|| instance.loader.clone())
}

fn install_mod(instance_id: &str, spec: &str) -> ExitCode {
    // A bare project slug is convenient for scripts and defaults to Modrinth.
    let (provider, project) = match spec.split_once(':') {
        Some((provider, project)) => {
            let provider = match provider.to_ascii_lowercase().as_str() {
                "modrinth" => kaelis_api::mods::ModProvider::Modrinth,
                "curseforge" => kaelis_api::mods::ModProvider::CurseForge,
                other => {
                    eprintln!("[Kaelis] unknown provider {other}; use modrinth or curseforge");
                    return ExitCode::from(2);
                }
            };
            (provider, project)
        }
        None => (kaelis_api::mods::ModProvider::Modrinth, spec),
    };
    let instance = match Instance::load(instance_id) {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("[Kaelis] could not load instance {instance_id}: {error}");
            return ExitCode::FAILURE;
        }
    };
    if instance.loader.eq_ignore_ascii_case("vanilla") {
        eprintln!("[Kaelis] mods require a Fabric, Quilt, Forge or NeoForge instance");
        return ExitCode::FAILURE;
    }
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[Kaelis] {error}");
            return ExitCode::FAILURE;
        }
    };
    let http = match http_client() {
        Ok(http) => http,
        Err(error) => {
            eprintln!("[Kaelis] {error}");
            return ExitCode::FAILURE;
        }
    };
    // CurseForge needs no stored credential: content resolves through the
    // public index, and an approved key would come from the environment.
    let platforms = kaelis_api::mods::ModPlatformClient::new(http);
    let loader = effective_loader(&instance);
    let mut item = kaelis_api::mods::ModSearchItem {
        provider,
        id: project.to_owned(),
        // A CurseForge spec may be a slug or an id; the slug field carries it
        // so the public file index can resolve either form.
        slug: project.to_owned(),
        title: project.to_owned(),
        description: String::new(),
        downloads: 0,
        icon_url: None,
        ..kaelis_api::mods::ModSearchItem::default()
    };
    if provider == kaelis_api::mods::ModProvider::Modrinth
        && let Some(resolved) =
            runtime.block_on(platforms.resolve_project(project, &instance.mc_version, &loader))
    {
        item = resolved;
    }
    let mods_dir = instance.root.join("mods");
    match runtime.block_on(platforms.install_with_dependencies(
        &item,
        &instance.mc_version,
        &loader,
        &mods_dir,
        true,
    )) {
        Ok(report) => {
            println!("{}", report.summary());
            for path in &report.installed {
                println!("  {}", path.display());
            }
            for note in &report.notes {
                println!("note: {note}");
            }
            for warning in &report.warnings {
                eprintln!("[Kaelis] warning: {warning}");
            }
            // Installing one mod can expose ids that only a repair pass can
            // settle, such as Fabric API's virtual modules.
            let repair = runtime.block_on(kaelis_api::repair::repair_environment(
                &platforms,
                &instance.mc_version,
                &loader,
                &mods_dir,
            ));
            for note in &repair.notes {
                println!("repair: {note}");
            }
            for warning in &repair.warnings {
                eprintln!("[Kaelis] repair warning: {warning}");
            }
            if !repair.unresolved.is_empty() {
                eprintln!(
                    "[Kaelis] unresolved dependencies: {}",
                    repair.unresolved.join(", ")
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[Kaelis] mod install failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn repair_instance(instance_id: &str) -> ExitCode {
    let instance = match Instance::load(instance_id) {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("[Kaelis] could not load instance {instance_id}: {error}");
            return ExitCode::FAILURE;
        }
    };
    if instance.loader.eq_ignore_ascii_case("vanilla") {
        eprintln!("[Kaelis] mods require a Fabric, Quilt, Forge or NeoForge instance");
        return ExitCode::FAILURE;
    }
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(error) => return fail(error),
    };
    let http = match http_client() {
        Ok(http) => http,
        Err(error) => return fail(error),
    };
    // CurseForge needs no stored credential: content resolves through the
    // public index, and an approved key would come from the environment.
    let platforms = kaelis_api::mods::ModPlatformClient::new(http);
    let loader = effective_loader(&instance);
    let report = runtime.block_on(kaelis_api::repair::repair_environment(
        &platforms,
        &instance.mc_version,
        &loader,
        &instance.root.join("mods"),
    ));
    println!("{}", report.summary());
    for path in &report.installed {
        println!("  {}", path.display());
    }
    for note in &report.notes {
        println!("note: {note}");
    }
    for warning in &report.warnings {
        eprintln!("[Kaelis] warning: {warning}");
    }
    if !report.unresolved.is_empty() {
        eprintln!(
            "[Kaelis] unresolved dependencies: {}",
            report.unresolved.join(", ")
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Compare installed mods with Modrinth, optionally applying every update.
fn check_mod_updates(instance_id: &str, json: bool, apply: bool) -> ExitCode {
    let instance = match Instance::load(instance_id) {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("[Kaelis] could not load instance {instance_id}: {error}");
            return ExitCode::FAILURE;
        }
    };
    if instance.loader.eq_ignore_ascii_case("vanilla") {
        eprintln!("[Kaelis] mod updates require a Fabric, Quilt, Forge or NeoForge instance");
        return ExitCode::FAILURE;
    }
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(error) => return fail(error),
    };
    let http = match http_client() {
        Ok(http) => http,
        Err(error) => return fail(error),
    };
    let report = match runtime.block_on(kaelis_api::updates::check_updates(&http, &instance)) {
        Ok(report) => report,
        Err(error) => return fail(format!("could not check mod updates: {error}")),
    };
    let mods_dir = instance.root.join("mods");
    let mut applied = Vec::new();
    let mut failures = Vec::new();
    if apply {
        for update in &report.updates {
            match runtime.block_on(kaelis_api::updates::apply_update(&http, &mods_dir, update)) {
                Ok(path) => applied.push(
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                ),
                Err(error) => failures.push(format!("{}: {error}", update.title)),
            }
        }
    }
    if json {
        print_json(serde_json::json!({
            "instance": instance.id,
            "mc_version": instance.mc_version,
            "loader": instance.loader,
            "checked": report.checked,
            "up_to_date": report.up_to_date,
            "unmatched": report.unmatched,
            "skipped": report.skipped,
            "updates": report.updates,
            "applied": applied,
            "failed": failures,
        }));
    } else {
        println!("{}", report.summary());
        for update in &report.updates {
            println!(
                "  {}  {} → {}{} ({})",
                update.title,
                update.current_file,
                update.latest_version,
                if update.channel == "release" {
                    String::new()
                } else {
                    format!(" [{}]", update.channel)
                },
                kaelis_api::portable::human_bytes(update.size)
            );
        }
        if report.skipped > 0 {
            println!(
                "  skipped {} file(s) beyond the per-run request cap",
                report.skipped
            );
        }
        if apply {
            for name in &applied {
                println!("updated {name}");
            }
        }
        for failure in &failures {
            eprintln!("[Kaelis] update failed: {failure}");
        }
    }
    if failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn import_pack(instance_id: &str, pack_path: &str) -> ExitCode {
    let instance = match Instance::load(instance_id) {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("[Kaelis] could not load instance {instance_id}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[Kaelis] {error}");
            return ExitCode::FAILURE;
        }
    };
    let http = match http_client() {
        Ok(http) => http,
        Err(error) => {
            eprintln!("[Kaelis] {error}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(kaelis_api::mrpack::install_mrpack(
        &http,
        std::path::Path::new(pack_path),
        &instance.root,
    )) {
        Ok(count) => {
            println!("Imported {count} files into {}.", instance.root.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[Kaelis] modpack import failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Write one portable archive for a saved instance.
fn export_instance(instance_id: &str, destination: &str) -> ExitCode {
    let instance = match Instance::load(instance_id) {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("[Kaelis] could not load instance {instance_id}: {error}");
            return ExitCode::FAILURE;
        }
    };
    match kaelis_api::portable::export_instance(&instance, std::path::Path::new(destination)) {
        Ok(report) => {
            println!("{} → {destination}", report.summary());
            for skipped in report.skipped.iter().take(10) {
                println!("  skipped {skipped}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[Kaelis] instance export failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Copy one saved instance under a fresh id, reporting real file counts.
fn duplicate_instance(instance_id: &str, saves: bool, json: bool) -> ExitCode {
    let instance = match Instance::load(instance_id) {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("[Kaelis] could not load instance {instance_id}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let options = DuplicateOptions {
        content: true,
        saves,
        settings: true,
    };
    match instance.duplicate(options) {
        Ok(report) => {
            if json {
                print_json(serde_json::json!({
                    "source": instance_id,
                    "id": report.instance.id,
                    "name": report.instance.name,
                    "root": report.instance.root,
                    "mc_version": report.instance.mc_version,
                    "loader": report.instance.loader,
                    "files": report.files,
                    "bytes": report.bytes,
                    "skipped": report.skipped,
                }));
            } else {
                println!("{} ({})", report.summary(), report.instance.id);
                for skipped in report.skipped.iter().take(10) {
                    println!("  skipped {skipped}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[Kaelis] instance duplicate failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Unpack a portable archive into the live instances folder.
fn import_instance(archive: &str) -> ExitCode {
    let path = std::path::Path::new(archive);
    let probe = match kaelis_api::foreign::probe_archive(path) {
        Ok(probe) => probe,
        Err(error) => {
            eprintln!("[Kaelis] this is not a recognisable instance: {error}");
            return ExitCode::FAILURE;
        }
    };
    let instances = Instance::instances_dir();
    if probe.kind == kaelis_api::foreign::ForeignKind::Modrinth {
        eprintln!(
            "[Kaelis] this is a Modrinth pack: open Kaelis and drop it on Content, or use --import-modpack."
        );
        return ExitCode::FAILURE;
    }
    let result = if probe.kind == kaelis_api::foreign::ForeignKind::Kaelis {
        kaelis_api::portable::import_instance(path, &instances)
            .map(|report| (report.summary(), report.instance.id))
            .map_err(|error| error.to_string())
    } else {
        kaelis_api::foreign::import_foreign_archive(path, &probe, &instances, "")
            .map(|report| (report.summary(), report.instance.id))
            .map_err(|error| error.to_string())
    };
    match result {
        Ok((summary, id)) => {
            println!("{summary} ({id})");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[Kaelis] instance import failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Print one JSON document on stdout. Machine consumers read only this stream;
/// warnings stay on stderr so a parse cannot be corrupted by diagnostics.
fn print_json(value: serde_json::Value) {
    match serde_json::to_string_pretty(&value) {
        Ok(text) => println!("{text}"),
        Err(error) => eprintln!("[Kaelis] could not encode JSON: {error}"),
    }
}

fn instance_mod_count(instance: &Instance) -> usize {
    std::fs::read_dir(instance.root.join("mods"))
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("jar"))
                })
                .count()
        })
        .unwrap_or(0)
}

fn short_uuid(uuid: &str) -> String {
    if uuid.len() >= 8 {
        format!("{}…", &uuid[..8])
    } else if uuid.is_empty() {
        "no profile id yet".to_owned()
    } else {
        uuid.to_owned()
    }
}

/// Saved identities and the one the next launch prefers.
fn list_accounts(json: bool) -> ExitCode {
    let settings = match kaelis_api::accounts::try_load_settings() {
        Ok(settings) => settings,
        Err(error) => return fail(format!("could not read settings: {error}")),
    };
    let accounts = migrated_accounts(&settings);
    let active = active_account(&settings, &accounts);
    let is_active = |account: &kaelis_api::accounts::SavedAccount| {
        active.is_some_and(|active| {
            active.kind == account.kind && active.name.eq_ignore_ascii_case(&account.name)
        })
    };
    if json {
        let records = accounts
            .iter()
            .map(|account| {
                serde_json::json!({
                    "kind": account.kind.as_str(),
                    "name": account.name,
                    "uuid": account.uuid,
                    "added_unix": account.added_unix,
                    "active": is_active(account),
                })
            })
            .collect::<Vec<_>>();
        print_json(serde_json::json!({
            "count": records.len(),
            "active": active.map(|account| serde_json::json!({
                "kind": account.kind.as_str(),
                "name": account.name,
                "uuid": account.uuid,
            })),
            "accounts": records,
        }));
        return ExitCode::SUCCESS;
    }
    if accounts.is_empty() {
        println!("no saved accounts yet");
        return ExitCode::SUCCESS;
    }
    match active {
        Some(account) => println!(
            "{} saved account{} · active: {} ({})",
            accounts.len(),
            if accounts.len() == 1 { "" } else { "s" },
            account.name,
            account.kind.label().to_ascii_lowercase()
        ),
        None => println!(
            "{} saved account{} · none active",
            accounts.len(),
            if accounts.len() == 1 { "" } else { "s" }
        ),
    }
    for account in &accounts {
        println!(
            "  {} {:<18} {:<10} {}",
            if is_active(account) { "*" } else { " " },
            account.name,
            account.kind.label().to_ascii_lowercase(),
            short_uuid(&account.uuid)
        );
    }
    ExitCode::SUCCESS
}

/// Choose the identity the next launch uses. No credential is read or written.
fn use_account(name: &str, json: bool) -> ExitCode {
    let account = match switch_account(name) {
        Ok(account) => account,
        Err(error) => return fail(format!("could not switch account: {error}")),
    };
    if json {
        print_json(serde_json::json!({
            "active": {
                "kind": account.kind.as_str(),
                "name": account.name,
                "uuid": account.uuid,
            },
            "restart_required": true,
        }));
        return ExitCode::SUCCESS;
    }
    println!(
        "Active account: {} ({})",
        account.name,
        account.kind.label().to_ascii_lowercase()
    );
    println!("Takes effect on the next Kaelis launch.");
    ExitCode::SUCCESS
}

/// Report every saved instance without touching the network.
fn list_instances(json: bool) -> ExitCode {
    let instances = match Instance::load_all() {
        Ok(instances) => instances,
        Err(error) => return fail(format!("could not read instances: {error}")),
    };
    if json {
        let records = instances
            .iter()
            .map(|instance| {
                serde_json::json!({
                    "id": instance.id,
                    "name": instance.name,
                    "mc_version": instance.mc_version,
                    "loader": effective_loader(instance),
                    "root": instance.root.display().to_string(),
                    "mods": instance_mod_count(instance),
                    "ram_mb": instance.ram_mb,
                    "group": instance.group,
                    "last_played_unix": instance.last_played_unix,
                })
            })
            .collect::<Vec<_>>();
        print_json(serde_json::json!({
            "count": records.len(),
            "instances": records,
        }));
        return ExitCode::SUCCESS;
    }
    if instances.is_empty() {
        println!("no instances yet");
        return ExitCode::SUCCESS;
    }
    for instance in &instances {
        println!(
            "{}  {}  {}  {}  {} mods",
            instance.id,
            instance.name,
            instance.mc_version,
            effective_loader(instance),
            instance_mod_count(instance)
        );
    }
    ExitCode::SUCCESS
}

/// Report the local environment: paths, Java, and instance inventory size.
fn status(json: bool) -> ExitCode {
    let instances_dir = Instance::instances_dir();
    let data_dir = instances_dir
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| instances_dir.clone());
    let instances = Instance::load_all().unwrap_or_default();
    let java = kaelis_api::runtime::java_on_path()
        .or_else(kaelis_api::runtime::system_java)
        .map(|path| path.display().to_string());
    let settings = load_settings();
    let accounts = migrated_accounts(&settings);
    let active = active_account(&settings, &accounts);
    if json {
        print_json(serde_json::json!({
            "name": kaelis_core::brand::LAUNCHER_NAME,
            "version": env!("CARGO_PKG_VERSION"),
            "platform": kaelis_core::brand::build_platform(),
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "data_dir": data_dir.display().to_string(),
            "instances_dir": instances_dir.display().to_string(),
            "instances": instances.len(),
            "accounts": accounts.len(),
            "active_account": active.map(|account| serde_json::json!({
                "kind": account.kind.as_str(),
                "name": account.name,
                "uuid": account.uuid,
            })),
            "java": java,
            "website": kaelis_core::brand::WEBSITE,
        }));
        return ExitCode::SUCCESS;
    }
    println!(
        "{} {}",
        kaelis_core::brand::LAUNCHER_NAME,
        env!("CARGO_PKG_VERSION")
    );
    println!("platform: {}", kaelis_core::brand::build_platform());
    println!("data: {}", data_dir.display());
    println!("instances: {}", instances.len());
    println!(
        "accounts: {}{}",
        accounts.len(),
        active.map_or_else(String::new, |account| format!(
            " · active {} ({})",
            account.name,
            account.kind.label().to_ascii_lowercase()
        ))
    );
    println!("java: {}", java.as_deref().unwrap_or("not found yet"));
    ExitCode::SUCCESS
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|error| format!("could not start Tokio runtime: {error}"))
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(kaelis_core::brand::user_agent())
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(60))
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .redirect(https_redirects_only())
        .build()
        .map_err(|error| format!("could not create HTTP client: {error}"))
}

const MAX_REDIRECTS: usize = 10;

fn redirect_chain_is_allowed(next: &reqwest::Url, previous: &[reqwest::Url]) -> bool {
    previous.len() <= MAX_REDIRECTS
        && next.scheme() == "https"
        && previous.iter().all(|url| url.scheme() == "https")
}

fn https_redirects_only() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        if redirect_chain_is_allowed(attempt.url(), attempt.previous()) {
            attempt.follow()
        } else {
            attempt.error(std::io::Error::other(
                "refusing an unsafe or overlong redirect chain",
            ))
        }
    })
}

fn list_versions(show_alpha: bool, show_beta: bool, show_snapshots: bool, json: bool) -> ExitCode {
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(error) => return fail(error),
    };
    let client = match http_client() {
        Ok(client) => client,
        Err(error) => return fail(error),
    };
    let manifest = match runtime.block_on(kaelis_api::versions::fetch_manifest(&client)) {
        Ok(manifest) => manifest,
        Err(error) => return fail(format!("could not load Mojang version manifest: {error}")),
    };
    let versions =
        kaelis_api::versions::filter_versions(&manifest, show_alpha, show_beta, show_snapshots)
            .into_iter()
            .take(30)
            .collect::<Vec<_>>();
    if json {
        print_json(serde_json::json!({
            "latest": {
                "release": manifest.latest.release,
                "snapshot": manifest.latest.snapshot,
            },
            "count": versions.len(),
            "versions": versions
                .iter()
                .map(|version| serde_json::json!({
                    "id": version.id,
                    "type": version.kind,
                    "released": version.release_time,
                }))
                .collect::<Vec<_>>(),
        }));
        return ExitCode::SUCCESS;
    }
    for version in versions {
        println!("{}  [{}]", version.id, version.kind);
    }
    ExitCode::SUCCESS
}

/// Resolve the offline identity for a headless launch.
///
/// The GUI and CLI share one saved account list, so a player who picked an
/// offline profile in the window should not silently launch as `Player` from a
/// script. A Microsoft identity cannot be used here because its access token
/// is minted inside the GUI, and that is stated rather than guessed at.
fn headless_identity(requested: Option<&str>) -> Result<String, String> {
    if let Some(name) = requested {
        return Ok(name.to_owned());
    }
    let settings = load_settings();
    let accounts = migrated_accounts(&settings);
    match active_account(&settings, &accounts) {
        Some(account) if account.kind == AccountKind::Offline => Ok(account.name.clone()),
        Some(account) => Err(format!(
            "the active account is Microsoft ({}) and command-line launches are offline-only; pass --offline <username> to choose an identity",
            account.name
        )),
        None => Ok("Player".to_owned()),
    }
}

fn play_instance(instance_id: &str, username: Option<&str>) -> ExitCode {
    let username = match headless_identity(username) {
        Ok(username) => username,
        Err(error) => return fail(error),
    };
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(error) => return fail(error),
    };
    let instance = match Instance::load(instance_id) {
        Ok(instance) => instance,
        Err(error) => return fail(format!("could not load instance: {error}")),
    };
    let profile = offline_profile(&username);
    let identity = LaunchIdentity::offline(profile.name, profile.uuid);
    let log = EphemeralLog::new(20_000);
    match runtime.block_on(run_instance(instance, identity, log.clone(), || {})) {
        Ok(status) if status.success() => {
            log.clear();
            ExitCode::SUCCESS
        }
        Ok(status) => {
            log.clear();
            fail(format!("Minecraft exited with {status}"))
        }
        Err(error) => {
            log.clear();
            fail(format!("could not launch Minecraft: {error}"))
        }
    }
}

fn run_gui(handoff: Option<HandoffServer>) -> ExitCode {
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(error) => return fail(error),
    };
    let client = match http_client() {
        Ok(client) => client,
        Err(error) => return fail(error),
    };
    // The sweeper's first tick fires immediately, so it already performs the
    // startup sweep on the runtime instead of walking the temp folder on the
    // thread that is about to create the window.
    let sweeper = kaelis_core::sweeper::spawn_background_sweeper(runtime.handle(), 300, 3600);
    // A newer build retires the copies a portable folder or a manual download
    // left behind, so only one launcher remains next to the running one.
    kaelis_core::sweeper::retire_older_builds(env!("CARGO_PKG_VERSION"));
    let handle = runtime.handle().clone();
    // Capture runs may ask for a larger stage so the marketing stills stay
    // crisp; the shipping default still fits a 1366×768 laptop.
    let window_size = if std::env::var_os("KAELIS_SCREENSHOT").is_some() {
        match (
            std::env::var("KAELIS_SCREENSHOT_WIDTH")
                .ok()
                .and_then(|value| value.parse::<f32>().ok()),
            std::env::var("KAELIS_SCREENSHOT_HEIGHT")
                .ok()
                .and_then(|value| value.parse::<f32>().ok()),
        ) {
            (Some(width), Some(height)) => [width, height],
            _ => [1080.0, 700.0],
        }
    } else {
        [1080.0, 700.0]
    };
    // The app creator is consumed by a backend that fails before the window
    // exists, so the handoff server and sweeper wait in slots that only the
    // attempt that really starts the event loop takes from.
    let handoff = std::sync::Arc::new(std::sync::Mutex::new(handoff));
    let sweeper = std::sync::Arc::new(std::sync::Mutex::new(Some(sweeper)));
    // A remote-desktop or virtual-machine session can expose only OpenGL 1.1,
    // which the glow renderer refuses; Direct3D 12 (and its WARP software
    // adapter) still works there, so wgpu is the fallback rather than a crash.
    let requested = std::env::var("KAELIS_RENDERER").ok();
    let mut failures = Vec::new();
    for (renderer, label) in renderer_attempts(requested.as_deref()) {
        let options = native_options(renderer, window_size);
        let handoff = std::sync::Arc::clone(&handoff);
        let sweeper = std::sync::Arc::clone(&sweeper);
        let client = client.clone();
        let handle = handle.clone();
        match eframe::run_native(
            kaelis_core::brand::LAUNCHER_NAME,
            options,
            Box::new(move |creation| {
                let handoff = handoff.lock().ok().and_then(|mut slot| slot.take());
                let sweeper = match sweeper.lock().ok().and_then(|mut slot| slot.take()) {
                    Some(sweeper) => sweeper,
                    // Only a retry reaches this: keep the same app shape with
                    // a finished task instead of a second sweeper.
                    None => handle.spawn(async {}),
                };
                Ok(Box::new(kaelis_gui::KaelisApp::new(
                    creation, client, handle, sweeper, handoff,
                )))
            }),
        ) {
            Ok(()) => {
                // The window is gone, so nothing this process staged is still
                // in use: empty the staging root before the process exits.
                let _ = kaelis_core::sweeper::sweep_stale(0);
                return ExitCode::SUCCESS;
            }
            Err(error) => {
                // A terminal launch and the packaged build both deserve the
                // reason: the fallback hides it otherwise.
                eprintln!(
                    "[{}] {label} renderer unavailable: {error}",
                    kaelis_core::brand::LAUNCHER_NAME
                );
                failures.push(format!("{label}: {error}"));
            }
        }
    }
    report_startup_error(&failures.join("\n"));
    ExitCode::FAILURE
}

/// Which backends to try, in order. `KAELIS_RENDERER=glow|wgpu` pins one for
/// diagnostics; otherwise OpenGL runs first because it starts fastest, and wgpu
/// catches the drivers where a remote session only offers OpenGL 1.1.
fn renderer_attempts(requested: Option<&str>) -> Vec<(eframe::Renderer, &'static str)> {
    match requested
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("glow" | "opengl") => vec![(eframe::Renderer::Glow, "OpenGL")],
        Some("wgpu") => vec![(eframe::Renderer::Wgpu, "wgpu")],
        _ => vec![
            (eframe::Renderer::Glow, "OpenGL"),
            (eframe::Renderer::Wgpu, "wgpu"),
        ],
    }
}

fn native_options(renderer: eframe::Renderer, window_size: [f32; 2]) -> eframe::NativeOptions {
    eframe::NativeOptions {
        renderer,
        viewport: eframe::egui::ViewportBuilder::default()
            // Fits a 1366×768 laptop with the taskbar visible; eframe restores
            // the last size and position once the window has been moved.
            .with_inner_size(window_size)
            .with_min_inner_size([760.0, 520.0])
            .with_icon(kaelis_gui::window_icon())
            // Wayland/X11 use the app id to match the window to its desktop
            // entry, so the icon and name are right in the taskbar.
            .with_app_id("kaelis")
            .with_transparent(true),
        ..Default::default()
    }
}

/// A GUI build has no console, so a launch that cannot open a window has to say
/// why in a dialog; otherwise it looks like the app silently did nothing.
fn report_startup_error(details: &str) {
    eprintln!(
        "[{}] could not run Kaelis UI: {details}",
        kaelis_core::brand::LAUNCHER_NAME
    );
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
        let message = format!(
            "Kaelis could not open its window.\n\n{details}\n\n\
             Update the graphics driver, or run Kaelis outside a remote desktop session."
        );
        let title = wide(kaelis_core::brand::LAUNCHER_NAME);
        let text = wide(&message);
        // SAFETY: both buffers are NUL-terminated and live for the call.
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                text.as_ptr(),
                title.as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn fail(message: String) -> ExitCode {
    eprintln!("[{}] {message}", kaelis_core::brand::LAUNCHER_NAME);
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::{Action, MAX_REDIRECTS, parse_args, redirect_chain_is_allowed, renderer_attempts};
    use reqwest::Url;

    fn parse(arguments: &[&str]) -> Result<Action, String> {
        parse_args(arguments.iter().map(|value| (*value).to_owned()))
    }

    #[test]
    fn renderer_attempts_prefer_opengl_and_allow_pinning() {
        let fallback = renderer_attempts(None);
        assert_eq!(fallback.len(), 2);
        assert!(matches!(fallback[0].0, eframe::Renderer::Glow));
        assert!(matches!(fallback[1].0, eframe::Renderer::Wgpu));
        let pinned = renderer_attempts(Some(" WGPU "));
        assert_eq!(pinned.len(), 1);
        assert!(matches!(pinned[0].0, eframe::Renderer::Wgpu));
        assert!(matches!(
            renderer_attempts(Some("opengl"))[0].0,
            eframe::Renderer::Glow
        ));
        // An unknown pin keeps the default order instead of failing to start.
        assert_eq!(renderer_attempts(Some("software")).len(), 2);
    }

    #[test]
    fn json_marks_only_machine_readable_reports() {
        assert!(matches!(
            parse(&["--list-instances", "--json"]),
            Ok(Action::ListInstances { json: true })
        ));
        assert!(matches!(
            parse(&["--status", "--json"]),
            Ok(Action::Status { json: true })
        ));
        // `--json` alone has nothing to describe and must not open the GUI by
        // accident, which would look like a working command to a script.
        assert!(parse(&["--json"]).is_err());
    }

    #[test]
    fn reports_refuse_to_mix_with_launching() {
        assert!(parse(&["--status", "--play", "fabric-1-21"]).is_err());
        assert!(parse(&["--list-instances", "--status"]).is_err());
    }

    #[test]
    fn account_commands_parse_and_stay_standalone() {
        assert!(matches!(
            parse(&["--accounts", "--json"]),
            Ok(Action::Accounts { json: true })
        ));
        assert!(matches!(
            parse(&["--use-account", "Builder"]),
            Ok(Action::UseAccount { name, json: false }) if name == "Builder"
        ));
        assert!(parse(&["--accounts", "--use-account", "Builder"]).is_err());
        assert!(parse(&["--accounts", "--list-instances"]).is_err());
        assert!(parse(&["--use-account", "Builder", "--play", "x"]).is_err());
    }

    #[test]
    fn play_without_an_explicit_name_defers_to_the_saved_account() {
        assert!(matches!(
            parse(&["--play", "fabric-1-21"]),
            Ok(Action::Play { username: None, .. })
        ));
        assert!(matches!(
            parse(&["--play", "fabric-1-21", "--offline", "Builder"]),
            Ok(Action::Play { username: Some(name), .. }) if name == "Builder"
        ));
    }

    #[test]
    fn update_checks_are_standalone_and_accept_json() {
        assert!(matches!(
            parse(&["--check-updates", "sodium", "--json"]),
            Ok(Action::CheckUpdates { json: true, .. })
        ));
        assert!(matches!(
            parse(&["--update-mods", "sodium"]),
            Ok(Action::UpdateMods { json: false, .. })
        ));
        assert!(parse(&["--check-updates", "sodium", "--play", "x"]).is_err());
        assert!(parse(&["--check-updates", "x", "--update-mods", "y"]).is_err());
    }

    #[test]
    fn duplicate_is_standalone_and_worlds_is_opt_in() {
        assert!(matches!(
            parse(&["--duplicate", "fabric-1-21"]),
            Ok(Action::Duplicate {
                saves: false,
                json: false,
                ..
            })
        ));
        assert!(matches!(
            parse(&["--duplicate", "fabric-1-21", "--with-worlds", "--json"]),
            Ok(Action::Duplicate {
                saves: true,
                json: true,
                ..
            })
        ));
        assert!(parse(&["--with-worlds"]).is_err());
        assert!(parse(&["--duplicate", "a", "--play", "b"]).is_err());
        assert!(parse(&["--duplicate", "a", "--list-instances"]).is_err());
    }

    #[test]
    fn list_versions_accepts_the_json_contract() {
        assert!(matches!(
            parse(&["--list-versions", "--json"]),
            Ok(Action::ListVersions { json: true, .. })
        ));
    }

    #[test]
    fn allows_https_redirects_to_download_cdn_hosts() {
        let previous = [Url::parse("https://piston-meta.mojang.com/version.json").unwrap()];
        let next = Url::parse("https://cdn.example.net/client.jar").unwrap();
        assert!(redirect_chain_is_allowed(&next, &previous));
    }

    #[test]
    fn rejects_https_to_http_redirect_downgrade() {
        let previous = [Url::parse("https://login.microsoftonline.com/token").unwrap()];
        let next = Url::parse("http://attacker.example/token").unwrap();
        assert!(!redirect_chain_is_allowed(&next, &previous));
    }

    #[test]
    fn rejects_chains_that_started_without_https() {
        let previous = [Url::parse("http://metadata.example/manifest").unwrap()];
        let next = Url::parse("https://cdn.example.net/manifest").unwrap();
        assert!(!redirect_chain_is_allowed(&next, &previous));
    }

    #[test]
    fn caps_https_redirect_chains_at_ten_hops() {
        let next = Url::parse("https://cdn.example.net/file").unwrap();
        let at_limit = (0..MAX_REDIRECTS)
            .map(|index| Url::parse(&format!("https://cdn.example.net/{index}")).unwrap())
            .collect::<Vec<_>>();
        let over_limit = (0..=MAX_REDIRECTS)
            .map(|index| Url::parse(&format!("https://cdn.example.net/{index}")).unwrap())
            .collect::<Vec<_>>();
        assert!(redirect_chain_is_allowed(&next, &at_limit));
        assert!(!redirect_chain_is_allowed(&next, &over_limit));
    }
}
