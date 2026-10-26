# Refresh the product page's download sizes and hashes from real artifacts.
#
#   powershell -NoProfile -File tools/hydrate-site.ps1
#
# The page in git carries last-known values so a local preview is never blank.
# GitHub Pages cannot serve gitignored EXE/ZIP files, so the deploy workflow
# fetches the release assets into site/downloads first and then runs this. Every
# slot must exist; a renamed artifact or a missing file fails the deploy instead
# of publishing a stale hash next to a live download.

param(
    [string]$Page = "site/index.html",
    [string]$DownloadDir = "site/downloads"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root
$utf8 = [System.Text.UTF8Encoding]::new($false)
$pagePath = Join-Path $root $Page
$downloadPath = Join-Path $root $DownloadDir
$html = [System.IO.File]::ReadAllText($pagePath, $utf8)

function Format-Size([long]$bytes) {
    if ($bytes -ge 1GB) { return "{0:N1} GB" -f ($bytes / 1GB) }
    if ($bytes -ge 1MB) { return "{0:N1} MB" -f ($bytes / 1MB) }
    if ($bytes -ge 1KB) { return "{0:N0} KB" -f ($bytes / 1KB) }
    return "$bytes B"
}

# Windows is packaged by the local release script, so its three artifacts are
# required. The Linux and macOS artifacts are built only by the release
# workflow; the page keeps its last-known fallback when they are not fetched.
$artifacts = [ordered]@{
    "exe"      = "Kaelis.exe"
    "zip"      = "Kaelis-Portable.zip"
    "setup"    = "Kaelis-Setup.exe"
    "dmg"      = "Kaelis-macos-universal.dmg"
    "deb"      = "Kaelis-linux-x64.deb"
    "appimage" = "Kaelis-linux-x64.AppImage"
}
$required = @("exe", "zip", "setup")

foreach ($key in $artifacts.Keys) {
    $name = $artifacts[$key]
    $path = Join-Path $downloadPath $name
    if (-not (Test-Path $path)) {
        if ($required -contains $key) { throw "missing artifact: $path" }
        Write-Host "$name`: not fetched, keeping the page fallback"
        continue
    }
    $item = Get-Item $path
    $size = Format-Size $item.Length
    $hash = (Get-FileHash $item.FullName -Algorithm SHA256).Hash.ToLower()
    foreach ($field in @("size", "hash")) {
        $value = if ($field -eq "size") { $size } else { $hash }
        $pattern = "(data-release=`"$key-$field`">)[^<]*(<)"
        $matches = [regex]::Matches($html, $pattern)
        if ($matches.Count -ne 1) {
            throw "expected one $key-$field slot, found $($matches.Count)"
        }
        $html = [regex]::Replace($html, $pattern, "`${1}$value`${2}")
    }
    Write-Host "$name`: $size  $hash"
}

[System.IO.File]::WriteAllText($pagePath, $html, $utf8)
