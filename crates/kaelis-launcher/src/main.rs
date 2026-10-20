//! Kaelis entry point: claim the OS mutex before creating a runtime, parse a
//! small CLI surface, then run the desktop app or the requested operation.

use kaelis_api::auth::offline_profile;
use kaelis_api::launch::{LaunchIdentity, run_instance};
use kaelis_core::{
    ephemeral_log::EphemeralLog,
    handoff::{self, HandoffServer},
    instance::Instance,
    single_instance::{SingleInstance, SingleInstanceError},
};
use std::process::ExitCode;

#[derive(Debug)]
enum Action {
    Gui,
    Help,
    ListVersions {
        show_alpha: bool,
        show_beta: bool,
        show_snapshots: bool,
    },
    Offline(String),
    Play {
        instance_id: String,
        username: String,
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
}

fn main() -> ExitCode {
    let action = match parse_args(std::env::args().skip(1)) {
        Ok(action) => action,
        Err(error) => {
            eprintln!("[Kaelis] {error}\n\n{}", usage());
            return ExitCode::from(2);
        }
    };
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
        Action::Offline(name) => {
            let profile = offline_profile(&name);
            println!("offline profile: {} ({})", profile.name, profile.uuid);
            println!(
                "head: {}",
                kaelis_api::auth::crafatar_head_url(&profile.uuid, 64)
            );
            ExitCode::SUCCESS
        }
        Action::ListVersions {
            show_alpha,
            show_beta,
            show_snapshots,
        } => list_versions(show_alpha, show_beta, show_snapshots),
        Action::Play {
            instance_id,
            username,
        } => play_instance(&instance_id, &username),
        Action::InstallMod { instance_id, spec } => install_mod(&instance_id, &spec),
        Action::Repair { instance_id } => repair_instance(&instance_id),
        Action::ImportPack {
            instance_id,
            pack_path,
        } => import_pack(&instance_id, &pack_path),
        Action::Gui => run_gui(handoff),
    }
}

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
    let mut list_versions = false;
    let mut show_alpha = false;
    let mut show_beta = false;
    let mut show_snapshots = true;
    let mut offline_name = None;
    let mut instance_id = None;
    let mut repair_instance_id = None;
    let mut install_mod = None;
    let mut import_pack = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--help" | "-h" => help = true,
            "--list-versions" => list_versions = true,
            "--show-alpha" => show_alpha = true,
            "--show-beta" => show_beta = true,
            "--hide-snapshots" => show_snapshots = false,
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
            unknown => return Err(format!("unknown option: {unknown}")),
        }
    }
    if help {
        return Ok(Action::Help);
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
    if install_mod.is_some() && import_pack.is_some() {
        return Err("--install-mod and --import-modpack are separate operations".to_owned());
    }
    if list_versions {
        return Ok(Action::ListVersions {
            show_alpha,
            show_beta,
            show_snapshots,
        });
    }
    if let Some((instance_id, spec)) = install_mod {
        return Ok(Action::InstallMod { instance_id, spec });
    }
    if let Some(instance_id) = repair_instance_id {
        return Ok(Action::Repair { instance_id });
    }
    if let Some((instance_id, pack_path)) = import_pack {
        return Ok(Action::ImportPack {
            instance_id,
            pack_path,
        });
    }
    if let Some(instance_id) = instance_id {
        let username = offline_name.unwrap_or_else(|| "Player".to_owned());
        return Ok(Action::Play {
            instance_id,
            username,
        });
    }
    if let Some(name) = offline_name {
        return Ok(Action::Offline(name));
    }
    if show_alpha || show_beta || !show_snapshots {
        return Err("version filters require --list-versions".to_owned());
    }
    Ok(Action::Gui)
}

fn usage() -> &'static str {
    "Kaelis — Rust Minecraft Java launcher\n\
     Usage:\n\
       kaelis [--help]\n\
       kaelis --list-versions [--show-alpha] [--show-beta] [--hide-snapshots]\n\
       kaelis --offline <username>\n\
       kaelis --play <instance-id> [--offline <username>]\n\
       kaelis --install-mod <instance-id> <modrinth:project|curseforge:id|slug>\n\
       kaelis --repair <instance-id>\n\
       kaelis --import-modpack <instance-id> <pack.mrpack|pack.zip>\n\
     With no arguments, open the Kaelis desktop launcher.\n\
     --install-mod also installs every required dependency, and --repair installs\n\
     every required mod that the instance is missing."
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
    let curseforge_key =
        kaelis_api::secrets::load_secret(kaelis_api::secrets::CURSEFORGE_KEY_ACCOUNT)
            .ok()
            .flatten();
    let platforms = kaelis_api::mods::ModPlatformClient::with_curseforge_key(http, curseforge_key);
    let loader = effective_loader(&instance);
    let mut item = kaelis_api::mods::ModSearchItem {
        provider,
        id: project.to_owned(),
        slug: String::new(),
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
    let curseforge_key =
        kaelis_api::secrets::load_secret(kaelis_api::secrets::CURSEFORGE_KEY_ACCOUNT)
            .ok()
            .flatten();
    let platforms = kaelis_api::mods::ModPlatformClient::with_curseforge_key(http, curseforge_key);
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

fn list_versions(show_alpha: bool, show_beta: bool, show_snapshots: bool) -> ExitCode {
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
    for version in
        kaelis_api::versions::filter_versions(&manifest, show_alpha, show_beta, show_snapshots)
            .into_iter()
            .take(30)
    {
        println!("{}  [{}]", version.id, version.kind);
    }
    ExitCode::SUCCESS
}

fn play_instance(instance_id: &str, username: &str) -> ExitCode {
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(error) => return fail(error),
    };
    let instance = match Instance::load(instance_id) {
        Ok(instance) => instance,
        Err(error) => return fail(format!("could not load instance: {error}")),
    };
    let profile = offline_profile(username);
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
    let _ = kaelis_core::sweeper::sweep_stale(3600);
    let sweeper = kaelis_core::sweeper::spawn_background_sweeper(runtime.handle(), 300, 3600);
    let handle = runtime.handle().clone();
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            // Fits a 1366×768 laptop with the taskbar visible; eframe restores
            // the last size and position once the window has been moved.
            .with_inner_size([1080.0, 700.0])
            .with_min_inner_size([760.0, 520.0])
            .with_icon(kaelis_gui::window_icon())
            .with_transparent(true),
        ..Default::default()
    };
    match eframe::run_native(
        kaelis_core::brand::LAUNCHER_NAME,
        options,
        Box::new(move |creation| {
            Ok(Box::new(kaelis_gui::KaelisApp::new(
                creation, client, handle, sweeper, handoff,
            )))
        }),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(format!("could not run Kaelis UI: {error}")),
    }
}

fn fail(message: String) -> ExitCode {
    eprintln!("[{}] {message}", kaelis_core::brand::LAUNCHER_NAME);
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::{MAX_REDIRECTS, redirect_chain_is_allowed};
    use reqwest::Url;

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
