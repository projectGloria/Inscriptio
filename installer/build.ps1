$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Push-Location $root
try {
    $metadata = cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Could not read Cargo metadata' }
    $version = ($metadata.packages | Where-Object name -eq 'inscriptio').version
    $manifest = Join-Path $root 'gloria.toml'
    $manifestVersion = [regex]::Match([IO.File]::ReadAllText($manifest), '(?m)^version\s*=\s*"([^"]+)"').Groups[1].Value
    if (!$version -or $manifestVersion -cne $version) { throw 'Cargo.toml and gloria.toml versions must match' }

    cargo build --release --locked
    if ($LASTEXITCODE -ne 0) { throw 'Release build failed' }
    $stage = Join-Path $root ('target\installer\' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $stage -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $metadata.target_directory 'release\inscriptio.exe') -Destination $stage

    $out = Join-Path $root 'dist'
    & npx --yes gloria-setup@latest --app $stage --manifest $manifest --version $version --out $out
    if ($LASTEXITCODE -ne 0) { throw 'GloriaSetup packaging failed' }

    $setup = Get-Item -LiteralPath (Join-Path $out "Inscriptio_$($version)_x64-setup.exe")
    $result = Start-Process -FilePath $setup.FullName -ArgumentList '/S --dry-run' -Wait -PassThru
    if ($result.ExitCode -ne 0) { throw "Installer dry run failed ($($result.ExitCode))" }
    $setup
} finally {
    Pop-Location
}
