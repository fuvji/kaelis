param(
    [string]$VaultPath = $env:KAELIS_VAULT,
    [string]$Subfolder = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$source = Join-Path $root "docs/brain"
if (-not (Test-Path -LiteralPath $source)) {
    throw "brain source not found: $source"
}
if ([string]::IsNullOrWhiteSpace($VaultPath)) {
    throw "Pass -VaultPath '<Obsidian vault folder>' or set KAELIS_VAULT."
}
if (-not (Test-Path -LiteralPath $VaultPath -PathType Container)) {
    throw "vault folder not found: $VaultPath"
}
$destination = if ([string]::IsNullOrWhiteSpace($Subfolder)) {
    $VaultPath
} else {
    Join-Path $VaultPath $Subfolder
}
New-Item -ItemType Directory -Path $destination -Force | Out-Null

$notes = Get-ChildItem -LiteralPath $source -Filter "*.md" -File
if ($notes.Count -eq 0) {
    throw "no brain notes found in $source"
}
foreach ($note in $notes) {
    Copy-Item -LiteralPath $note.FullName -Destination (Join-Path $destination $note.Name) -Force
}
Write-Host "Synced $($notes.Count) Kaelis brain notes to $destination"
$notes | ForEach-Object { Write-Host "  $($_.Name)" }
