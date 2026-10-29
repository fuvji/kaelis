# Generate the complete Kaelis URL, installation, and link index.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/links.ps1
#   ... -Verify        # also probe every static HTTP(S) link
#
# Outputs:
#   dist/KAELIS-LINKS.txt              local, machine-readable inventory
#   site/downloads/KAELIS-LINKS.txt    published copy (same bytes)
#
# The release script calls this after new artifacts are hashed, so the file
# always describes the current build. Running it directly is safe and
# idempotent. Legacy link files and stale duplicate binaries in dist are
# removed first; nothing outside dist/site/downloads is touched.

param(
    [switch]$Verify,
    [switch]$Quiet
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root
$utf8 = [System.Text.UTF8Encoding]::new($false)

function Write-Note([string]$message) {
    if (-not $Quiet) { Write-Host $message }
}

$dist = Join-Path $root "dist"
$downloads = Join-Path $root "site/downloads"
New-Item -ItemType Directory -Force -Path $dist, $downloads | Out-Null

# Remove files this generator used to write, plus abandoned duplicate builds.
# Only exact known names are removed; user files are never matched by pattern.
foreach ($name in @(
    "LINKS.txt", "links.txt", "URLS.txt", "urls.txt",
    "SOURCES.txt", "sources.txt", "KAELIS-URLS.txt", "KAELIS_LINKS.txt"
)) {
    $path = Join-Path $dist $name
    if (Test-Path $path) {
        Remove-Item $path -Force
        Write-Note "removed legacy link file: dist/$name"
    }
}
$canonical = Join-Path $dist "Kaelis.exe"
if (Test-Path $canonical) {
    $canonicalTime = (Get-Item $canonical).LastWriteTimeUtc
    foreach ($name in @("Kaelis.new.exe", "Kaelis.old.exe", "Kaelis.tmp.exe")) {
        $path = Join-Path $dist $name
        if ((Test-Path $path) -and ((Get-Item $path).LastWriteTimeUtc -le $canonicalTime)) {
            Remove-Item $path -Force
            Write-Note "removed stale duplicate: dist/$name"
        }
    }
}

$version = (Select-String -Path "Cargo.toml" -Pattern '^version = "(.*)"').Matches[0].Groups[1].Value
$build = $null
$buildPath = Join-Path $dist "build.json"
if (Test-Path $buildPath) {
    try { $build = Get-Content -Raw $buildPath | ConvertFrom-Json } catch { $build = $null }
}

function Get-Artifact([string]$path) {
    if (-not (Test-Path $path)) { return $null }
    $item = Get-Item $path
    [pscustomobject]@{
        Name   = $item.Name
        Bytes  = $item.Length
        Sha256 = (Get-FileHash $item.FullName -Algorithm SHA256).Hash.ToLower()
        Path   = $item.FullName
    }
}

function Format-Size([long]$bytes) {
    if ($bytes -ge 1GB) { return "{0:N1} GB" -f ($bytes / 1GB) }
    if ($bytes -ge 1MB) { return "{0:N1} MB" -f ($bytes / 1MB) }
    if ($bytes -ge 1KB) { return "{0:N0} KB" -f ($bytes / 1KB) }
    return "$bytes B"
}

$exe = Get-Artifact (Join-Path $downloads "Kaelis.exe")
$zip = Get-Artifact (Join-Path $downloads "Kaelis-Portable.zip")
$setup = Get-Artifact (Join-Path $downloads "Kaelis-Setup.exe")
$setupStale = $false
if ($setup -and $exe) {
    $setupStale = (Get-Item (Join-Path $downloads "Kaelis-Setup.exe")).LastWriteTimeUtc -lt (Get-Item (Join-Path $downloads "Kaelis.exe")).LastWriteTimeUtc
}

$sb = New-Object System.Text.StringBuilder
function Add([string]$text = "") { [void]$sb.Append($text).Append("`n") }
function Add-Head([string]$title) {
    Add ""
    Add ("=" * 78)
    Add $title
    Add ("=" * 78)
}
$curated = New-Object System.Collections.Generic.HashSet[string]
function Add-Link([string]$label, [string]$url, [string]$note = "") {
    Add "  $label"
    Add "    $url"
    if ($note) { Add "    $note" }
    [void]$curated.Add($url)
}

Add "KAELIS - URL, INSTALLATION, AND LINK INDEX"
Add "=========================================="
Add "Launcher      Kaelis $version"
Add "Platform      $($build.platform)"
Add "Packaged      $($build.packaged_at)"
Add "Generator     tools/links.ps1 (run again after any endpoint or release change)"
Add "Repository    https://github.com/fuvji/kaelis"
Add "Website       https://fuvji.github.io/kaelis/"
Add ""
Add "This file is generated, not hand-edited. Every URL the launcher uses, every"
Add "download and installation method, and every project/community link is"
Add "listed below. The release script regenerates both copies, prunes legacy"
Add "link files, and refreshes the hashes after the build."

Add-Head "1. OFFICIAL DOWNLOADS"
Add "  Project website"
Add "    https://fuvji.github.io/kaelis/"
[void]$curated.Add("https://fuvji.github.io/kaelis/")
Add "  Download section"
Add "    https://fuvji.github.io/kaelis/#downloads"
[void]$curated.Add("https://fuvji.github.io/kaelis/#downloads")
Add-Link "Windows portable EXE" "https://fuvji.github.io/kaelis/downloads/Kaelis.exe" "Single executable, no installer, no runtime prerequisite."
Add-Link "Windows portable ZIP" "https://fuvji.github.io/kaelis/downloads/Kaelis-Portable.zip" "EXE + LICENSE + portable README."
Add-Link "Windows installer" "https://fuvji.github.io/kaelis/downloads/Kaelis-Setup.exe" $(if ($setupStale) { "WARNING: this file bundles an older binary; rebuild with tools/release.ps1." } else { "Per-user install under %LOCALAPPDATA%\Programs\Kaelis; no admin rights." })
Add-Link "Checksums (SHA-256)" "https://fuvji.github.io/kaelis/downloads/SHA256SUMS.txt" "Verify every download with certutil or sha256sum."
Add-Link "Download README" "https://fuvji.github.io/kaelis/downloads/README.md" "Portable usage, scripting, and verification."
Add-Link "This index, published" "https://fuvji.github.io/kaelis/downloads/KAELIS-LINKS.txt" "Same generated content as dist/KAELIS-LINKS.txt."
Add-Link "macOS universal disk image" "https://github.com/fuvji/kaelis/releases/latest/download/Kaelis-macos-universal.dmg" "Drag Kaelis.app to Applications; ad-hoc signed."
Add-Link "macOS universal build" "https://github.com/fuvji/kaelis/releases/latest/download/Kaelis-macos-universal.zip" "Built and signed ad hoc by packaging/macos/bundle.sh."
Add-Link "Linux Debian package" "https://github.com/fuvji/kaelis/releases/latest/download/Kaelis-linux-x64.deb" "apt install ./Kaelis-linux-x64.deb; desktop entry and uninstall included."
Add-Link "Linux AppImage" "https://github.com/fuvji/kaelis/releases/latest/download/Kaelis-linux-x64.AppImage" "One executable, no root, runs from any folder."
Add-Link "Linux x64 build" "https://github.com/fuvji/kaelis/releases/latest/download/Kaelis-linux-x64.tar.gz" "Binary, desktop entry, icon, LICENSE, README."
Add-Link "Every release" "https://github.com/fuvji/kaelis/releases" "All tagged builds and assets."
Add-Link "Latest release" "https://github.com/fuvji/kaelis/releases/latest" "Newest tagged release."
Add-Link "Latest Windows EXE" "https://github.com/fuvji/kaelis/releases/latest/download/Kaelis.exe"
Add-Link "Latest Windows ZIP" "https://github.com/fuvji/kaelis/releases/latest/download/Kaelis-Portable.zip"
Add-Link "Latest Windows installer" "https://github.com/fuvji/kaelis/releases/latest/download/Kaelis-Setup.exe"

Add-Head "2. INSTALLATION METHODS"
Add ""
Add "  2.1 Windows portable (recommended for a current build)"
Add "    Download Kaelis-Portable.zip, extract it anywhere you can write, and"
Add "    run Kaelis.exe. There is no installer and no bundled runtime download"
Add "    for the launcher itself. To keep instances and settings beside the"
Add "    executable, set KAELIS_DATA_DIR to that folder before launching."
Add "    Data: %APPDATA%\kaelis\Kaelis\data (default)"
Add "          %APPDATA%\kaelis\Kaelis\data\instances"
Add ""
Add "  2.2 Windows installer"
Add "    Download Kaelis-Setup.exe and run it. It installs per user under"
Add "    %LOCALAPPDATA%\Programs\Kaelis, adds Start menu and desktop shortcuts,"
Add "    registers an uninstaller, and needs no administrator rights."
Add "    Uninstall: Settings > Apps > Kaelis, or Uninstall.exe in that folder."
Add "    The installer carries the same Kaelis.exe build as the download page."
Add ""
Add "  2.3 macOS"
Add "    Prebuilt: Kaelis-macos-universal.dmg from the latest release; open the"
Add "    image and drag Kaelis.app to /Applications. The ZIP holds the same"
Add "    bundle if you prefer to unpack it yourself."
Add "    From source: ./packaging/macos/bundle.sh [--universal]"
Add "                 ./packaging/macos/dmg.sh"
Add "    Output: dist/Kaelis.app, dist/Kaelis-macos-universal.dmg"
Add "    Data: ~/Library/Application Support/gg.kaelis.Kaelis/instances"
Add ""
Add "  2.4 Linux"
Add "    Prebuilt: Kaelis-linux-x64.deb (apt), .AppImage (portable), or"
Add "    Kaelis-linux-x64.tar.gz (plain binary)."
Add "    Debian / Ubuntu: sudo apt install ./Kaelis-linux-x64.deb"
Add "    AppImage:        chmod +x Kaelis-linux-x64.AppImage && ./Kaelis-linux-x64.AppImage"
Add "    From source: ./packaging/linux/install.sh"
Add "                 ./packaging/linux/package.sh --no-build"
Add "    Installs: ~/.local/bin/kaelis (install.sh)"
Add "              /usr/bin/kaelis (deb)"
Add "              ~/.local/share/applications/kaelis.desktop"
Add "              ~/.local/share/icons/hicolor/scalable/apps/kaelis.svg"
Add "    Data: ~/.local/share/kaelis/instances"
Add "    Build packages: libgtk-3-dev libayatana-appindicator3-dev"
Add "                    libxkbcommon-dev libwayland-dev libx11-dev"
Add ""
Add "  2.5 Build from source"
Add "    Requirements: Rust 1.85+ and a C/C++ linker for the target."
Add "    Windows: stable-x86_64-pc-windows-msvc + Visual Studio C++ workload"
Add "    macOS:   stable-aarch64-apple-darwin or stable-x86_64-apple-darwin"
Add "    Linux:   stable-x86_64-unknown-linux-gnu"
Add "    Commands:"
Add "      git clone https://github.com/fuvji/kaelis.git"
Add "      cd kaelis"
Add "      cargo build --release -p kaelis-launcher"
Add "      target/release/kaelis --version"
Add ""
Add "  2.6 Where data lives"
Add "    Windows  %APPDATA%\kaelis\Kaelis\data"
Add "    macOS    ~/Library/Application Support/gg.kaelis.Kaelis"
Add "    Linux    ~/.local/share/kaelis"
Add "    Override any platform with KAELIS_DATA_DIR=<folder>."
Add "    Instances live under <data>/instances/<instance-id>/ with mods,"
Add "    resourcepacks, shaderpacks, saves, config, logs, runtime, and versions."
Add "    Settings: <data>/settings.json. Microsoft refresh tokens: OS keychain"
Add "    under service 'kaelis'. Access tokens never touch disk."
Add ""
Add "  2.7 Command line"
Add "    kaelis                         Open the desktop launcher"
Add "    kaelis --version [--json]      Build identity and public URLs"
Add "    kaelis --status [--json]       Data root, instances, accounts, Java"
Add "    kaelis --list-instances [--json]"
Add "    kaelis --list-versions [--show-alpha] [--show-beta] [--hide-snapshots]"
Add "    kaelis --accounts [--json]"
Add "    kaelis --use-account <name> [--json]"
Add "    kaelis --offline <name> [--json]"
Add "    kaelis --play <instance> [--offline <name>]"
Add "    kaelis --install-mod <instance> <modrinth:project|curseforge:id|slug>"
Add "    kaelis --repair <instance>"
Add "    kaelis --check-updates <instance> [--json]"
Add "    kaelis --update-mods <instance> [--json]"
Add "    kaelis --import-modpack <instance> <pack.mrpack|pack.zip>"
Add "    kaelis --export-instance <instance> <out.kaelis.zip>"
Add "    kaelis --import-instance <archive.kaelis.zip>"
Add "    kaelis --duplicate <instance> [--with-worlds] [--json]"

Add-Head "3. RUNTIME SERVICE URLS"
Add "  These are the endpoints the launcher contacts. Dynamic URLs are shown as"
Add "  patterns; values in <angle-brackets> come from Mojang or provider metadata."
Add ""
Add "  3.1 Mojang / Minecraft Services"
Add-Link "Version manifest (GET)" "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json" "Every install begins here."
Add-Link "Version metadata (GET, per version)" "https://piston-meta.mojang.com/v1/packages/<sha1>/<version>.json" "URL supplied by each version_manifest entry."
Add-Link "Mojang libraries (GET)" "https://libraries.minecraft.net/<group>/<artifact>/<version>/<file>" "Fallback base when version metadata omits a library URL."
Add-Link "Asset objects (GET)" "https://resources.download.minecraft.net/<first-2-hex>/<hash>" "SHA-1-addressed game assets."
Add-Link "Java runtime catalogue (GET)" "https://piston-meta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json" "Selects the version-matched Mojang JDK."
Add-Link "Java runtime files (GET)" "https://piston-meta.mojang.com/<runtime-manifest-path>" "Manifest URL is supplied by the runtime catalogue."
Add-Link "Premium name lookup (GET)" "https://api.mojang.com/users/profiles/minecraft" "Append /<username>; resolves an offline name's premium UUID and skin."
Add-Link "Minecraft EULA" "https://www.minecraft.net/en-us/eula"
Add ""
Add "  3.2 Microsoft / Xbox Live authentication"
Add-Link "Device code (POST)" "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode" "Scope: XboxLive.signin offline_access openid profile."
Add-Link "Browser authorize (GET)" "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize" "Authorization-code + PKCE flow."
Add-Link "Token exchange (POST)" "https://login.microsoftonline.com/consumers/oauth2/v2.0/token" "Device-code polling and refresh."
Add-Link "Xbox Live user auth (POST)" "https://user.auth.xboxlive.com/user/authenticate"
Add-Link "XSTS authorize (POST)" "https://xsts.auth.xboxlive.com/xsts/authorize"
Add-Link "Minecraft login (POST)" "https://api.minecraftservices.com/authentication/login_with_xbox"
Add-Link "Minecraft profile (GET)" "https://api.minecraftservices.com/minecraft/profile" "Confirms the profile after sign-in."
Add-Link "Local OAuth callback" "http://localhost:<ephemeral-port>/" "Loopback only; the port is bound before the browser opens."
Add-Link "Xbox relying-party identifier" "http://auth.xboxlive.com" "Protocol value, not a network request."
Add-Link "Entra device-code documentation" "https://learn.microsoft.com/entra/identity-platform/v2-oauth2-device-code"
Add-Link "Entra app registrations" "https://portal.azure.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade" "Where the optional public client ID is created."
Add ""
Add "  3.3 Modrinth"
Add-Link "API v2 base" "https://api.modrinth.com/v2"
Add-Link "Search (GET)" "https://api.modrinth.com/v2/search" "Merged with CurseForge results."
Add-Link "Project (GET)" "https://api.modrinth.com/v2/project/<id-or-slug>"
Add-Link "Project versions (GET)" "https://api.modrinth.com/v2/project/<project-id>/version" "Filtered by game version and loader."
Add-Link "One version (GET)" "https://api.modrinth.com/v2/version/<version-id>"
Add-Link "Hash lookup (GET)" "https://api.modrinth.com/v2/version_file/<sha1>?algorithm=sha1"
Add-Link "Bulk update check (POST)" "https://api.modrinth.com/v2/version_files/update" "Only SHA-1 hashes, loader, and game version leave the machine."
Add-Link "Projects by id (GET)" "https://api.modrinth.com/v2/projects?ids=[<ids>]"
Add-Link "Project site" "https://modrinth.com/"
Add-Link "Mod link pattern" "https://modrinth.com/mod/<slug>"
Add-Link "Resource-pack link pattern" "https://modrinth.com/resourcepack/<slug>"
Add-Link "Shader link pattern" "https://modrinth.com/shader/<slug>"
Add-Link "Modpack link pattern" "https://modrinth.com/modpack/<slug>"
Add-Link "Download CDN pattern" "https://cdn.modrinth.com/data/<project>/versions/<version>/<file>"
Add-Link "Modrinth documentation" "https://docs.modrinth.com/"
Add ""
Add "  3.4 CurseForge"
Add-Link "API v1 base" "https://api.curseforge.com/v1" "Requires an API key in Settings > Integrations."
Add-Link "Search (GET)" "https://api.curseforge.com/v1/mods/search?gameId=432" "gameId 432 is Minecraft."
Add-Link "Files (GET)" "https://api.curseforge.com/v1/mods/<mod-id>/files"
Add-Link "Project site" "https://www.curseforge.com/minecraft"
Add-Link "Mod link pattern" "https://www.curseforge.com/minecraft/mc-mods/<slug>"
Add-Link "Resource-pack link pattern" "https://www.curseforge.com/minecraft/texture-packs/<slug>"
Add-Link "Shader link pattern" "https://www.curseforge.com/minecraft/shaders/<slug>"
Add-Link "Modpack link pattern" "https://www.curseforge.com/minecraft/modpacks/<slug>"
Add-Link "Download CDN pattern" "https://media.forgecdn.net/<path>" "The API supplies the full download URL."
Add-Link "CurseForge console" "https://console.curseforge.com/" "Create the API key used by the launcher."
Add-Link "CurseForge API documentation" "https://docs.curseforge.com/"
Add ""
Add "  3.5 Loaders"
Add-Link "Fabric meta base" "https://meta.fabricmc.net/v2"
Add-Link "Fabric loader versions (GET)" "https://meta.fabricmc.net/v2/versions/loader/<game-version>"
Add-Link "Fabric profile JSON (GET)" "https://meta.fabricmc.net/v2/versions/loader/<game-version>/<loader-version>/profile/json"
Add-Link "Quilt meta base" "https://meta.quiltmc.org/v3"
Add-Link "Quilt loader versions (GET)" "https://meta.quiltmc.org/v3/versions/loader/<game-version>"
Add-Link "Quilt profile JSON (GET)" "https://meta.quiltmc.org/v3/versions/loader/<game-version>/<loader-version>/profile/json"
Add-Link "Fabric wiki" "https://wiki.fabricmc.net/"
Add-Link "Quilt project" "https://quiltmc.org/"
Add ""
Add "  3.6 Avatar mirrors"
Add-Link "Crafatar (primary)" "https://crafatar.com/avatars/<uuid>?size=<size>&overlay"
Add-Link "mc-heads (fallback)" "https://mc-heads.net/avatar/<uuid>/<size>"
Add-Link "Minotar (fallback)" "https://minotar.net/helm/<uuid>/<size>.png"

Add-Head "4. CONTENT LINK PATTERNS THE UI ACCEPTS"
Add "  Paste one of these into the content link field, or drop a local file:"
Add "    https://modrinth.com/mod/<slug>"
Add "    https://modrinth.com/resourcepack/<slug>"
Add "    https://modrinth.com/shader/<slug>"
Add "    https://modrinth.com/modpack/<slug>"
Add "    https://www.curseforge.com/minecraft/mc-mods/<slug>"
Add "    https://www.curseforge.com/minecraft/texture-packs/<slug>"
Add "    https://www.curseforge.com/minecraft/shaders/<slug>"
Add "    https://www.curseforge.com/minecraft/modpacks/<slug>"
Add "    https://<any-https-host>/<path>.jar    direct mod jar"
Add "    https://<any-https-host>/<path>.zip    resource pack, shader pack, or pack"
Add "    .mrpack links are installed as a new instance, not into the current one."
Add "    Other hosts are fetched directly (no scraping); only HTTPS is accepted."

Add-Head "5. PROJECT, COMMUNITY, AND SECURITY"
Add-Link "Source repository" "https://github.com/fuvji/kaelis"
Add-Link "Clone URL" "https://github.com/fuvji/kaelis.git"
Add-Link "Issues" "https://github.com/fuvji/kaelis/issues"
Add-Link "Security advisory" "https://github.com/fuvji/kaelis/security/advisories/new"
Add-Link "Release workflow" "https://github.com/fuvji/kaelis/actions/workflows/release.yml"
Add-Link "CI workflow" "https://github.com/fuvji/kaelis/actions/workflows/ci.yml"
Add-Link "Command reference" "https://github.com/fuvji/kaelis#command-line"
Add-Link "Changelog" "https://github.com/fuvji/kaelis/blob/main/CHANGELOG.md"
Add-Link "Discord community" "https://discord.gg/fvrdYzrQjq"
Add-Link "Founder" "https://github.com/fuvji"
Add-Link "License (proprietary)" "https://github.com/fuvji/kaelis/blob/main/LICENSE"

Add-Head "6. RELEASE ARTIFACTS IN THIS CHECKOUT"
Add "  Paths are relative to the repository root. Hashes are SHA-256."
Add ""
if ($exe) {
    Add "  site/downloads/Kaelis.exe"
    Add "    $($exe.Bytes) bytes ($(Format-Size $exe.Bytes))"
    Add "    $($exe.Sha256)"
} else {
    Add "  site/downloads/Kaelis.exe is missing."
}
if ($zip) {
    Add ""
    Add "  site/downloads/Kaelis-Portable.zip"
    Add "    $($zip.Bytes) bytes ($(Format-Size $zip.Bytes))"
    Add "    $($zip.Sha256)"
}
if ($setup) {
    Add ""
    Add "  site/downloads/Kaelis-Setup.exe"
    Add "    $($setup.Bytes) bytes ($(Format-Size $setup.Bytes))"
    Add "    $($setup.Sha256)"
    if ($setupStale) { Add "    WARNING: older than Kaelis.exe; rebuild with tools/release.ps1." }
}
Add ""
Add "  dist/Kaelis.exe and dist/Kaelis-Setup.exe are local scratch copies;"
Add "  site/downloads is the published set. The portable ZIP contains the same"
Add "  Kaelis.exe plus LICENSE and the portable README."

Add-Head "7. VERIFY A DOWNLOAD"
Add "  Windows:"
Add "    certutil -hashfile Kaelis.exe SHA256"
Add "  macOS / Linux:"
Add "    shasum -a 256 -c SHA256SUMS.txt"
Add "  Compare against https://fuvji.github.io/kaelis/downloads/SHA256SUMS.txt."
Add "  A mismatched file is not a Kaelis release."

Add-Head "8. AUTOMATIC SOURCE AUDIT"
Add "  Every https:// constant found in the Rust crates and the installer is"
Add "  listed here when it is not already covered above. This catches a new"
Add "  endpoint that was added without updating this generator."
Add ""
$discovered = @{}
$scanFiles = @()
$scanFiles += Get-ChildItem (Join-Path $root "crates") -Recurse -Filter *.rs -File
$installer = Join-Path $root "installer/kaelis.nsi"
if (Test-Path $installer) { $scanFiles += Get-Item $installer }
foreach ($file in $scanFiles) {
    $lines = [System.IO.File]::ReadAllLines($file.FullName, $utf8)
    for ($index = 0; $index -lt $lines.Length; $index++) {
        $line = $lines[$index]
        if ($line -notmatch '\bconst\b' -and $line -notmatch '!define\s+APP_URL') { continue }
        foreach ($match in [regex]::Matches($line, 'https?://[^\s"''<>\)]+')) {
            $url = $match.Value.TrimEnd(',', ';', ':')
            if ($url -match 'example\.(com|net|org)|cdn\.example|attacker\.example|metadata\.example|localhost|127\.0\.0\.1|schema\.org|sitemaps\.org|google\.com/schemas|apple\.com/DTDs') { continue }
            if ($curated.Contains($url)) { continue }
            if (-not $discovered.ContainsKey($url)) {
                $relative = $file.FullName.Substring($root.Length).TrimStart('\', '/')
                $discovered[$url] = "$relative`:$($index + 1)"
            }
        }
    }
}
if ($discovered.Count -eq 0) {
    Add "  No extra source URLs found; the curated inventory is complete."
} else {
    foreach ($url in ($discovered.Keys | Sort-Object)) {
        Add "  $url"
        Add "    $($discovered[$url])"
    }
}

Add-Head "9. DISCLAIMER"
Add "  Kaelis is not affiliated with Mojang Studios, Microsoft, Modrinth, or"
Add "  CurseForge. Minecraft is a trademark of Mojang Studios. Provider URLs"
Add "  and catalogue availability are controlled by their operators."

$text = $sb.ToString()
if (-not $text.EndsWith("`n")) { $text += "`n" }
$distOut = Join-Path $dist "KAELIS-LINKS.txt"
$siteOut = Join-Path $downloads "KAELIS-LINKS.txt"
[System.IO.File]::WriteAllText($distOut, $text, $utf8)
[System.IO.File]::WriteAllText($siteOut, $text, $utf8)
Write-Note "wrote dist/KAELIS-LINKS.txt and site/downloads/KAELIS-LINKS.txt"

if ($Verify) {
    # Only static browse/download URLs are probed. POST-only API endpoints and
    # <pattern> URLs are not, because a probe would be meaningless or harmful.
    $probe = New-Object System.Collections.Generic.List[string]
    foreach ($url in $curated) {
        if ($url -match '<|>|\{|\[|localhost|auth\.xboxlive\.com') { continue }
        if ($url -match 'api\.(modrinth|curseforge|mojang|minecraftservices)\.com') { continue }
        if ($url -match 'piston-meta\.mojang\.com/v1/products|resources\.download\.minecraft\.net|libraries\.minecraft\.net|cdn\.modrinth\.com|media\.forgecdn\.net') { continue }
        # POST-only auth/update endpoints and bare API bases are not pages; a
        # GET probe would report 400/404 without saying anything about health.
        if ($url -match 'devicecode$|/token$|/authenticate$|/xsts/authorize$|login_with_xbox$|/version_files/update$|/v1$|/v2$|/v3$') { continue }
        [void]$probe.Add($url)
    }
    $curl = Get-Command curl.exe -ErrorAction SilentlyContinue
    $failures = 0
    foreach ($url in ($probe | Sort-Object -Unique)) {
        $status = 0
        if ($curl) {
            # curl follows the same redirects a browser does and does not depend
            # on PowerShell's TLS defaults. HEAD is used for artifacts so a
            # probe never downloads a release; HTML and API pages use a ranged
            # GET because some hosts reject HEAD.
            $common = @("-L", "-s", "-o", "NUL", "-w", "%{http_code}", "--max-time", "20", "-A", "Kaelis-link-check/$version")
            if ($url -match '\.(exe|zip|gz)$') {
                $raw = (& $curl.Source @common --head $url) -join ""
            } else {
                $raw = (& $curl.Source @common --range "0-0" $url) -join ""
            }
            [void][int]::TryParse($raw.Trim(), [ref]$status)
        } else {
            try {
                $response = Invoke-WebRequest -Uri $url -Method Head -MaximumRedirection 5 -TimeoutSec 15 -UseBasicParsing
                $status = [int]$response.StatusCode
            } catch {
                $status = 0
            }
        }
        if ($status -lt 200 -or $status -ge 400) {
            $failures++
            Write-Host "unreachable ($status): $url"
        }
    }
    if ($failures -gt 0) {
        throw "$failures link(s) did not answer with 2xx/3xx."
    }
    Write-Note "verified $($probe.Count) static links"
}
