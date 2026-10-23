# Build, package, and sign the Windows release artifacts.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/release.ps1
#   ... -SkipBuild            # repackage an existing target/release binary
#   ... -Toolchain stable-x86_64-pc-windows-msvc
#
# Outputs:
#   dist/Kaelis.exe, site/downloads/Kaelis.exe          shipped binary
#   site/downloads/Kaelis-Portable.zip                  exe + LICENSE + README
#   site/downloads/SHA256SUMS.txt                       hashes for every artifact
#   dist/build.json                                     build provenance
#   dist/KAELIS-LINKS.txt                               generated URL + install index
#   site/downloads/KAELIS-LINKS.txt                     published copy of the index
#   site/index.html                                     size/hash slots refreshed
#
# The script is the single source of truth for release numbers: the site never
# carries a hand-typed hash. It also rebuilds the Windows installer from the
# fresh download copy and regenerates the link index, so a release cannot ship
# a stale setup or a stale URL list.

param(
    [string]$Toolchain = "stable-x86_64-pc-windows-gnu",
    [switch]$SkipBuild,
    [switch]$SkipInstaller
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root
$version = (Select-String -Path "crates/kaelis-launcher/Cargo.toml" -Pattern '^version = "(.*)"').Matches[0].Groups[1].Value

# The GNU toolchain needs its bundled MinGW linker and gcc on PATH.
$mingw = Join-Path $root "tools/mingw64/bin"
if (Test-Path $mingw) {
    $env:PATH = "$mingw;$env:PATH"
}
$env:PATH = "$env:USERPROFILE\.cargo\bin;$env:PATH"

if (-not $SkipBuild) {
    Write-Host "Building release with $Toolchain..."
    & cargo "+$Toolchain" build --release -p kaelis-launcher
    if ($LASTEXITCODE -ne 0) { throw "cargo build failed with $LASTEXITCODE" }
}

$binary = Join-Path $root "target/release/kaelis.exe"
if (-not (Test-Path $binary)) { throw "missing $binary" }

$dist = Join-Path $root "dist"
$downloads = Join-Path $root "site/downloads"
New-Item -ItemType Directory -Force -Path $dist, $downloads | Out-Null

# `dist` is the local scratch copy; a running Kaelis locks it, and that must
# not stop a release. The site download below is the shipped artifact.
$distCopied = $true
try {
    Copy-Item $binary (Join-Path $dist "Kaelis.exe") -Force
} catch {
    $distCopied = $false
    Write-Warning "could not refresh dist/Kaelis.exe (a running copy holds it): $($_.Exception.Message)"
}
Copy-Item $binary (Join-Path $downloads "Kaelis.exe") -Force

# Portable ZIP: exe + license + portable readme, at the archive root.
$portable = Join-Path $root "target/portable"
if (Test-Path $portable) { Remove-Item $portable -Recurse -Force }
New-Item -ItemType Directory -Force -Path $portable | Out-Null
# Source the archive from the fresh download copy, not `dist`: a running
# launcher can lock the scratch copy, and packaging that older file would
# silently ship a stale binary inside a fresh ZIP.
Copy-Item (Join-Path $downloads "Kaelis.exe") (Join-Path $portable "Kaelis.exe")
Copy-Item (Join-Path $root "LICENSE") (Join-Path $portable "LICENSE")
Copy-Item (Join-Path $root "site/downloads/README.md") (Join-Path $portable "README.md")
$zipPath = Join-Path $downloads "Kaelis-Portable.zip"
if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
Compress-Archive -Path (Join-Path $portable "Kaelis.exe"), (Join-Path $portable "LICENSE"), (Join-Path $portable "README.md") `
    -DestinationPath $zipPath -CompressionLevel Optimal
# dist is the complete local set, so it carries the same ZIP and checksums the
# site publishes. A stale ZIP here is how a hand-uploaded release goes wrong.
Copy-Item $zipPath (Join-Path $dist "Kaelis-Portable.zip") -Force
Copy-Item (Join-Path $root "site/downloads/README.md") (Join-Path $dist "README-Portable.txt") -Force

$exe = Get-Item (Join-Path $downloads "Kaelis.exe")
$zip = Get-Item $zipPath
$exeHash = (Get-FileHash $exe.FullName -Algorithm SHA256).Hash.ToLower()
$zipHash = (Get-FileHash $zip.FullName -Algorithm SHA256).Hash.ToLower()

# Windows installer: stage the payload, then compile it with NSIS. The stage is
# the working directory, so every File path is relative and a path containing
# spaces cannot break the compiler invocation. The payload comes from
# site/downloads, never the locked dist copy.
$setupPath = Join-Path $downloads "Kaelis-Setup.exe"
if (-not $SkipInstaller) {
    $makensis = Join-Path $root "tools/nsis/nsis-3.11/makensis.exe"
    if (-not (Test-Path $makensis)) {
        $command = Get-Command makensis -ErrorAction SilentlyContinue
        if ($command) { $makensis = $command.Source }
    }
    if (-not $makensis) {
        throw "makensis was not found. Install NSIS (choco install nsis) or pass -SkipInstaller."
    }
    $stage = Join-Path $root "target/installer"
    if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
    New-Item -ItemType Directory -Force -Path $stage | Out-Null
    Copy-Item $exe.FullName (Join-Path $stage "Kaelis.exe")
    Copy-Item (Join-Path $root "LICENSE") (Join-Path $stage "LICENSE")
    Copy-Item (Join-Path $root "site/downloads/README.md") (Join-Path $stage "README.md")
    Copy-Item (Join-Path $root "assets/kaelis.ico") (Join-Path $stage "kaelis.ico")
    Copy-Item (Join-Path $root "installer/kaelis.nsi") (Join-Path $stage "kaelis.nsi")
    # The setup must carry the exact downloadable binary, not a locked or
    # half-copied file. Fail before compiling rather than shipping a mismatch.
    $stagedHash = (Get-FileHash (Join-Path $stage "Kaelis.exe") -Algorithm SHA256).Hash.ToLower()
    if ($stagedHash -ne $exeHash) {
        throw "installer payload mismatch: staged $stagedHash, download $exeHash"
    }
    $version4 = if ($version -match '^(\d+)\.(\d+)\.(\d+)') {
        "$($Matches[1]).$($Matches[2]).$($Matches[3]).0"
    } else {
        "0.0.0.0"
    }
    Push-Location $stage
    try {
        & $makensis "/DAPP_VERSION=$version" "/DAPP_VERSION4=$version4" `
            "/DKAELIS_EXE=Kaelis.exe" "/DKAELIS_LICENSE=LICENSE" `
            "/DKAELIS_README=README.md" "/DKAELIS_ICON=kaelis.ico" `
            "/DKAELIS_OUT=Kaelis-Setup.exe" "kaelis.nsi"
        if ($LASTEXITCODE -ne 0) { throw "makensis failed with $LASTEXITCODE" }
    } finally {
        Pop-Location
    }
    Copy-Item (Join-Path $stage "Kaelis-Setup.exe") $setupPath -Force
    Copy-Item $setupPath (Join-Path $dist "Kaelis-Setup.exe") -Force
}
$setup = Get-Item $setupPath -ErrorAction SilentlyContinue

# SHA256SUMS.txt: `sha256  name`, compatible with `shasum -a 256 -c`. Written
# with LF endings: `sha256sum -c` rejects CRLF lines as malformed.
$lines = @(
    "# Kaelis $version - released $(Get-Date -Format yyyy-MM-dd)",
    "# Verify: shasum -a 256 -c SHA256SUMS.txt  |  certutil -hashfile <file> SHA256",
    "$exeHash  Kaelis.exe",
    "$zipHash  Kaelis-Portable.zip"
)
if ($setup) {
    $setupHash = (Get-FileHash $setup.FullName -Algorithm SHA256).Hash.ToLower()
    $lines += "$setupHash  Kaelis-Setup.exe"
}
$sumsPath = Join-Path $downloads "SHA256SUMS.txt"
[System.IO.File]::WriteAllText($sumsPath, ($lines -join "`n") + "`n", [System.Text.UTF8Encoding]::new($false))
Copy-Item $sumsPath (Join-Path $dist "SHA256SUMS.txt") -Force


$rustc = (& rustc --version) -join ""
$provenance = [ordered]@{
    product      = "Kaelis"
    version      = $version
    # Packaging time; the cargo build above reports its own finish time.
    packaged_at  = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    rustc        = $rustc
    toolchain    = $Toolchain
    platform     = "windows-x86_64"
    dist_copy    = $distCopied
    artifacts    = @(
        [ordered]@{ name = "Kaelis.exe"; bytes = $exe.Length; sha256 = $exeHash }
        [ordered]@{ name = "Kaelis-Portable.zip"; bytes = $zip.Length; sha256 = $zipHash }
    ) + @(
        if ($setup) {
            $setupHash = (Get-FileHash $setup.FullName -Algorithm SHA256).Hash.ToLower()
            [ordered]@{ name = "Kaelis-Setup.exe"; bytes = $setup.Length; sha256 = $setupHash }
        }
    )
    source       = "https://github.com/fuvji/kaelis"
    checksums    = "https://fuvji.github.io/kaelis/downloads/SHA256SUMS.txt"
}
# Written without a BOM: `Set-Content -Encoding utf8` on PowerShell 5.1 prepends
# one, and strict JSON readers reject it.
[System.IO.File]::WriteAllText(
    (Join-Path $dist "build.json"),
    ($provenance | ConvertTo-Json -Depth 5),
    [System.Text.UTF8Encoding]::new($false)
)

function Format-Size([long]$bytes) {
    if ($bytes -ge 1GB) { return "{0:N1} GB" -f ($bytes / 1GB) }
    if ($bytes -ge 1MB) { return "{0:N1} MB" -f ($bytes / 1MB) }
    return "{0:N0} KB" -f ($bytes / 1KB)
}

# Refresh the site's release slots so a published page can never show a stale
# size or checksum. The hydrator is shared with the Pages deploy workflow.
$index = Join-Path $root "site/index.html"
if (Test-Path $index) {
    & (Join-Path $root "tools/hydrate-site.ps1")
}

# Regenerate the URL/installation index from the just-written provenance and
# checksums. It prunes legacy link files and stale duplicate binaries first.
& (Join-Path $root "tools/links.ps1") -Quiet

# Remove packaging staging and local captures so the next build starts clean.
# Shipped artifacts (dist, site/downloads) are outside the prune list.
& (Join-Path $root "tools/prune.ps1")

Write-Host ""
Write-Host "Kaelis $version"
Write-Host "  dist/Kaelis.exe              $(Format-Size $exe.Length)  $exeHash"
Write-Host "  site/downloads/Kaelis.exe    $(Format-Size $exe.Length)  (copy)"
Write-Host "  Kaelis-Portable.zip          $(Format-Size $zip.Length)  $zipHash"
if ($setup) {
    $setupHash = (Get-FileHash $setup.FullName -Algorithm SHA256).Hash.ToLower()
    Write-Host "  Kaelis-Setup.exe             $(Format-Size $setup.Length)  $setupHash"
}
Write-Host "  SHA256SUMS.txt, dist/build.json, KAELIS-LINKS.txt written"
