param(
    [Parameter(Mandatory = $true)][string]$Version,
    [string]$TargetDir = "target\release",
    [string]$DistDir = "dist"
)

$ErrorActionPreference = "Stop"
$mpvUrl = "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/20260903/mpv-x86_64-20260903-git-69e63f425a.7z"
$mpvSha = "418dbfb5feb851cbed33d6c05d8481ba71802621bfd6efe8974522b28d42ac97"
$ffmpegUrl = "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/20260903/ffmpeg-x86_64-git-9fc8c785e.7z"
$ffmpegSha = "03bc01eff87973fd757ac0ef5ead32796223b4f8435c46965e3238ab11d5b685"

$payload = Join-Path $DistDir "windows"
$downloads = Join-Path $DistDir "downloads"
New-Item -ItemType Directory -Force $payload, $downloads | Out-Null
$payload = (Resolve-Path $payload).Path
$downloads = (Resolve-Path $downloads).Path

function Get-VerifiedArchive([string]$Url, [string]$Sha, [string]$Name) {
    $archive = Join-Path $downloads $Name
    Invoke-WebRequest -Uri $Url -OutFile $archive
    $actual = (Get-FileHash -Algorithm SHA256 $archive).Hash.ToLowerInvariant()
    if ($actual -ne $Sha) { throw "SHA-256 mismatch for $Name: $actual" }
    return $archive
}

$mpvArchive = Get-VerifiedArchive $mpvUrl $mpvSha "mpv.7z"
$ffmpegArchive = Get-VerifiedArchive $ffmpegUrl $ffmpegSha "ffmpeg.7z"
7z x $mpvArchive "-o$downloads\mpv" -y | Out-Null
7z x $ffmpegArchive "-o$downloads\ffmpeg" -y | Out-Null

Copy-Item (Join-Path $TargetDir "muscli.exe") $payload
Copy-Item (Get-ChildItem "$downloads\mpv" -Recurse -Filter "mpv.exe" | Select-Object -First 1).FullName $payload
Copy-Item (Get-ChildItem "$downloads\ffmpeg" -Recurse -Filter "ffmpeg.exe" | Select-Object -First 1).FullName $payload

$iscc = (Get-Command iscc.exe -ErrorAction SilentlyContinue).Source
if (-not $iscc) {
    $iscc = "C:\Program Files (x86)\Inno Setup 6\ISCC.exe"
}
& $iscc "/DMyAppVersion=$Version" "/DPayloadDir=$payload" "installer\windows\muscli.iss"
