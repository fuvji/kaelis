param([switch]$Force)
$ErrorActionPreference = 'Stop'

# Narration for the site film. One calm voice, short sentences, no hype.
# Lines are synthesized as SSML so the delivery is slightly slower and lower
# than the default, which reads as a product film rather than a notification.

Add-Type -AssemblyName System.Runtime.WindowsRuntime
$asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {
    $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and
    $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1'
})[0]
function Await($WinRtTask, $ResultType) {
    $asTaskGeneric = $asTask.MakeGenericMethod($ResultType)
    $netTask = $asTaskGeneric.Invoke($null, @($WinRtTask))
    $netTask.Wait(-1) | Out-Null
    $netTask.Result
}

[Windows.Media.SpeechSynthesis.SpeechSynthesizer, Windows.Media, ContentType = WindowsRuntime] | Out-Null
$synth = [Windows.Media.SpeechSynthesis.SpeechSynthesizer]::new()
$synth.Voice = $synth.AllVoices | Where-Object { $_.Language -like 'en*' } | Select-Object -First 1
Write-Host ("voice: " + $synth.Voice.DisplayName)

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$outDir = Join-Path $root 'media\tmp\audio'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null

$lines = @(
    'Every launcher asks you to manage files. Kaelis asks you to play.',
    'One workspace. Your instances, your content, your accounts.',
    'Mods, packs, shaders, and modpacks. Two catalogues, one search.',
    'Branch an instance, import an archive, launch. Nothing to configure.',
    'Every identity, one click apart.',
    'Kaelis. Minecraft, composed. Free on Windows, macOS, and Linux.'
)

for ($index = 0; $index -lt $lines.Count; $index++) {
    $path = Join-Path $outDir ("vo-{0}.wav" -f ($index + 1))
    if ((Test-Path $path) -and -not $Force) {
        Write-Host ("kept " + $path)
        continue
    }
    $ssml = @"
<speak version="1.0" xmlns="http://www.w3.org/2001/10/synthesis" xml:lang="en-US">
  <prosody rate="-8%" pitch="-2%">$($lines[$index])</prosody>
</speak>
"@
    $stream = Await ($synth.SynthesizeSsmlToStreamAsync($ssml)) ([Windows.Media.SpeechSynthesis.SpeechSynthesisStream])
    $size = [uint32]$stream.Size
    $reader = [Windows.Storage.Streams.DataReader]::new($stream)
    Await ($reader.LoadAsync($size)) ([uint32]) | Out-Null
    $bytes = New-Object byte[] $size
    $reader.ReadBytes($bytes)
    [System.IO.File]::WriteAllBytes($path, $bytes)
    Write-Host ("wrote " + $path + " (" + $size + " bytes)")
}
