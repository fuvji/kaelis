# Regenerate the static brand assets from the same geometry as the in-app mark
# (crates/kaelis-gui/src/mark.rs). Run from the repository root:
#   powershell -NoProfile -File tools/make-brand-assets.ps1
#
# Outputs: assets/kaelis-mark.png (1024 px master), assets/kaelis.ico (16-256 px),
#          site/assets/kaelis-mark.png (512 px), the 192/512 PWA icons, a 180 px
#          touch icon, and the 1200x630 social card.
#
# The mark is a folded launch dart: a right-pointing arrowhead with a V notch
# at its back and a hairline seam between two planes. It is drawn on a fully
# transparent canvas in the product accent, with a lighter upper plane. Keep
# these points in sync with mark.rs and the site SVG.

Add-Type -AssemblyName System.Drawing

$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$accent = [System.Drawing.Color]::FromArgb(255, 61, 130, 246)
$planeLight = [System.Drawing.Color]::FromArgb(255, 112, 165, 255)
$planeDark = [System.Drawing.Color]::FromArgb(255, 46, 112, 232)

# Dart and fold in the 128 px design grid.
$tailTop = @(12.8, 17.92)
$nose = @(115.2, 64.0)
$tailBottom = @(12.8, 110.08)
$notchX = 38.4
# The seam opens at the back notch and tapers to the nose, so the point stays
# sharp and the 16 px silhouette stays solid.
$seam = 2.688

$facets = @(
    @($tailTop, $nose, @($notchX, (64.0 - $seam / 2))),
    @(@($notchX, (64.0 + $seam / 2)), $nose, $tailBottom)
)
$facetColors = @($planeLight, $planeDark)

function New-MarkBitmap([int]$size) {
    $bitmap = New-Object System.Drawing.Bitmap $size, $size
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.Clear([System.Drawing.Color]::Transparent)
    $k = $size / 128.0

    for ($index = 0; $index -lt $facets.Count; $index++) {
        $facet = $facets[$index]
        $points = [System.Drawing.PointF[]]@(
            (New-Object System.Drawing.PointF ([single]($facet[0][0] * $k)), ([single]($facet[0][1] * $k))),
            (New-Object System.Drawing.PointF ([single]($facet[1][0] * $k)), ([single]($facet[1][1] * $k))),
            (New-Object System.Drawing.PointF ([single]($facet[2][0] * $k)), ([single]($facet[2][1] * $k)))
        )
        $brush = New-Object System.Drawing.SolidBrush $facetColors[$index]
        $graphics.FillPolygon($brush, $points)
        $brush.Dispose()
    }

    $graphics.Dispose()
    return $bitmap
}

function Save-MarkPng([string]$path, [int]$size) {
    $bitmap = New-MarkBitmap $size
    $bitmap.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose()
    Write-Host "wrote $path ($size px)"
}

# 32-bit BMP payload: the uncompressed form every Windows icon surface and
# installer reads. Alpha rides in the fourth byte and the AND mask marks fully
# transparent pixels, so the icon stays crisp on light and dark shells alike.
function Write-IconDib($bitmap, $stream) {
    $size = $bitmap.Width
    $rowBytes = $size * 4
    $maskBytes = [int]([math]::Ceiling($size / 32.0) * 4)
    $writer = New-Object System.IO.BinaryWriter ($stream, [System.Text.Encoding]::UTF8, $true)
    $writer.Write([UInt32]40)
    $writer.Write([Int32]$size)
    $writer.Write([Int32]($size * 2))
    $writer.Write([UInt16]1)
    $writer.Write([UInt16]32)
    $writer.Write([UInt32]0)
    $writer.Write([UInt32]($rowBytes * $size + $maskBytes * $size))
    $writer.Write([Int32]0)
    $writer.Write([Int32]0)
    $writer.Write([UInt32]0)
    $writer.Write([UInt32]0)
    for ($y = $size - 1; $y -ge 0; $y--) {
        for ($x = 0; $x -lt $size; $x++) {
            $pixel = $bitmap.GetPixel($x, $y)
            $writer.Write([byte]$pixel.B)
            $writer.Write([byte]$pixel.G)
            $writer.Write([byte]$pixel.R)
            $writer.Write([byte]$pixel.A)
        }
    }
    for ($y = $size - 1; $y -ge 0; $y--) {
        $mask = New-Object byte[] $maskBytes
        for ($x = 0; $x -lt $size; $x++) {
            if ($bitmap.GetPixel($x, $y).A -lt 128) {
                $mask[[int][math]::Floor($x / 8)] = $mask[[int][math]::Floor($x / 8)] -bor (0x80 -shr ($x % 8))
            }
        }
        $writer.Write($mask)
    }
    $writer.Flush()
}

# ICO container: BMP entries up to 128 px for universal support, PNG for 256 px.
function Save-MarkIco([string]$path, [int[]]$sizes) {
    $payloads = @()
    foreach ($size in $sizes) {
        $bitmap = New-MarkBitmap $size
        $stream = New-Object System.IO.MemoryStream
        if ($size -gt 64) {
            # 128 and 256 are PNG to keep the file small; every size the shell
            # and installers actually request stays BMP for older tools.
            $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
        } else {
            Write-IconDib $bitmap $stream
        }
        $payloads += , $stream.ToArray()
        $stream.Dispose()
        $bitmap.Dispose()
    }
    $file = [System.IO.File]::Create($path)
    $writer = New-Object System.IO.BinaryWriter $file
    $writer.Write([UInt16]0)
    $writer.Write([UInt16]1)
    $writer.Write([UInt16]$sizes.Count)
    $offset = 6 + 16 * $sizes.Count
    for ($index = 0; $index -lt $sizes.Count; $index++) {
        $size = $sizes[$index]
        $writer.Write([byte]$(if ($size -ge 256) { 0 } else { $size }))
        $writer.Write([byte]$(if ($size -ge 256) { 0 } else { $size }))
        $writer.Write([byte]0)
        $writer.Write([byte]0)
        $writer.Write([UInt16]1)
        $writer.Write([UInt16]32)
        $writer.Write([UInt32]$payloads[$index].Length)
        $writer.Write([UInt32]$offset)
        $offset += $payloads[$index].Length
    }
    foreach ($payload in $payloads) {
        $writer.Write($payload)
    }
    $writer.Dispose()
    $file.Dispose()
    Write-Host "wrote $path ($($sizes -join ', ') px)"
}

Save-MarkPng (Join-Path $root "assets/kaelis-mark.png") 1024
Save-MarkPng (Join-Path $root "site/assets/kaelis-mark.png") 512
Save-MarkPng (Join-Path $root "site/assets/kaelis-mark-192.png") 192
Save-MarkPng (Join-Path $root "site/assets/kaelis-mark-512.png") 512
Save-MarkPng (Join-Path $root "site/assets/kaelis-touch.png") 180
Save-MarkIco (Join-Path $root "assets/kaelis.ico") @(16, 20, 24, 32, 40, 48, 64, 128, 256)

# Social card: one mark, one line, no clutter. Text lives in the card, never in
# the icon itself.
function Save-SocialCard([string]$path) {
    $w = 1200; $h = 630
    $bitmap = New-Object System.Drawing.Bitmap $w, $h
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $graphics.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
    $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic

    $bounds = New-Object System.Drawing.Rectangle 0, 0, $w, $h
    $background = New-Object System.Drawing.Drawing2D.LinearGradientBrush $bounds, ([System.Drawing.Color]::FromArgb(255, 8, 9, 12)), ([System.Drawing.Color]::FromArgb(255, 12, 18, 31)), 20
    $graphics.FillRectangle($background, $bounds)

    $glowPath = New-Object System.Drawing.Drawing2D.GraphicsPath
    $glowPath.AddEllipse(-140, -40, 760, 760)
    $glow = New-Object System.Drawing.Drawing2D.PathGradientBrush $glowPath
    $glow.CenterColor = [System.Drawing.Color]::FromArgb(74, 61, 130, 246)
    $glow.SurroundColors = @([System.Drawing.Color]::FromArgb(0, 61, 130, 246))
    $graphics.FillPath($glow, $glowPath)

    $mark = New-MarkBitmap 1024
    $graphics.DrawImage($mark, 78, 215, 200, 200)

    try {
        $family = New-Object System.Drawing.FontFamily 'Segoe UI'
    } catch {
        $family = New-Object System.Drawing.FontFamily 'Arial'
    }
    $titleFont = New-Object System.Drawing.Font $family, 92, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
    $taglineFont = New-Object System.Drawing.Font $family, 34, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
    $detailFont = New-Object System.Drawing.Font $family, 20, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
    $titleBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 241, 244, 248))
    $mutedBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 152, 160, 174))
    $accentBrush = New-Object System.Drawing.SolidBrush $accent

    $graphics.DrawString('Kaelis', $titleFont, $titleBrush, 320, 224)
    $graphics.DrawString('Minecraft, composed.', $taglineFont, $mutedBrush, 324, 350)
    $graphics.FillRectangle($accentBrush, 324, 414, 56, 4)
    $graphics.DrawString('Native launcher for Windows, macOS, and Linux', $detailFont, $mutedBrush, 324, 446)

    $encoder = [System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object { $_.MimeType -eq 'image/jpeg' }
    $parameters = New-Object System.Drawing.Imaging.EncoderParameters 1
    $parameters.Param[0] = New-Object System.Drawing.Imaging.EncoderParameter ([System.Drawing.Imaging.Encoder]::Quality, 92)
    $bitmap.Save($path, $encoder, $parameters)

    $parameters.Dispose()
    $titleFont.Dispose(); $taglineFont.Dispose(); $detailFont.Dispose(); $family.Dispose()
    $titleBrush.Dispose(); $mutedBrush.Dispose(); $accentBrush.Dispose()
    $glow.Dispose(); $glowPath.Dispose(); $background.Dispose(); $mark.Dispose()
    $graphics.Dispose(); $bitmap.Dispose()
    Write-Host "wrote $path (1200x630)"
}

Save-SocialCard (Join-Path $root "site/assets/og-kaelis.jpg")
