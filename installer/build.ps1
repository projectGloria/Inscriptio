# Builds the release exe and the installer: dist\Inscriptio-<version>-setup.exe
# Usage (from anywhere):  & .\installer\build.ps1
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    # Single source of truth for the version: the [package] version in Cargo.toml.
    $version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
    if (-not $version) { throw 'No version found in Cargo.toml' }

    cargo build --release
    if ($LASTEXITCODE) { throw 'cargo build failed' }

    # winget --scope user puts Inno Setup under LocalAppData; a machine-wide install uses Program Files (x86).
    $iscc = @(
        (Get-Command ISCC.exe -ErrorAction SilentlyContinue).Source,
        "$env:LOCALAPPDATA\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
    ) | Where-Object { $_ -and (Test-Path $_) } | Select-Object -First 1
    if (-not $iscc) { throw 'ISCC.exe not found. Install it with: winget install JRSoftware.InnoSetup' }

    & $iscc /Qp "/DAppVersion=$version" installer\Inscriptio.iss
    if ($LASTEXITCODE) { throw 'ISCC failed' }
    Get-Item "dist\Inscriptio-$version-setup.exe" | Select-Object Name, Length
}
finally { Pop-Location }
