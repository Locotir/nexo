# nexo installer for Windows: builds nexo from source with cargo.
#   irm https://raw.githubusercontent.com/Locotir/nexo/main/install.ps1 | iex
# $env:NEXO_INSTALL_RUST = '1' installs Rust (rustup, with winget) without asking when it is missing.

function Install-Nexo {
    $ErrorActionPreference = 'Stop'
    $repo = 'https://github.com/Locotir/nexo'
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

    # rustup puts cargo here, often before this terminal's PATH knows it
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path (Join-Path $cargoBin 'cargo.exe'))) {
        $env:Path = "$cargoBin;$env:Path"
    }

    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Write-Host 'nexo: nexo is built with Rust, and cargo was not found.'
        $answer = $env:NEXO_INSTALL_RUST
        if (-not $answer) { $answer = Read-Host 'nexo: install Rust now with winget (Rustlang.Rustup)? [y/N]' }
        if ($answer -notmatch '^(y|yes|1)$') {
            Write-Host 'nexo: install Rust from https://rustup.rs (it also offers the Visual Studio build tools it needs), then run this again.'
            return
        }
        if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
            Write-Host 'nexo: winget is not available; install Rust from https://rustup.rs, then run this again.'
            return
        }
        winget install --id Rustlang.Rustup -e --accept-source-agreements --accept-package-agreements
        $env:Path = "$cargoBin;$env:Path"
        if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
            Write-Host 'nexo: Rust is installed but cargo is not found yet; open a new terminal and run this again.'
            return
        }
    }

    Write-Host 'nexo: building the latest nexo (a few minutes the first time)'
    cargo install --locked --git $repo nexo-cli
    if ($LASTEXITCODE -ne 0) {
        Write-Host 'nexo: the build failed. On Windows it needs the Visual Studio C++ build tools (rustup-init offers them).'
        return
    }

    $exe = Get-Command nexo -ErrorAction SilentlyContinue
    if ($exe -and ((Split-Path $exe.Source -Parent) -eq $cargoBin)) {
        Write-Host "nexo: installed: $($exe.Source). Run: nexo"
    } elseif ($exe) {
        Write-Host "nexo: installed in $cargoBin, but another nexo comes first on your PATH: $($exe.Source)"
        Write-Host "nexo: remove that one, or put $cargoBin first on your PATH."
    } else {
        Write-Host "nexo: installed in $cargoBin, which is not on this terminal's PATH yet: open a new terminal, then run nexo."
    }
}

Install-Nexo
