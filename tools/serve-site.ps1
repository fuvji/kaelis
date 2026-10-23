# Serve site/ over HTTP for local preview.
#
#   powershell -NoProfile -File tools/serve-site.ps1 -Port 8088
#
# The page uses .webmanifest, .mp4, and .vtt files; the default Windows MIME
# table gets those wrong, so they are mapped explicitly here.

param(
    [int]$Port = 8088,
    [string]$Root
)

$ErrorActionPreference = "Stop"
if (-not $Root) {
    $Root = Join-Path (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)) "site"
}
$Root = (Resolve-Path $Root).Path

$types = @{
    ".html"       = "text/html; charset=utf-8"
    ".css"        = "text/css; charset=utf-8"
    ".js"         = "text/javascript; charset=utf-8"
    ".json"       = "application/json; charset=utf-8"
    ".webmanifest" = "application/manifest+json; charset=utf-8"
    ".xml"        = "application/xml; charset=utf-8"
    ".txt"        = "text/plain; charset=utf-8"
    ".md"         = "text/markdown; charset=utf-8"
    ".svg"        = "image/svg+xml"
    ".png"        = "image/png"
    ".jpg"        = "image/jpeg"
    ".jpeg"       = "image/jpeg"
    ".webp"       = "image/webp"
    ".ico"        = "image/x-icon"
    ".mp4"        = "video/mp4"
    ".vtt"        = "text/vtt; charset=utf-8"
    ".zip"        = "application/zip"
    ".exe"        = "application/octet-stream"
}

$listener = [System.Net.HttpListener]::new()
$prefix = "http://127.0.0.1:$Port/"
$listener.Prefixes.Add($prefix)
$listener.Start()
Write-Host "Serving $Root at $prefix"

try {
    while ($listener.IsListening) {
        $context = $listener.GetContext()
        $request = $context.Request
        $response = $context.Response
        try {
            $relative = [Uri]::UnescapeDataString($request.Url.AbsolutePath).TrimStart("/")
            if ([string]::IsNullOrWhiteSpace($relative)) { $relative = "index.html" }
            $path = Join-Path $Root $relative
            $resolved = [System.IO.Path]::GetFullPath($path)
            # Refuse anything that escapes the site root.
            if (-not $resolved.StartsWith($Root, [StringComparison]::OrdinalIgnoreCase)) {
                $response.StatusCode = 403
                $response.Close()
                continue
            }
            if (Test-Path $resolved -PathType Container) {
                $resolved = Join-Path $resolved "index.html"
            }
            if (Test-Path $resolved -PathType Leaf) {
                $bytes = [System.IO.File]::ReadAllBytes($resolved)
                $ext = [System.IO.Path]::GetExtension($resolved).ToLowerInvariant()
                if ($types.ContainsKey($ext)) {
                    $response.ContentType = $types[$ext]
                } else {
                    $response.ContentType = "application/octet-stream"
                }
                $response.Headers.Add("Cache-Control", "no-store")
                $response.ContentLength64 = $bytes.Length
                $response.OutputStream.Write($bytes, 0, $bytes.Length)
                Write-Host ("200 " + $relative)
            } else {
                $response.StatusCode = 404
                $body = [System.Text.Encoding]::UTF8.GetBytes("404 " + $relative)
                $response.ContentType = "text/plain; charset=utf-8"
                $response.OutputStream.Write($body, 0, $body.Length)
                Write-Host ("404 " + $relative)
            }
        } catch {
            $response.StatusCode = 500
            Write-Host ("500 " + $_.Exception.Message)
        } finally {
            $response.Close()
        }
    }
} finally {
    $listener.Stop()
}
