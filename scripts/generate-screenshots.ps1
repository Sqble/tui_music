param(
    [string[]]$Pages = @("library", "lyrics", "stats", "online", "actions"),
    [string[]]$Resolution = @("120x36", "90x30"),
    [double]$FontScale = 1.0,
    [string]$AssetOutputDir = "",
    [switch]$Debug
)

$ErrorActionPreference = "Stop"

$profile = if ($Debug) { "debug" } else { "release" }
$cargoArgs = @("build")
if (-not $Debug) {
    $cargoArgs += "--release"
}

Write-Host "Building tune executable ($profile)..." -ForegroundColor Cyan
& cargo @cargoArgs

$exeName = if ($IsWindows -or $env:OS -eq "Windows_NT") { "tune.exe" } else { "tune" }
$exe = Join-Path -Path (Join-Path -Path "target" -ChildPath $profile) -ChildPath $exeName
if (-not (Test-Path $exe)) {
    throw "Executable not found: $exe"
}

$runArgs = @(
    "--screenshots",
    "--screenshot-pages",
    ($Pages -join ","),
    "--screenshot-font-scale",
    ([string]$FontScale)
)

foreach ($size in $Resolution) {
    $runArgs += @("--screenshot-size", $size)
}

Write-Host "Generating screenshots beside $exe..." -ForegroundColor Cyan
& $exe @runArgs

$exeDir = Split-Path -Parent $exe
if ($AssetOutputDir -ne "") {
    if (-not (Test-Path -LiteralPath $AssetOutputDir)) {
        New-Item -ItemType Directory -Path $AssetOutputDir | Out-Null
    }
    Remove-Item -LiteralPath (Join-Path -Path $AssetOutputDir -ChildPath "tunetui-screenshots-manifest.txt") -ErrorAction SilentlyContinue
    Remove-Item -Path (Join-Path -Path $AssetOutputDir -ChildPath "tunetui-*.svg") -ErrorAction SilentlyContinue
    Copy-Item -Path (Join-Path -Path $exeDir -ChildPath "tunetui-*.svg") -Destination $AssetOutputDir
    Copy-Item -LiteralPath (Join-Path -Path $exeDir -ChildPath "tunetui-screenshots-manifest.txt") -Destination $AssetOutputDir
    Write-Host "Copied docs screenshots to $AssetOutputDir" -ForegroundColor Green
}
Write-Host "Generated screenshots in $exeDir" -ForegroundColor Green
