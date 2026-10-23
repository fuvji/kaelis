param([switch]$SkipStills)
$ErrorActionPreference = 'Stop'

# Rebuild the site intro film.
#
# Stills are painted once by render-film.ps1. The cut is deliberately still:
# each frame holds, then dissolves into the next. No push-in, no slide, no
# zoom — camera motion on interface screenshots reads as a demo, while a held
# frame with a slow dissolve reads as a product film. The score is a looped
# ambient pad under the film: no speech engine exists on this host, so the
# narration script ships as captions, and tools/narrate.ps1 can drop voice
# files in beside it later without touching this encoder.

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$stills = Join-Path $root 'media\tmp\film'
$media = Join-Path $root 'site\media'
$ffmpeg = 'C:\Users\Admin\AppData\Local\Riff\bin\ffmpeg.exe'
if (-not (Test-Path $ffmpeg)) {
    $ffmpeg = (Get-Command ffmpeg -ErrorAction SilentlyContinue).Source
}
if (-not $ffmpeg) { throw 'ffmpeg not found' }

if (-not $SkipStills) {
    & (Join-Path $PSScriptRoot 'render-film.ps1')
}

$fps = 60
# A long dissolve is the only transition, and it is deliberately slower than a
# template wipe: 0.9 s reads as a cut in a product film, 0.4 s reads as a
# slideshow. No frame ever moves inside its hold.
$cross = 0.9
# Editorial pacing: title, three product beats, the account beat, end card.
# Holds are long enough to read the screen without waiting.
$scenes = @(
    @{ seconds = 5.0; caption = 'A native Minecraft launcher.' },
    @{ seconds = 5.2; caption = 'Instances, content, and accounts in one workspace.' },
    @{ seconds = 5.2; caption = 'Two catalogues. One search.' },
    @{ seconds = 4.8; caption = 'Branch, import, launch.' },
    @{ seconds = 4.6; caption = 'Every identity, one click away.' },
    @{ seconds = 5.6; caption = 'Minecraft, composed. Windows, macOS, and Linux.' }
)
$count = $scenes.Count

$inputs = @()
$filters = @()
for ($i = 0; $i -lt $count; $i++) {
    # A still is a one-frame stream: without `-loop 1` every input ends after
    # its first frame and the xfade chain collapses to a few frames. Each input
    # is held for exactly its scene length, and the dissolves overlap them.
    $inputs += @(
        '-loop', '1', '-framerate', "$fps", '-t', "$($scenes[$i].seconds)",
        '-i', (Join-Path $stills ("scene{0}.png" -f ($i + 1)))
    )
    # One still, resampled once to the delivery raster. The frame never moves,
    # so straight UI edges stay pixel-clean for the whole hold.
    $filters += ("[{0}:v]scale=1920:1080:flags=lanczos,setsar=1," +
        "fps={1},format=yuv420p[v{0}]") -f $i, $fps
}

# Chain the scenes with dissolves only. No motion, no slide, no zoom.
$chain = ''
$previous = 'v0'
$bounds = @()
$start = 0.0
for ($i = 0; $i -lt $count; $i++) {
    $bounds += $start
    if ($i -lt $count - 1) {
        $offset = [Math]::Round($start + $scenes[$i].seconds - $cross, 3)
        $out = "x$($i + 1)"
        $chain += "[$previous][v$($i + 1)]xfade=transition=fade:duration=${cross}:offset=${offset}[$out];"
        $previous = $out
        $start = $offset
    }
}

$total = [Math]::Round($start + $scenes[$count - 1].seconds, 3)
$fadeOut = [Math]::Round($total - 0.7, 3)
$graph = ($filters -join ';') + ';' + $chain +
    "[$previous]fade=t=in:st=0:d=0.9,fade=t=out:st=${fadeOut}:d=0.9,format=yuv420p[v]"

# Score: the ambient pad, trimmed to the film, softened and faded.
$pad = Join-Path $root 'media\tmp\audio\pad.wav'
$audio = ''
$withAudio = Test-Path $pad
if ($withAudio) {
    $inputs += @('-stream_loop', '-1', '-i', $pad)
    $audio = "[${count}:a]atrim=0:${total},asetpts=N/SR/TB,highpass=f=70,lowpass=f=5200," +
        "highpass=f=75,lowpass=f=4800,volume=0.9,loudnorm=I=-18:TP=-1.5:LRA=8," +
        "afade=t=in:st=0:d=1.2,afade=t=out:st=${fadeOut}:d=0.8[a]"
    $graph = $graph + ';' + $audio
}

New-Item -ItemType Directory -Force -Path $media | Out-Null
$output = Join-Path $media 'kaelis-intro.mp4'

$encode = @('-hide_banner', '-loglevel', 'error', '-y') + $inputs +
    @('-filter_complex', $graph, '-map', '[v]')
if ($withAudio) { $encode += @('-map', '[a]', '-c:a', 'aac', '-b:a', '128k', '-ar', '48000') }
else { $encode += @('-an') }
$encode += @(
    '-r', $fps, '-c:v', 'libx264', '-preset', 'slow', '-crf', '20',
    '-profile:v', 'high', '-level', '4.2', '-pix_fmt', 'yuv420p',
    '-movflags', '+faststart', '-maxrate', '4000k', '-bufsize', '8000k', $output
)
& $ffmpeg @encode
if ($LASTEXITCODE -ne 0) { throw "ffmpeg failed ($LASTEXITCODE)" }

# Poster: the title beat, fully faded in. `-update 1` makes the single-image
# muxer write the frame instead of demanding a numbered pattern.
& $ffmpeg -hide_banner -loglevel error -y -ss 2.2 -i $output -frames:v 1 -update 1 `
    -q:v 3 (Join-Path $media 'kaelis-intro-poster.jpg')
if ($LASTEXITCODE -ne 0) { throw "poster failed ($LASTEXITCODE)" }

# A truncated encode still writes a playable header, so the build refuses to
# call the film done unless the whole timeline decodes back.
& $ffmpeg -hide_banner -loglevel error -i $output -map 0:v -f null NUL
if ($LASTEXITCODE -ne 0) { throw "film does not decode cleanly ($LASTEXITCODE)" }

# Captions are generated from the same timing table, so they cannot drift.
$stamp = {
    param([double]$seconds)
    $ts = [TimeSpan]::FromSeconds($seconds)
    '{0:00}:{1:00}:{2:00}.{3:000}' -f [int]$ts.TotalHours, $ts.Minutes, $ts.Seconds, $ts.Milliseconds
}
$vtt = "WEBVTT`r`n"
for ($i = 0; $i -lt $count; $i++) {
    $from = $bounds[$i] + 0.45
    $to = if ($i -lt $count - 1) { $bounds[$i + 1] - 0.1 } else { $total - 0.2 }
    $vtt += "`r`n$($i + 1)`r`n$(& $stamp $from) --> $(& $stamp $to)`r`n$($scenes[$i].caption)`r`n"
}
[System.IO.File]::WriteAllText((Join-Path $media 'kaelis-intro.vtt'), $vtt, (New-Object System.Text.UTF8Encoding($false)))

$size = [Math]::Round((Get-Item $output).Length / 1MB, 2)
Write-Host ("film: {0}s at {1} fps, {2} MB, score={3}" -f $total, $fps, $size, $withAudio)
