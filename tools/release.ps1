# Build, package, and hash the Windows release.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/release.ps1
#   ... -SkipBuild            # repackage an existing target/release binary
#   ... -Toolchain stable-x86_64-pc-windows-msvc
#
# Outputs:
#   dist/Kaelis.exe, site/downloads/Kaelis.exe          shipped binary
#   site/downloads/SHA256SUMS.txt                       checksum
#   dist/build.json                                     build provenance
#   dist/KAELIS-LINKS.txt                               generated URL index
#   site/downloads/KAELIS-LINKS.txt                     published copy
#   site/index.html                                     size/hash slots refreshed
#
# Windows ships as one executable. There is no installer, NSIS payload, or
# portable archive to build, sign, checksum, or upload.

param(
    [string]$Toolchain = "stable-x86_64-pc-windows-gnu",
    [switch]$SkipBuild
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root
$version = (Select-String -Path "Cargo.toml" -Pattern '^version = "(.*)"').Matches[0].Groups[1].Value

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

$exe = Get-Item (Join-Path $downloads "Kaelis.exe")
$exeHash = (Get-FileHash $exe.FullName -Algorithm SHA256).Hash.ToLower()

# SHA256SUMS.txt: `sha256  name`, compatible with `shasum -a 256 -c`. Written
# with LF endings: `sha256sum -c` rejects CRLF lines as malformed.
$lines = @(
    "# Kaelis $version - released $(Get-Date -Format yyyy-MM-dd)",
    "# Verify: shasum -a 256 -c SHA256SUMS.txt  |  certutil -hashfile Kaelis.exe SHA256",
    "$exeHash  Kaelis.exe"
)
$sumsPath = Join-Path $downloads "SHA256SUMS.txt"
[System.IO.File]::WriteAllText($sumsPath, ($lines -join "`n") + "`n", [System.Text.UTF8Encoding]::new($false))

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
# checksum. The deployed copy excludes every Windows package that no longer
# ships.
& (Join-Path $root "tools/links.ps1") -Quiet

# Remove packaging staging and local captures so the next build starts clean.
# Shipped artifacts (dist, site/downloads) are outside the prune list.
& (Join-Path $root "tools/prune.ps1")

Write-Host ""
Write-Host "Kaelis $version"
Write-Host "  dist/Kaelis.exe              $(Format-Size $exe.Length)  $exeHash"
Write-Host "  site/downloads/Kaelis.exe    $(Format-Size $exe.Length)  (copy)"
Write-Host "  SHA256SUMS.txt, dist/build.json, KAELIS-LINKS.txt written"
