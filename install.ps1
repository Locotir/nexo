# nexo installer for Windows: installs what is missing (the Visual Studio C++ build tools, Rust),
# then builds nexo with cargo.
#   irm https://raw.githubusercontent.com/Locotir/nexo/main/install.ps1 | iex

function Install-Nexo {
    $ErrorActionPreference = 'Stop'
    $repo = 'https://github.com/Locotir/nexo'
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

    # the Rust compiler for Windows links with the Visual Studio C++ build tools
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    $msvc = (Test-Path $vswhere) -and
        (& $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath)
    if (-not $msvc) {
        if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
            Write-Host 'nexo: the Visual Studio C++ build tools are missing and winget is not here to install them.'
            Write-Host 'nexo: install "Build Tools for Visual Studio" (C++ workload), then run this again.'
            return
        }
        Write-Host 'nexo: installing the Visual Studio C++ build tools (a few minutes; Windows may ask to allow it)'
        winget install --id Microsoft.VisualStudio.2022.BuildTools -e --silent `
            --accept-source-agreements --accept-package-agreements `
            --override '--quiet --wait --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'
    }

    # rustup puts cargo here, often before this terminal's PATH knows it
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path (Join-Path $cargoBin 'cargo.exe'))) {
        $env:Path = "$cargoBin;$env:Path"
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-Host 'nexo: installing Rust (rustup, https://rustup.rs)'
        $arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'aarch64' } else { 'x86_64' }
        $init = Join-Path $env:TEMP 'rustup-init.exe'
        Invoke-WebRequest "https://win.rustup.rs/$arch" -OutFile $init -UseBasicParsing
        & $init -y --profile minimal
        Remove-Item $init -ErrorAction SilentlyContinue
        $env:Path = "$cargoBin;$env:Path"
    }

    Write-Host 'nexo: building the latest nexo (a few minutes the first time)'
    cargo install --locked --git $repo nexo-cli
    if ($LASTEXITCODE -ne 0) {
        Write-Host 'nexo: the build failed; the lines above say why.'
        return
    }

    $exe = Get-Command nexo -ErrorAction SilentlyContinue
    if ($exe -and ((Split-Path $exe.Source -Parent) -eq $cargoBin)) {
        Write-Host "nexo: installed: $($exe.Source). Run: nexo"
    } elseif ($exe) {
        Write-Host "nexo: installed in $cargoBin, but another nexo comes first on your PATH: $($exe.Source)"
        Write-Host "nexo: remove that one, or put $cargoBin first on your PATH."
    } else {
        Write-Host "nexo: installed in $cargoBin. Open a new terminal, then run: nexo"
    }
}

Install-Nexo
