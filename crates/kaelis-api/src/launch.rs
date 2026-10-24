//! Prepare and run a Java edition client from an instance that already has
//! its Mojang version JSON, client jar, libraries, and assets installed.
//! Instance output is captured only in the bounded RAM log buffer.

use kaelis_core::{ephemeral_log::EphemeralLog, instance::Instance, jvm_flags};
use serde_json::Value;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;
use zeroize::Zeroizing;

static NATIVE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("version metadata is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("native archive is invalid: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("launch preparation worker failed: {0}")]
    Join(String),
    #[error("version metadata is missing at {0}")]
    MissingVersionMetadata(String),
    #[error("client jar is missing at {0}")]
    MissingClientJar(String),
    #[error("configured Java executable is missing at {0}")]
    MissingJavaExecutable(String),
    #[error(
        "no Java runtime could be started ({0}); install Java for this instance from its menu, or set a Java path in the instance editor"
    )]
    JavaNotFound(String),
    #[error("version metadata has no main class")]
    MissingMainClass,
    #[error("classpath is too long for this operating system")]
    ClasspathTooLong,
    #[error("invalid Minecraft version id: {0}")]
    InvalidVersion(String),
    #[error("native library path is unsafe: {0}")]
    UnsafePath(String),
    #[error("quick-join address is invalid: {0}")]
    InvalidQuickJoin(String),
    #[error("{0}")]
    InstanceBusy(#[from] kaelis_core::instance_guard::InstanceGuardError),
    #[error(
        "{0} is not provisioned for this instance yet; Kaelis installs Fabric and Quilt automatically, but {0} must be added before playing"
    )]
    LoaderNotProvisioned(String),
}

pub struct LaunchIdentity {
    pub name: String,
    pub uuid: String,
    pub access_token: Zeroizing<String>,
    pub account_type: AccountType,
}

#[derive(Debug, Clone, Copy)]
pub enum AccountType {
    Microsoft,
    Offline,
}

impl LaunchIdentity {
    pub fn offline(name: String, uuid: String) -> Self {
        Self {
            name,
            uuid,
            access_token: Zeroizing::new("0".to_owned()),
            account_type: AccountType::Offline,
        }
    }

    pub fn microsoft(name: String, uuid: String, access_token: String) -> Self {
        Self {
            name,
            uuid,
            access_token: Zeroizing::new(access_token),
            account_type: AccountType::Microsoft,
        }
    }
}

struct LaunchPlan {
    java: OsString,
    args: Vec<OsString>,
    env: Vec<(OsString, OsString)>,
    current_dir: PathBuf,
    native_dir: NativeTempDir,
    pre_launch: Option<String>,
    post_exit: Option<String>,
}

struct NativeTempDir {
    path: PathBuf,
    cleanup_on_drop: bool,
}

impl NativeTempDir {
    fn create(path: PathBuf) -> Result<Self, std::io::Error> {
        std::fs::create_dir(&path)?;
        Ok(Self {
            path,
            cleanup_on_drop: true,
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn retain_for_child(&mut self) {
        // A canceled launch future must not remove DLLs while the child can use them.
        self.cleanup_on_drop = false;
    }

    fn cleanup(&mut self) {
        self.cleanup_on_drop = false;
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

impl Drop for NativeTempDir {
    fn drop(&mut self) {
        if self.cleanup_on_drop {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn platform_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

fn platform_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "64"
    } else if cfg!(target_arch = "x86") {
        "32"
    } else if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "64"
    }
}

fn resolve_java_executable(instance: &Instance) -> Result<OsString, LaunchError> {
    match instance.java_path.as_deref() {
        Some(path) => crate::runtime::java_from_setting(path)
            .map(PathBuf::into_os_string)
            .ok_or_else(|| LaunchError::MissingJavaExecutable(path.display().to_string())),
        None => Ok(crate::runtime::managed_java(&instance.root)
            .map(PathBuf::into_os_string)
            .unwrap_or_else(|| OsString::from(crate::runtime::java_binary_name()))),
    }
}

fn matching_rule(rules: Option<&Value>, default: bool) -> bool {
    let Some(rules) = rules.and_then(Value::as_array) else {
        return default;
    };
    let mut allowed = false;
    for rule in rules {
        let os_matches = rule
            .pointer("/os/name")
            .and_then(Value::as_str)
            .is_none_or(|name| name == platform_name());
        let arch_matches = rule
            .pointer("/os/arch")
            .and_then(Value::as_str)
            .is_none_or(|arch| arch == std::env::consts::ARCH);
        let features_match = rule.get("features").is_none();
        if os_matches && arch_matches && features_match {
            allowed = rule
                .get("action")
                .and_then(Value::as_str)
                .is_some_and(|action| action == "allow");
        }
    }
    allowed
}

fn library_allowed(library: &Value) -> bool {
    matching_rule(library.get("rules"), true)
        && maven_native_classifier(library).is_none_or(maven_native_classifier_matches)
}

fn maven_native_classifier(library: &Value) -> Option<&str> {
    library
        .get("name")
        .and_then(Value::as_str)
        .and_then(|name| name.split(':').nth(3))
        .filter(|classifier| classifier.starts_with("natives-"))
}

fn maven_native_classifier_matches(classifier: &str) -> bool {
    let os_name = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let Some(suffix) = classifier.strip_prefix(&format!("natives-{os_name}")) else {
        return false;
    };
    if suffix.is_empty() {
        return cfg!(target_arch = "x86_64");
    }
    let Some(arch) = suffix.strip_prefix('-') else {
        return false;
    };
    match std::env::consts::ARCH {
        "x86_64" => matches!(arch, "64" | "x64" | "x86_64" | "amd64"),
        "x86" => matches!(arch, "32" | "x86" | "i386"),
        "aarch64" => matches!(arch, "arm64" | "aarch64"),
        _ => false,
    }
}

fn collect_jars(root: &Path, output: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    if !root.exists() {
        return Ok(());
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for item in std::fs::read_dir(directory)? {
            let item = item?;
            let file_type = item.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            let path = item.path();
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file()
                && path.extension().is_some_and(|extension| extension == "jar")
            {
                // Natives jars belong on the classpath: since LWJGL 3.3 the
                // native libraries are loaded either from `java.library.path`
                // or, as a fallback, from their jar on the classpath. Excluding
                // them produced `UnsatisfiedLinkError: Failed to locate
                // library: lwjgl.dll` on every modern version.
                output.push(path);
            }
        }
    }
    Ok(())
}

fn native_classifier(library: &Value) -> Option<String> {
    if !library_allowed(library) {
        return None;
    }
    if let Some(native_name) = library
        .pointer(&format!("/natives/{}", platform_name()))
        .and_then(Value::as_str)
    {
        let native_name = native_name.replace("${arch}", platform_arch());
        return library
            .pointer(&format!("/downloads/classifiers/{native_name}/path"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }
    maven_native_classifier(library)?;
    library
        .pointer("/downloads/artifact/path")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn safe_archive_path(value: &str) -> Option<PathBuf> {
    if value.contains('\\') || value.contains('\0') || value.contains(':') {
        return None;
    }
    let path = Path::new(value);
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return None;
    }
    Some(path.to_path_buf())
}

fn extract_natives(
    version: &Value,
    libraries_root: &Path,
    native_dir: &Path,
) -> Result<(), LaunchError> {
    for directory in ["jna", "lwjgl", "netty"] {
        std::fs::create_dir_all(native_dir.join(directory))?;
    }
    if let Some(libraries) = version.get("libraries").and_then(Value::as_array) {
        for library in libraries {
            let Some(relative) = native_classifier(library) else {
                continue;
            };
            let is_maven_native = maven_native_classifier(library).is_some();
            let Some(relative) = safe_archive_path(&relative) else {
                return Err(LaunchError::UnsafePath(relative));
            };
            let jar_path = libraries_root.join(relative);
            if !jar_path.is_file() {
                continue;
            }
            let mut archive = zip::ZipArchive::new(std::fs::File::open(jar_path)?)?;
            for index in 0..archive.len() {
                let mut entry = archive.by_index(index)?;
                if entry.is_dir() || entry.name().starts_with("META-INF/") {
                    continue;
                }
                if entry
                    .unix_mode()
                    .is_some_and(|mode| (mode & 0o170000) == 0o120000)
                {
                    continue;
                }
                let Some(relative) = safe_archive_path(entry.name()) else {
                    continue;
                };
                let destination = if is_maven_native {
                    let is_native_binary = relative.extension().is_some_and(|extension| {
                        matches!(
                            extension.to_string_lossy().to_ascii_lowercase().as_str(),
                            "dll" | "so" | "dylib" | "jnilib"
                        )
                    });
                    if !is_native_binary {
                        continue;
                    }
                    // Classifier natives (`natives-windows`, …) store the
                    // DLLs under `windows/x64/org/lwjgl/…` inside the jar.
                    // `java.library.path` only looks at the top level, so
                    // flatten the binaries into the natives directory.
                    native_dir.join(
                        relative
                            .file_name()
                            .ok_or_else(|| LaunchError::UnsafePath(entry.name().to_owned()))?,
                    )
                } else {
                    native_dir.join(relative)
                };
                if !destination.starts_with(native_dir) {
                    continue;
                }
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut file = std::fs::File::create(destination)?;
                kaelis_core::stream::copy_stream(&mut entry, &mut file)?;
            }
        }
    }
    Ok(())
}

fn argument_values(value: &Value) -> Vec<String> {
    if let Some(value) = value.as_str() {
        return vec![value.to_owned()];
    }
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    if !matching_rule(object.get("rules"), false) {
        return Vec::new();
    }
    match object.get("value") {
        Some(Value::String(value)) => vec![value.clone()],
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn expand(value: &str, placeholders: &[(String, String)]) -> String {
    placeholders
        .iter()
        .fold(value.to_owned(), |text, (key, value)| {
            text.replace(&format!("${{{key}}}"), value)
        })
}

fn prepare_launch_plan(
    instance: &Instance,
    identity: &LaunchIdentity,
) -> Result<LaunchPlan, LaunchError> {
    let valid_version = !instance.mc_version.is_empty()
        && instance.mc_version.len() <= 64
        && instance
            .mc_version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !valid_version {
        return Err(LaunchError::InvalidVersion(instance.mc_version.clone()));
    }
    let version_dir = instance.root.join("versions").join(&instance.mc_version);
    let version_json_path = version_dir.join(format!("{}.json", instance.mc_version));
    let version: Value = if version_json_path.is_file() {
        serde_json::from_slice(&std::fs::read(&version_json_path)?)?
    } else {
        return Err(LaunchError::MissingVersionMetadata(
            version_json_path.display().to_string(),
        ));
    };
    let client_jar = version_dir.join(format!("{}.jar", instance.mc_version));
    if !client_jar.is_file() {
        return Err(LaunchError::MissingClientJar(
            client_jar.display().to_string(),
        ));
    }
    // A provisioned Fabric-family loader (Fabric or Quilt) replaces the
    // vanilla main class and adds its own JVM arguments.
    let loader_install =
        crate::loader::load_for(&instance.root, &instance.mc_version, &instance.loader);
    // Launching vanilla under a modded label would look successful while every
    // mod silently stayed unloaded, so refuse it with an actionable message.
    let loader_name = instance.loader.trim().to_ascii_lowercase();
    if !loader_name.is_empty() && loader_name != "vanilla" && loader_install.is_none() {
        return Err(LaunchError::LoaderNotProvisioned(instance.loader.clone()));
    }
    let main_class = loader_install
        .as_ref()
        .map(|install| install.main_class.as_str())
        .or_else(|| version.get("mainClass").and_then(Value::as_str))
        .ok_or(LaunchError::MissingMainClass)?;
    let java = resolve_java_executable(instance)?;
    let assets_dir = instance.root.join("assets");
    let asset_index = version
        .pointer("/assetIndex/id")
        .and_then(Value::as_str)
        .unwrap_or(&instance.mc_version);
    let native_root = kaelis_core::sweeper::temp_root();
    std::fs::create_dir_all(&native_root)?;
    let sequence = NATIVE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let native_dir = NativeTempDir::create(
        native_root.join(format!("natives-{}-{sequence}", std::process::id())),
    )?;
    extract_natives(
        &version,
        &instance.root.join("libraries"),
        native_dir.path(),
    )?;

    let mut jars = Vec::new();
    collect_jars(&instance.root.join("libraries"), &mut jars)?;
    jars.push(client_jar);
    jars.sort();
    let classpath = std::env::join_paths(&jars).map_err(|_| LaunchError::ClasspathTooLong)?;
    let classpath_string = classpath.to_string_lossy().into_owned();
    let user_type = match identity.account_type {
        AccountType::Microsoft => "msa",
        AccountType::Offline => "legacy",
    };
    let auth_uuid = identity.uuid.replace('-', "");
    let version_type = version
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("release");
    let classpath_separator = if cfg!(windows) { ";" } else { ":" };
    let placeholders = [
        ("auth_player_name".to_owned(), identity.name.clone()),
        ("version_name".to_owned(), instance.mc_version.clone()),
        (
            "game_directory".to_owned(),
            instance.root.to_string_lossy().into_owned(),
        ),
        (
            "assets_root".to_owned(),
            assets_dir.to_string_lossy().into_owned(),
        ),
        ("assets_index_name".to_owned(), asset_index.to_owned()),
        ("auth_uuid".to_owned(), auth_uuid.clone()),
        (
            "auth_access_token".to_owned(),
            identity.access_token.as_str().to_owned(),
        ),
        ("user_type".to_owned(), user_type.to_owned()),
        ("version_type".to_owned(), version_type.to_owned()),
        (
            "natives_directory".to_owned(),
            native_dir.path().to_string_lossy().into_owned(),
        ),
        ("launcher_name".to_owned(), "Kaelis".to_owned()),
        (
            "launcher_version".to_owned(),
            env!("CARGO_PKG_VERSION").to_owned(),
        ),
        ("classpath".to_owned(), classpath_string.clone()),
        (
            "classpath_separator".to_owned(),
            classpath_separator.to_owned(),
        ),
        ("user_properties".to_owned(), "{}".to_owned()),
        ("clientid".to_owned(), String::new()),
        ("auth_xuid".to_owned(), String::new()),
    ];

    let mut args =
        jvm_flags::default_flags_with_min(instance.ram_mb, instance.min_ram_mb.unwrap_or(512))
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
    let jvm_arguments = version.pointer("/arguments/jvm").and_then(Value::as_array);
    if let Some(jvm_arguments) = jvm_arguments {
        args.extend(
            jvm_arguments
                .iter()
                .flat_map(argument_values)
                .map(|value| OsString::from(expand(&value, &placeholders))),
        );
    } else {
        args.push(OsString::from(format!(
            "-Djava.library.path={}",
            native_dir.path().display()
        )));
        args.push(OsString::from("-cp"));
        args.push(classpath);
    }
    if let Some(install) = &loader_install {
        args.extend(install.jvm_arguments.iter().cloned().map(OsString::from));
    }
    args.extend(instance.jvm_args.iter().cloned().map(OsString::from));
    args.push(OsString::from(main_class));

    if let Some(game_arguments) = version.pointer("/arguments/game").and_then(Value::as_array) {
        args.extend(
            game_arguments
                .iter()
                .flat_map(argument_values)
                .map(|value| OsString::from(expand(&value, &placeholders))),
        );
    } else if let Some(legacy_arguments) = version.get("minecraftArguments").and_then(Value::as_str)
    {
        args.extend(
            legacy_arguments
                .split_whitespace()
                .map(|value| OsString::from(expand(value, &placeholders))),
        );
    } else {
        args.extend([
            OsString::from("--username"),
            OsString::from(&identity.name),
            OsString::from("--version"),
            OsString::from(&instance.mc_version),
            OsString::from("--gameDir"),
            instance.root.as_os_str().to_owned(),
            OsString::from("--assetsDir"),
            assets_dir.into_os_string(),
            OsString::from("--assetIndex"),
            OsString::from(asset_index),
            OsString::from("--uuid"),
            OsString::from(&auth_uuid),
            OsString::from("--accessToken"),
            OsString::from(identity.access_token.as_str()),
            OsString::from("--userType"),
            OsString::from(user_type),
            OsString::from("--versionType"),
            OsString::from(version_type),
        ]);
    }
    if let Some(install) = &loader_install {
        args.extend(install.game_arguments.iter().cloned().map(OsString::from));
    }
    args.extend(
        window_and_join_args(instance)?
            .into_iter()
            .map(OsString::from),
    );
    args.extend(instance.game_args.iter().cloned().map(OsString::from));
    let hook = |value: &Option<String>| {
        value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    Ok(LaunchPlan {
        java,
        args,
        env: parse_environment(&instance.env),
        current_dir: instance.root.clone(),
        native_dir,
        pre_launch: hook(&instance.pre_launch_command),
        post_exit: hook(&instance.post_exit_command),
    })
}

/// Build the window and quick-join arguments from instance properties. Modern
/// clients (1.20+) use quick-play; older clients use the legacy server/port
/// pair. Values are rejected rather than passed through when they contain
/// characters that could split into extra JVM arguments.
fn window_and_join_args(instance: &Instance) -> Result<Vec<String>, LaunchError> {
    let mut args = Vec::new();
    if let (Some(width), Some(height)) = (instance.window_width, instance.window_height)
        && (320..=16_384).contains(&width)
        && (240..=16_384).contains(&height)
    {
        args.push("--width".to_owned());
        args.push(width.to_string());
        args.push("--height".to_owned());
        args.push(height.to_string());
    }
    if instance.fullscreen {
        args.push("--fullscreen".to_owned());
    }
    let Some(address) = instance
        .quick_join
        .as_deref()
        .map(str::trim)
        .filter(|address| !address.is_empty())
    else {
        return Ok(args);
    };
    if address.chars().any(|character| {
        character.is_whitespace() || character.is_control() || matches!(character, '/' | '\\')
    }) {
        return Err(LaunchError::InvalidQuickJoin(address.to_owned()));
    }
    let (host, port) = split_host_port(address)
        .ok_or_else(|| LaunchError::InvalidQuickJoin(address.to_owned()))?;
    if host.is_empty() {
        return Err(LaunchError::InvalidQuickJoin(address.to_owned()));
    }
    if modern_quick_play(&instance.mc_version) {
        args.push("--quickPlayMultiplayer".to_owned());
        args.push(if port.is_some() {
            address.to_owned()
        } else {
            format!("{host}:25565")
        });
    } else {
        args.push("--server".to_owned());
        args.push(host.to_owned());
        if let Some(port) = port {
            args.push("--port".to_owned());
            args.push(port.to_owned());
        }
    }
    Ok(args)
}

fn split_host_port(address: &str) -> Option<(&str, Option<&str>)> {
    if let Some(rest) = address.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        if host.is_empty() {
            return None;
        }
        if tail.is_empty() {
            return Some((address, None));
        }
        let port = tail.strip_prefix(':')?;
        return valid_port(port).then_some((address, Some(port)));
    }
    match address.rsplit_once(':') {
        Some((host, port)) if valid_port(port) => Some((host, Some(port))),
        Some(_) => None,
        None => Some((address, None)),
    }
}

fn valid_port(port: &str) -> bool {
    port.parse::<u16>().is_ok_and(|port| port != 0)
}

fn modern_quick_play(version: &str) -> bool {
    let mut parts = version
        .split('.')
        .filter_map(|part| part.parse::<u32>().ok());
    match (parts.next(), parts.next()) {
        (Some(1), Some(minor)) => minor >= 20,
        (Some(major), _) if major > 1 => true,
        _ => version
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_digit()),
    }
}

/// Run a Prism-style pre/post command through the platform shell. Failures and
/// output are written to the bounded in-memory log and never block a launch.
async fn run_hook(
    command: &str,
    cwd: &Path,
    env: &[(OsString, OsString)],
    log: &EphemeralLog,
    label: &str,
) {
    let max_lines = 40;
    let mut process = if cfg!(windows) {
        let mut process = tokio::process::Command::new("cmd");
        process.args(["/C", command]);
        process
    } else {
        let mut process = tokio::process::Command::new("sh");
        process.args(["-c", command]);
        process
    };
    process
        .current_dir(cwd)
        .envs(env.iter().cloned())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        process.as_std_mut().creation_flags(0x08000000);
    }
    let output =
        match tokio::time::timeout(std::time::Duration::from_secs(120), process.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                log.push(format!("[{label}] could not start command: {error}"));
                return;
            }
            Err(_) => {
                log.push(format!("[{label}] command timed out after 120 seconds"));
                return;
            }
        };
    for stream in [&output.stdout, &output.stderr] {
        for line in String::from_utf8_lossy(stream).lines().take(max_lines) {
            log.push(format!("[{label}] {line}"));
        }
    }
    if !output.status.success() {
        log.push(format!("[{label}] command exited with {}", output.status));
    }
}

/// Turn `KEY=VALUE` lines into process environment entries. Malformed or
/// empty names are ignored so old metadata cannot make a launch fail.
fn parse_environment(entries: &[String]) -> Vec<(OsString, OsString)> {
    entries
        .iter()
        .filter_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            let key = key.trim();
            if key.is_empty() || key.contains(['\0', '=']) || value.contains('\0') {
                return None;
            }
            Some((OsString::from(key), OsString::from(value.trim())))
        })
        .collect()
}

async fn capture<R>(mut reader: R, log: EphemeralLog, stream_name: &'static str)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    const CHUNK: usize = 4096;
    const MAX_LINE: usize = 16 * 1024;
    let mut bytes = [0_u8; CHUNK];
    let mut line = Vec::with_capacity(CHUNK);
    let mut truncated = false;
    loop {
        let count = match reader.read(&mut bytes).await {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        for byte in &bytes[..count] {
            if *byte == b'\n' {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                let suffix = if truncated {
                    "… [line truncated]"
                } else {
                    ""
                };
                log.push(format!(
                    "[{stream_name}] {}{suffix}",
                    String::from_utf8_lossy(&line)
                ));
                line.clear();
                truncated = false;
            } else if line.len() < MAX_LINE {
                line.push(*byte);
            } else {
                truncated = true;
            }
        }
    }
    if !line.is_empty() {
        log.push(format!(
            "[{stream_name}] {}{}",
            String::from_utf8_lossy(&line),
            if truncated {
                "… [line truncated]"
            } else {
                ""
            }
        ));
    }
}

/// Launch a preinstalled Java instance. `on_spawn` runs after the JVM process
/// is created, allowing the UI to freeze and move to its tray state; it receives
/// the new process id so the caller can report which game owns a window.
pub async fn run_instance<F>(
    instance: Instance,
    identity: LaunchIdentity,
    log: EphemeralLog,
    on_spawn: F,
) -> Result<std::process::ExitStatus, LaunchError>
where
    F: FnOnce(u32) + Send + 'static,
{
    // One instance, one JVM. Different instances are independent, so the lock
    // is per instance rather than per launcher.
    let _guard = kaelis_core::instance_guard::InstanceGuard::claim(&instance.id)?;
    let mut plan = tokio::task::spawn_blocking(move || prepare_launch_plan(&instance, &identity))
        .await
        .map_err(|error| LaunchError::Join(error.to_string()))??;

    if let Some(hook) = plan.pre_launch.as_deref() {
        run_hook(hook, &plan.current_dir, &plan.env, &log, "pre-launch").await;
    }

    let mut command = tokio::process::Command::new(&plan.java);
    command
        .args(&plan.args)
        .envs(plan.env.iter().cloned())
        .current_dir(&plan.current_dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.as_std_mut().creation_flags(0x08000000);
    }
    // A missing `java` on PATH surfaces here on machines without a system JDK.
    // Name the real problem instead of the raw "program not found" io error.
    let mut child = command.spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => {
            LaunchError::JavaNotFound(plan.java.to_string_lossy().into_owned())
        }
        _ => LaunchError::Io(error),
    })?;
    plan.native_dir.retain_for_child();
    // Hold the power registration for exactly the life of this JVM: when the
    // game exits (or the launch future is cancelled) the guard drops and the
    // monitor forgets the pid instead of polling a dead process forever.
    let pid = child.id().unwrap_or_default();
    let _power = (pid != 0).then(|| crate::power::track(pid));
    on_spawn(pid);

    let stdout_task = child
        .stdout
        .take()
        .map(|stdout| tokio::spawn(capture(stdout, log.clone(), "stdout")));
    let stderr_task = child
        .stderr
        .take()
        .map(|stderr| tokio::spawn(capture(stderr, log.clone(), "stderr")));
    let status = child.wait().await;
    if let Some(task) = stdout_task {
        let _ = task.await;
    }
    if let Some(task) = stderr_task {
        let _ = task.await;
    }
    let exit = status?;
    // A post-exit hook can take up to two minutes; run it beside the return so
    // the launcher window comes back the instant the game process is gone.
    if let Some(hook) = plan.post_exit.take() {
        let cwd = plan.current_dir.clone();
        let env = plan.env.clone();
        let hook_log = log.clone();
        tokio::spawn(async move {
            run_hook(&hook, &cwd, &env, &hook_log, "post-exit").await;
        });
    }
    plan.native_dir.cleanup();
    Ok(exit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    static TEST_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn test_instance() -> Instance {
        Instance::new(
            "java-test",
            "Java test",
            "1.21.1",
            "vanilla",
            PathBuf::from("instance"),
        )
    }

    #[test]
    fn environment_entries_are_parsed_and_malformed_lines_ignored() {
        let entries = vec![
            "KAELIS_TEST=1".to_owned(),
            "SPACED = value=with=equals".to_owned(),
            "NO_VALUE".to_owned(),
            "=missing".to_owned(),
            "BAD\0KEY=value".to_owned(),
        ];
        let parsed = parse_environment(&entries);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, std::ffi::OsString::from("KAELIS_TEST"));
        assert_eq!(parsed[0].1, std::ffi::OsString::from("1"));
        assert_eq!(parsed[1].0, std::ffi::OsString::from("SPACED"));
        assert_eq!(parsed[1].1, std::ffi::OsString::from("value=with=equals"));
    }

    #[test]
    fn window_and_quick_join_arguments_are_wired() {
        let mut instance = test_instance();
        instance.mc_version = "1.21.1".to_owned();
        instance.window_width = Some(1280);
        instance.window_height = Some(720);
        instance.fullscreen = true;
        instance.quick_join = Some("play.example.net".to_owned());
        let args = window_and_join_args(&instance).unwrap();
        assert!(args.windows(2).any(|pair| pair == ["--width", "1280"]));
        assert!(args.windows(2).any(|pair| pair == ["--height", "720"]));
        assert!(args.iter().any(|arg| arg == "--fullscreen"));
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--quickPlayMultiplayer", "play.example.net:25565"])
        );
        instance.mc_version = "1.19.4".to_owned();
        let args = window_and_join_args(&instance).unwrap();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--server", "play.example.net"])
        );
    }

    #[test]
    fn malformed_quick_join_is_rejected() {
        let mut instance = test_instance();
        instance.quick_join = Some("two words".to_owned());
        assert!(matches!(
            window_and_join_args(&instance),
            Err(LaunchError::InvalidQuickJoin(_))
        ));
        instance.quick_join = Some("host:99999".to_owned());
        assert!(matches!(
            window_and_join_args(&instance),
            Err(LaunchError::InvalidQuickJoin(_))
        ));
    }

    #[test]
    fn configured_java_executable_is_selected() {
        let path = std::env::temp_dir().join(format!(
            "kaelis-java-selection-{}-{}.exe",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        crate::runtime::write_java_stub(&path).unwrap();
        let mut instance = test_instance();
        instance.java_path = Some(path.clone());
        let result = resolve_java_executable(&instance);
        let _ = std::fs::remove_file(&path);
        assert_eq!(result.unwrap(), path.as_os_str());
    }

    #[test]
    fn configured_jdk_home_resolves_to_its_java_binary() {
        let home = std::env::temp_dir().join(format!(
            "kaelis-jdk-home-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let java = bin.join(crate::runtime::java_binary_name());
        crate::runtime::write_java_stub(&java).unwrap();
        let mut instance = test_instance();
        instance.java_path = Some(home.clone());

        let resolved = resolve_java_executable(&instance).unwrap();

        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(resolved, java.into_os_string());
    }

    #[test]
    fn an_unconfigured_instance_uses_its_managed_runtime_not_path_java() {
        let root = std::env::temp_dir().join(format!(
            "kaelis-managed-launch-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let bin = root
            .join("runtime")
            .join("java-runtime-delta")
            .join("21.0.5")
            .join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let java = bin.join(crate::runtime::java_binary_name());
        crate::runtime::write_java_stub(&java).unwrap();
        let mut instance = test_instance();
        instance.root = root.clone();

        let resolved = resolve_java_executable(&instance).unwrap();

        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(resolved, java.into_os_string());
    }

    #[test]
    fn missing_configured_java_executable_fails_clearly() {
        let mut instance = test_instance();
        instance.java_path = Some(std::env::temp_dir().join("kaelis-missing-java.exe"));
        assert!(matches!(
            resolve_java_executable(&instance),
            Err(LaunchError::MissingJavaExecutable(_))
        ));
    }

    #[test]
    fn unconfigured_instance_uses_path_java() {
        let instance = test_instance();
        let expected = if cfg!(windows) { "java.exe" } else { "java" };
        assert_eq!(resolve_java_executable(&instance).unwrap(), expected);
    }

    #[test]
    fn native_temp_directory_is_removed_when_launch_preparation_fails() {
        let path = std::env::temp_dir().join(format!(
            "kaelis-native-cleanup-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        {
            let directory = NativeTempDir::create(path.clone()).unwrap();
            std::fs::write(directory.path().join("partial.dll"), b"partial").unwrap();
        }
        assert!(!path.exists());
    }

    #[test]
    fn native_temp_directory_survives_task_cancellation_after_spawn() {
        let path = std::env::temp_dir().join(format!(
            "kaelis-native-retained-{}-{}",
            std::process::id(),
            TEST_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let mut directory = NativeTempDir::create(path.clone()).unwrap();
        directory.retain_for_child();
        drop(directory);
        assert!(path.is_dir());
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn extracts_explicit_maven_native_artifacts_for_the_current_platform() {
        let library = serde_json::json!({
            "name": "org.lwjgl:lwjgl:3.4.3:natives-windows",
            "rules": [{"action":"allow","os":{"name":"windows"}}],
            "downloads": {"artifact": {
                "path": "org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-windows.jar"
            }}
        });
        let expected = if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            Some("org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-windows.jar")
        } else {
            None
        };
        assert_eq!(native_classifier(&library).as_deref(), expected);
    }

    #[test]
    fn extracts_maven_native_binaries_into_the_java_library_path() {
        if !cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            return;
        }

        let sequence = TEST_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "kaelis-native-extraction-{}-{sequence}",
            std::process::id()
        ));
        let libraries_root = root.join("libraries");
        let native_dir = root.join("natives");
        let relative = PathBuf::from("org/lwjgl/lwjgl/3.4.3/lwjgl-3.4.3-natives-windows.jar");
        let jar_path = libraries_root.join(&relative);
        std::fs::create_dir_all(jar_path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(&jar_path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        archive
            .start_file(
                "windows/x64/org/lwjgl/lwjgl.dll",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        archive.write_all(b"native library fixture").unwrap();
        archive
            .start_file(
                "META-INF/windows/x64/org/lwjgl/lwjgl.dll.sha1",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        archive.write_all(b"metadata fixture").unwrap();
        archive.finish().unwrap();

        let version = serde_json::json!({
            "libraries": [{
                "name": "org.lwjgl:lwjgl:3.4.3:natives-windows",
                "rules": [{"action": "allow", "os": {"name": "windows"}}],
                "downloads": {"artifact": {"path": relative}}
            }]
        });
        extract_natives(&version, &libraries_root, &native_dir).unwrap();

        // Flattened into the directory passed as `-Djava.library.path`; LWJGL
        // loads `lwjgl.dll` from there, not from a nested `java/` folder.
        assert_eq!(
            std::fs::read(native_dir.join("lwjgl.dll")).unwrap(),
            b"native library fixture"
        );
        assert!(!native_dir.join("lwjgl.dll.sha1").exists());
        assert!(!native_dir.join("java/lwjgl.dll").exists());
        assert!(native_dir.join("jna").is_dir());
        assert!(native_dir.join("lwjgl").is_dir());
        assert!(native_dir.join("netty").is_dir());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_modded_instance_without_its_loader_is_refused() {
        let sequence = TEST_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "kaelis-unprovisioned-loader-{}-{sequence}",
            std::process::id()
        ));
        let version_dir = root.join("versions/1.21.1");
        std::fs::create_dir_all(&version_dir).unwrap();
        std::fs::write(
            version_dir.join("1.21.1.json"),
            br#"{"id":"1.21.1","mainClass":"net.minecraft.client.main.Main","libraries":[]}"#,
        )
        .unwrap();
        std::fs::write(version_dir.join("1.21.1.jar"), b"jar").unwrap();
        let instance = Instance::new(
            "forge-probe",
            "Forge probe",
            "1.21.1",
            "forge",
            root.clone(),
        );
        let identity = LaunchIdentity::offline("Tester".to_owned(), "uuid".to_owned());

        let result = prepare_launch_plan(&instance, &identity);

        let _ = std::fs::remove_dir_all(&root);
        assert!(
            matches!(result, Err(LaunchError::LoaderNotProvisioned(ref loader)) if loader == "forge"),
            "a modded launch must not silently fall back to vanilla"
        );
    }

    #[test]
    fn natives_jars_stay_on_the_classpath() {
        let sequence = TEST_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "kaelis-classpath-natives-{}-{sequence}",
            std::process::id()
        ));
        let jars = root.join("org/lwjgl/lwjgl/3.3.3");
        std::fs::create_dir_all(&jars).unwrap();
        for name in ["lwjgl-3.3.3.jar", "lwjgl-3.3.3-natives-windows.jar"] {
            std::fs::write(jars.join(name), b"jar").unwrap();
        }
        let mut collected = Vec::new();
        collect_jars(&root, &mut collected).unwrap();
        let _ = std::fs::remove_dir_all(&root);

        assert_eq!(
            collected.len(),
            2,
            "both the API and natives jar are needed"
        );
    }
}
