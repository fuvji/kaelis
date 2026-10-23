# Remove regenerable working output after a release.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/prune.ps1
#
# Nothing shipped lives here: dist and site/downloads are never touched, and
# neither are source assets, the film stills, or the screenshots the README
# links to. Only staging folders, local capture experiments, temporary import
# fixtures, and old logs are removed, so the next release starts clean without
# a manual hunt for leftovers.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
Set-Location $root

$targets = @(
    'target\portable',
    'target\installer',
    'target\shots',
    'target\filmcheck',
    'target\filmseq',
    'target\marketing',
    'target\import-fixture',
    'target\import-data',
    'target\tmp'
)

$freed = 0L
foreach ($relative in $targets) {
    $path = Join-Path $root $relative
    if (-not (Test-Path $path)) { continue }
    $bytes = (Get-ChildItem $path -Recurse -File -ErrorAction SilentlyContinue |
        Measure-Object Length -Sum).Sum
    if ($bytes) { $freed += [long]$bytes }
    Remove-Item $path -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "pruned $relative"
}

Get-ChildItem (Join-Path $root 'target') -Filter '*.log' -File -ErrorAction SilentlyContinue |
    ForEach-Object {
        $freed += $_.Length
        Remove-Item $_.FullName -Force -ErrorAction SilentlyContinue
    }

# A crashed capture can leave a half-written PNG next to a real one.
Get-ChildItem (Join-Path $root 'target') -Filter '*.tmp' -File -ErrorAction SilentlyContinue |
    Remove-Item -Force -ErrorAction SilentlyContinue

Write-Host ("pruned {0:N1} MiB of working output" -f ($freed / 1MB))
