# Reproducible Kaelis performance benchmark.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools/bench.ps1
#   ... -Runs 7 -Exe target\x86_64-pc-windows-gnu\release\kaelis.exe
#
# Measures, on this host only:
#   * CLI `--version` latency (process spawn included)
#   * time until the window appears, with an empty data root and with a
#     200-jar instance that forces a full metadata scan
#   * working set, private bytes, handles and idle CPU after settling
#
# It writes target/bench-report.json and prints a table. It does not compare
# against other launchers: rankings need the same machine, build, disk cache,
# and protocol, which this script cannot honestly invent.

param(
    [string]$Exe = "",
    [int]$Runs = 5,
    [string]$Out = ""
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
if (-not $Exe) { $Exe = Join-Path $root "site/downloads/Kaelis.exe" }
if (-not $Out) { $Out = Join-Path $root "target/bench-report.json" }
if (-not (Test-Path $Exe)) { throw "missing $Exe" }
$Exe = (Resolve-Path $Exe).Path
$scratch = Join-Path $root "target/bench"

function Median([double[]]$values) {
    $sorted = $values | Sort-Object
    return $sorted[[int][math]::Floor(($sorted.Count - 1) / 2)]
}

function Percentile95([double[]]$values) {
    $sorted = $values | Sort-Object
    return $sorted[[int][math]::Min($sorted.Count - 1, [math]::Ceiling(0.95 * $sorted.Count) - 1)]
}

function New-Fixture([string]$directory, [int]$modCount) {
    if (Test-Path $directory) { Remove-Item $directory -Recurse -Force }
    $instances = Join-Path $directory "instances"
    $instanceRoot = Join-Path $instances "probe"
    $mods = Join-Path $instanceRoot "mods"
    New-Item -ItemType Directory -Force -Path $mods | Out-Null
    for ($i = 0; $i -lt $modCount; $i++) {
        $jar = Join-Path $mods ("probe-mod-{0:D3}.jar" -f $i)
        $zip = [System.IO.Compression.ZipFile]::Open($jar, [System.IO.Compression.ZipArchiveMode]::Create)
        try {
            $entry = $zip.CreateEntry("fabric.mod.json", [System.IO.Compression.CompressionLevel]::Fastest)
            $writer = New-Object System.IO.StreamWriter($entry.Open())
            try {
                $writer.Write('{"schemaVersion":1,"id":"probe-mod-' + $i + '","name":"Probe ' + $i + '","version":"1.0.0"}')
            } finally { $writer.Dispose() }
        } finally { $zip.Dispose() }
    }
    $record = @{
        id = "probe"; name = "Benchmark $modCount"; mc_version = "1.21.1"
        loader = "fabric"; root = ($instanceRoot -replace "\\", "/"); ram_mb = 4096
    } | ConvertTo-Json -Compress
    [System.IO.File]::WriteAllText((Join-Path $instances "probe.json"), $record)
    return $directory
}

function Measure-Cli([string]$path) {
    $samples = @()
    for ($i = 0; $i -lt $Runs; $i++) {
        $watch = [System.Diagnostics.Stopwatch]::StartNew()
        & $path --version | Out-Null
        $watch.Stop()
        if ($LASTEXITCODE -ne 0) { throw "CLI run failed with $LASTEXITCODE" }
        $samples += $watch.Elapsed.TotalMilliseconds
    }
    return @{
        samples_ms = $samples
        min_ms = ($samples | Measure-Object -Minimum).Minimum
        median_ms = (Median $samples)
        p95_ms = (Percentile95 $samples)
    }
}

function Measure-Window([string]$path, [string]$data, [int]$modCount) {
    New-Fixture $data $modCount | Out-Null
    $env:KAELIS_DATA_DIR = $data
    $samples = @()
    $memory = @()
    foreach ($run in 1..$Runs) {
        $watch = [System.Diagnostics.Stopwatch]::StartNew()
        $process = Start-Process -FilePath $path -PassThru
        try {
            $condition = New-Object System.Windows.Automation.PropertyCondition(
                [System.Windows.Automation.AutomationElement]::ProcessIdProperty, $process.Id)
            $window = $null
            for ($attempt = 0; $attempt -lt 1500; $attempt++) {
                Start-Sleep -Milliseconds 10
                $window = [System.Windows.Automation.AutomationElement]::RootElement.FindFirst(
                    [System.Windows.Automation.TreeScope]::Descendants, $condition)
                if ($window) { break }
            }
            $watch.Stop()
            if (-not $window) { throw "window did not appear" }
            $samples += $watch.Elapsed.TotalMilliseconds

            # Settle past first paint and the background metadata scan, then
            # measure an idle window: the launcher is event-driven at rest.
            Start-Sleep -Seconds 5
            $process.Refresh()
            $cpuBefore = $process.TotalProcessorTime.TotalMilliseconds
            Start-Sleep -Seconds 10
            $process.Refresh()
            $cpuAfter = $process.TotalProcessorTime.TotalMilliseconds
            $memory += @{
                run = $run
                window_ms = $watch.Elapsed.TotalMilliseconds
                working_set_mb = $process.WorkingSet64 / 1MB
                private_mb = $process.PrivateMemorySize64 / 1MB
                handles = $process.HandleCount
                idle_cpu_ms_over_10s = $cpuAfter - $cpuBefore
                idle_cpu_percent = (($cpuAfter - $cpuBefore) / 10000) * 100
            }
        } finally {
            if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force }
        }
    }
    return @{
        window_samples_ms = $samples
        window_median_ms = (Median $samples)
        window_p95_ms = (Percentile95 $samples)
        runs = $memory
        median_working_set_mb = (Median ($memory | ForEach-Object { $_.working_set_mb }))
        median_private_mb = (Median ($memory | ForEach-Object { $_.private_mb }))
        median_handles = (Median ($memory | ForEach-Object { $_.handles }))
        median_idle_cpu_percent = (Median ($memory | ForEach-Object { $_.idle_cpu_percent }))
    }
}

New-Item -ItemType Directory -Force -Path $scratch | Out-Null
$binary = Get-Item $Exe
$hash = (Get-FileHash $Exe -Algorithm SHA256).Hash.ToLowerInvariant()
Write-Host "Benchmarking $($binary.FullName) ($($binary.Length) B, sha256 $hash), $Runs runs"

$cli = Measure-Cli $Exe
$fresh = Measure-Window $Exe (Join-Path $scratch "fresh") 0
$heavy = Measure-Window $Exe (Join-Path $scratch "heavy") 200

$report = @{
    generated_at = (Get-Date).ToUniversalTime().ToString("o")
    host = @{
        os = [System.Environment]::OSVersion.VersionString
        processors = [System.Environment]::ProcessorCount
        powershell = $PSVersionTable.PSVersion.ToString()
    }
    binary = @{
        path = $binary.FullName
        bytes = $binary.Length
        sha256 = $hash
        version = (& $Exe --version | Select-Object -First 1)
    }
    cli_version = $cli
    gui_empty = $fresh
    gui_200_mods = $heavy
}
$report | ConvertTo-Json -Depth 8 | Set-Content -Path $Out -Encoding utf8

Write-Host ""
Write-Host ("CLI --version   median {0:N0} ms  p95 {1:N0} ms" -f $cli.median_ms, $cli.p95_ms)
foreach ($case in @(
    @{ name = "GUI empty"; data = $fresh },
    @{ name = "GUI 200 mods"; data = $heavy }
)) {
    Write-Host ("{0,-15} window {1:N0} ms  WS {2:N1} MB  private {3:N1} MB  handles {4:N0}  idle CPU {5:N2}%" -f `
        $case.name, $case.data.window_median_ms, $case.data.median_working_set_mb, `
        $case.data.median_private_mb, $case.data.median_handles, $case.data.median_idle_cpu_percent)
}
Write-Host "JSON: $Out"
