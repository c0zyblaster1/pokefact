$ErrorActionPreference = "Stop"

$repo = "c0zyblaster1/pokefact"
$url = "https://github.com/$repo/releases/latest/download/pokefact-windows-x64.exe"
$installDir = Join-Path $env:LOCALAPPDATA "pokefact"
$exePath = Join-Path $installDir "pokefact.exe"

Write-Host "Installing pokefact..." -ForegroundColor Cyan
New-Item -ItemType Directory -Force -Path $installDir | Out-Null

try {
    Invoke-WebRequest -Uri $url -OutFile $exePath -UseBasicParsing
} catch {
    Write-Host "Download failed. Make sure a GitHub Release has been published at https://github.com/$repo/releases" -ForegroundColor Red
    exit 1
}

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if (-not $userPath) { $userPath = "" }

if ($userPath.Split(";") -notcontains $installDir) {
    $newPath = ($userPath.TrimEnd(";") + ";" + $installDir).TrimStart(";")
    [Environment]::SetEnvironmentVariable("Path", $newPath, "User")
    $env:Path = "$env:Path;$installDir"
    Write-Host "Added $installDir to your PATH." -ForegroundColor Yellow
}

Write-Host "Done! Try running: pokefact -p gengar" -ForegroundColor Green