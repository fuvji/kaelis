$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

# Paint the six stills for the site film. Every frame is a held composition:
# one idea, one product shot, generous black around it. The cut between frames
# is a dissolve, so nothing here moves — the stills carry the whole look.
#
# Design grid is 2400 x 1350 (16:9 at 2x), downsampled to 1920 x 1080 by the
# encoder. Type sizes are chosen for the 1920 delivery, so a 44 px subtitle
# here is 35 px on screen.

$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$outDir = Join-Path $root 'media\tmp\film'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
$W = 2400; $H = 1350

function Col([int]$r, [int]$g, [int]$b, [int]$a = 255) {
    [System.Drawing.Color]::FromArgb($a, $r, $g, $b)
}

function RoundPath([single]$x, [single]$y, [single]$w, [single]$h, [single]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = [Math]::Min($r * 2, [Math]::Min($w, $h))
    $p.AddArc($x, $y, $d, $d, 180, 90)
    $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure()
    return $p
}

function Glow($g, [single]$cx, [single]$cy, [single]$radius, $color) {
    # One soft radial wash, painted per pixel with a smooth falloff. A
    # PathGradientBrush is faster but quantises into visible rings on a black
    # stage; this stays clean at every size.
    $key = "$($color.ToArgb())|$($color.A)"
    if (-not $script:glowCache) { $script:glowCache = @{} }
    if (-not $script:glowCache.ContainsKey($key)) {
        $size = 256
        $bitmap = New-Object System.Drawing.Bitmap $size, $size
        $half = $size / 2.0
        for ($y = 0; $y -lt $size; $y++) {
            for ($x = 0; $x -lt $size; $x++) {
                $dx = ($x - $half + 0.5) / $half
                $dy = ($y - $half + 0.5) / $half
                $d = [Math]::Sqrt($dx * $dx + $dy * $dy)
                if ($d -ge 1) { continue }
                $falloff = [Math]::Pow(1 - $d, 2.4)
                $alpha = [int]($color.A * $falloff)
                if ($alpha -le 0) { continue }
                $bitmap.SetPixel($x, $y, [System.Drawing.Color]::FromArgb($alpha, $color.R, $color.G, $color.B))
            }
        }
        $script:glowCache[$key] = $bitmap
    }
    $g.DrawImage($script:glowCache[$key], $cx - $radius, $cy - $radius, $radius * 2, $radius * 2)
}

function Tracked($g, [string]$text, $font, $brush, [single]$x, [single]$y, [single]$tracking, [single]$width = 0) {
    # Letter-spaced capitals, the way a product page sets a small label.
    $solid = New-Object System.Drawing.SolidBrush $brush
    $total = 0.0
    foreach ($ch in $text.ToCharArray()) {
        $total += $g.MeasureString([string]$ch, $font).Width + $tracking
    }
    $cursor = if ($width -gt 0) { $x + ($width - $total) / 2 } else { $x }
    foreach ($ch in $text.ToCharArray()) {
        $s = [string]$ch
        $g.DrawString($s, $font, $solid, $cursor, $y)
        $cursor += $g.MeasureString($s, $font).Width + $tracking
    }
    $solid.Dispose()
}

function Get-NoiseTile {
    # A faint static dither. Without it a smooth dark gradient quantises into
    # wide rings on an 8-bit stage; with it the falloff reads as clean black.
    if ($script:noiseTile) { return $script:noiseTile }
    $size = 512
    $tile = New-Object System.Drawing.Bitmap $size, $size
    $random = New-Object System.Random 7
    for ($y = 0; $y -lt $size; $y++) {
        for ($x = 0; $x -lt $size; $x++) {
            $roll = $random.Next(0, 9)
            if ($roll -eq 0) {
                $tile.SetPixel($x, $y, [System.Drawing.Color]::FromArgb($random.Next(1, 3), 255, 255, 255))
            } elseif ($roll -eq 1) {
                $tile.SetPixel($x, $y, [System.Drawing.Color]::FromArgb($random.Next(1, 3), 0, 0, 0))
            }
        }
    }
    $script:noiseTile = $tile
    return $tile
}

function BaseCanvas {
    $bitmap = New-Object System.Drawing.Bitmap $W, $H
    $g = [System.Drawing.Graphics]::FromImage($bitmap)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::ClearTypeGridFit
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality

    # A near-black stage with the faintest vertical lift, so the black is deep
    # but not flat.
    $bounds = New-Object System.Drawing.Rectangle 0, 0, $W, $H
    $background = New-Object System.Drawing.Drawing2D.LinearGradientBrush $bounds, (Col 3 4 6), (Col 6 8 12), 90
    $g.FillRectangle($background, $bounds)
    $background.Dispose()

    # Light sits behind the product, not behind the type. It stays low so the
    # stage reads as black, not navy.
    Glow $g ($W / 2) 760 980 (Col 38 74 154 26)
    Glow $g ($W / 2 + 40) 1060 760 (Col 26 48 100 16)

    # Dither the stage before the type and product go down.
    $noise = Get-NoiseTile
    for ($y = 0; $y -lt $H; $y += $noise.Height) {
        for ($x = 0; $x -lt $W; $x += $noise.Width) {
            $g.DrawImageUnscaled($noise, $x, $y)
        }
    }

    # A one-pixel top edge catches light like a glass shelf.
    $edge = New-Object System.Drawing.Pen (Col 120 160 230 22), 1
    $g.DrawLine($edge, 0, 1, $W, 1)
    $edge.Dispose()
    return @($bitmap, $g)
}

function Mark() {
    # The master PNG is 1024 px; drawn at 2x the delivery size so it stays crisp
    # after the 1920 downsample.
    return [System.Drawing.Image]::FromFile((Join-Path $root 'assets\kaelis-mark.png'))
}

function Screenshot($g, [string]$name, [single]$x, [single]$y, [single]$w) {
    $img = [System.Drawing.Image]::FromFile((Join-Path $root "site\assets\$name"))
    # Fit the window inside its beat: never wider than the frame allows and
    # never taller than the space under the headline, so no edge is cropped.
    $ratio = $img.Width / $img.Height
    $slot = $w
    $maxHeight = 860
    if ($w / $ratio -gt $maxHeight) { $w = $maxHeight * $ratio }
    if ($w -lt $slot) { $x += ($slot - $w) / 2 }
    $h = $w / $ratio
    $radius = 22

    # Depth: a wide, low-opacity shadow instead of a hard drop. Two passes read
    # as ambient occlusion under a floating window.
    for ($pass = 0; $pass -lt 2; $pass++) {
        $alpha = if ($pass -eq 0) { 26 } else { 44 }
        $p = RoundPath ($x - 6) ($y + 18) ($w + 12) ($h + 12) ($radius + 6)
        $b = New-Object System.Drawing.SolidBrush (Col 0 0 0 $alpha)
        $g.FillPath($b, $p); $b.Dispose(); $p.Dispose()
    }

    # A hairline plate keeps the window edge legible on pure black.
    $plate = RoundPath ($x - 3) ($y - 3) ($w + 6) ($h + 6) ($radius + 3)
    $plateBrush = New-Object System.Drawing.SolidBrush (Col 150 180 235 34)
    $g.FillPath($plateBrush, $plate); $plateBrush.Dispose(); $plate.Dispose()

    $clip = RoundPath $x $y $w $h $radius
    $state = $g.Save()
    $g.SetClip($clip)
    $g.DrawImage($img, $x, $y, $w, $h)
    $g.Restore($state)

    # A soft top highlight along the window's top edge.
    $shine = New-Object System.Drawing.Pen (Col 255 255 255 26), 2
    $g.DrawLine($shine, $x + $radius, $y + 1, $x + $w - $radius, $y + 1)
    $shine.Dispose()

    $clip.Dispose(); $img.Dispose()
    return $h
}

function Save($bitmap, $g, [string]$name) {
    $g.Dispose()
    $bitmap.Save((Join-Path $outDir $name), [System.Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose()
    Write-Host "wrote $name"
}

function Centered($g, [string]$text, $font, $color, [single]$top, [single]$height) {
    $format = New-Object System.Drawing.StringFormat
    $format.Alignment = [System.Drawing.StringAlignment]::Center
    $format.LineAlignment = [System.Drawing.StringAlignment]::Near
    $brush = New-Object System.Drawing.SolidBrush $color
    $g.DrawString($text, $font, $brush, [System.Drawing.RectangleF]::new(0, $top, $W, $height), $format)
    $brush.Dispose(); $format.Dispose()
}

# 1 — title. One mark, one word, one line of context.
$pair = BaseCanvas; $bitmap = $pair[0]; $g = $pair[1]
$mark = Mark
$g.DrawImage($mark, ($W - 250) / 2, 300, 250, 250)
$mark.Dispose()

$f = New-Object System.Drawing.Font 'Segoe UI', 176, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
Centered $g 'Kaelis' $f (Col 244 246 250) 606 200
$f.Dispose()

$f = New-Object System.Drawing.Font 'Segoe UI', 52, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
Centered $g 'Minecraft, composed.' $f (Col 164 172 186) 830 70
$f.Dispose()

$f = New-Object System.Drawing.Font 'Segoe UI', 23, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
Tracked $g 'WINDOWS   /   MACOS   /   LINUX' $f (Col 128 140 158) 0 1010 6.0 $W
$f.Dispose()
Save $bitmap $g 'scene1.png'

# 2 — workspace.
$pair = BaseCanvas; $bitmap = $pair[0]; $g = $pair[1]
$f = New-Object System.Drawing.Font 'Segoe UI', 22, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
Tracked $g 'WORKSPACE' $f (Col 118 168 255) 0 150 7.0 $W
$f.Dispose()
$f = New-Object System.Drawing.Font 'Segoe UI', 78, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
Centered $g 'Everything in one window.' $f (Col 244 246 250) 196 110
$f.Dispose()
$f = New-Object System.Drawing.Font 'Segoe UI', 30, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
Centered $g 'Instances, content, and accounts, side by side.' $f (Col 160 168 182) 312 44
$f.Dispose()
Screenshot $g 'app-overview.png' (($W - 1780) / 2) 400 1780 | Out-Null
Save $bitmap $g 'scene2.png'

# 3 — content search.
$pair = BaseCanvas; $bitmap = $pair[0]; $g = $pair[1]
$f = New-Object System.Drawing.Font 'Segoe UI', 22, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
Tracked $g 'CONTENT' $f (Col 118 168 255) 0 150 7.0 $W
$f.Dispose()
$f = New-Object System.Drawing.Font 'Segoe UI', 78, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
Centered $g 'Two catalogues. One search.' $f (Col 244 246 250) 196 110
$f.Dispose()
$f = New-Object System.Drawing.Font 'Segoe UI', 30, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
Centered $g 'Mods, packs, shaders, and modpacks without leaving the app.' $f (Col 160 168 182) 312 44
$f.Dispose()
Screenshot $g 'app-content.png' (($W - 1780) / 2) 400 1780 | Out-Null
Save $bitmap $g 'scene3.png'

# 4 — instances.
$pair = BaseCanvas; $bitmap = $pair[0]; $g = $pair[1]
$f = New-Object System.Drawing.Font 'Segoe UI', 22, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
Tracked $g 'INSTANCES' $f (Col 118 168 255) 0 150 7.0 $W
$f.Dispose()
$f = New-Object System.Drawing.Font 'Segoe UI', 78, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
Centered $g 'Branch, import, launch.' $f (Col 244 246 250) 196 110
$f.Dispose()
$f = New-Object System.Drawing.Font 'Segoe UI', 30, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
Centered $g 'A fresh copy is one click. So is a whole pack.' $f (Col 160 168 182) 312 44
$f.Dispose()
Screenshot $g 'app-instances.png' (($W - 1780) / 2) 400 1780 | Out-Null
Save $bitmap $g 'scene4.png'

# 5 — accounts.
$pair = BaseCanvas; $bitmap = $pair[0]; $g = $pair[1]
$f = New-Object System.Drawing.Font 'Segoe UI', 22, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
Tracked $g 'ACCOUNTS' $f (Col 118 168 255) 0 150 7.0 $W
$f.Dispose()
$f = New-Object System.Drawing.Font 'Segoe UI', 78, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
Centered $g 'Every identity, one click away.' $f (Col 244 246 250) 196 110
$f.Dispose()
$f = New-Object System.Drawing.Font 'Segoe UI', 30, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
Centered $g 'Microsoft and offline profiles live together, with their own skins.' $f (Col 160 168 182) 312 44
$f.Dispose()
Screenshot $g 'app-accounts.png' (($W - 1780) / 2) 400 1780 | Out-Null
Save $bitmap $g 'scene5.png'

# 6 — end card.
$pair = BaseCanvas; $bitmap = $pair[0]; $g = $pair[1]
$mark = Mark
$g.DrawImage($mark, ($W - 210) / 2, 330, 210, 210)
$mark.Dispose()
$f = New-Object System.Drawing.Font 'Segoe UI', 132, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
Centered $g 'Kaelis' $f (Col 244 246 250) 566 150
$f.Dispose()
$f = New-Object System.Drawing.Font 'Segoe UI', 46, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
Centered $g 'Minecraft, composed.' $f (Col 164 172 186) 738 60
$f.Dispose()

$f = New-Object System.Drawing.Font 'Segoe UI', 31, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
Centered $g 'Download for Windows, macOS, and Linux' $f (Col 118 168 255) 866 50
$f.Dispose()
$f = New-Object System.Drawing.Font 'Segoe UI', 25, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
Centered $g 'fuvji.github.io/kaelis' $f (Col 130 140 156) 936 44
$f.Dispose()
Save $bitmap $g 'scene6.png'
