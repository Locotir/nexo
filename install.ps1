# nexo installer for Windows: installs what is missing (the Visual Studio C++ build tools, Rust),
# then builds nexo with cargo. Nothing to look up or click through: Windows only asks to allow the
# build tools installer (one prompt).
#   irm https://raw.githubusercontent.com/Locotir/nexo/main/install.ps1 | iex

$NexoVsWhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$NexoVsSetup = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\setup.exe'
# the C++ workload with its recommended parts (the compiler, the linker and the Windows SDK), no window
$NexoVsArgs = '--quiet --wait --norestart --nocache --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended'

# true when some Visual Studio or Build Tools on this machine has the C++ compiler and linker
function Test-NexoMsvc {
    if (-not (Test-Path $NexoVsWhere)) { return $false }
    $found = & $NexoVsWhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    return [bool]$found
}

# runs an installer elevated and waits; false when it could not start (the prompt was declined)
function Invoke-NexoElevated($file, $arguments) {
    try {
        $p = Start-Process -FilePath $file -ArgumentList $arguments -Verb RunAs -Wait -PassThru
        # 3010: done, a restart is advised later; the tools already work for this build
        if ($p.ExitCode -ne 0 -and $p.ExitCode -ne 3010) { Write-Host "nexo: the installer ended with code $($p.ExitCode)" }
        return $true
    } catch {
        Write-Host "nexo: the installer did not start: $($_.Exception.Message)"
        return $false
    }
}

# the three ways in, from the least to download to the most: each one is checked before the next
function Install-NexoMsvc {
    Write-Host 'nexo: installing the Visual Studio C++ build tools (5 to 15 minutes; allow the Windows prompt)'

    # 1. a Visual Studio or Build Tools is here without the C++ tools: add them to it
    #    (winget would say "already installed" and do nothing)
    if ((Test-Path $NexoVsWhere) -and (Test-Path $NexoVsSetup)) {
        $have = & $NexoVsWhere -products * -property installationPath | Select-Object -First 1
        if ($have) {
            Write-Host "nexo: adding the C++ tools to $have"
            [void](Invoke-NexoElevated $NexoVsSetup "modify --installPath `"$have`" $NexoVsArgs")
            if (Test-NexoMsvc) { return $true }
        }
    }

    # 2. winget, when this Windows has it
    if (Get-Command winget -ErrorAction SilentlyContinue) {
        winget install --id Microsoft.VisualStudio.2022.BuildTools -e --silent `
            --accept-source-agreements --accept-package-agreements --override $NexoVsArgs
        if (Test-NexoMsvc) { return $true }
    }

    # 3. Microsoft's own Build Tools installer, downloaded from Microsoft
    $boot = Join-Path $env:TEMP 'vs_BuildTools.exe'
    try {
        Invoke-WebRequest 'https://aka.ms/vs/17/release/vs_BuildTools.exe' -OutFile $boot -UseBasicParsing
        [void](Invoke-NexoElevated $boot $NexoVsArgs)
    } catch {
        Write-Host "nexo: could not download the Build Tools installer: $($_.Exception.Message)"
    } finally {
        Remove-Item $boot -ErrorAction SilentlyContinue
    }
    return (Test-NexoMsvc)
}

function Install-Nexo {
    $ErrorActionPreference = 'Stop'
    $repo = 'https://github.com/Locotir/nexo'
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'

    # the Rust compiler for Windows links with the Visual Studio C++ build tools
    if (-not (Test-NexoMsvc)) {
        $ErrorActionPreference = 'Continue'
        $ok = Install-NexoMsvc
        $ErrorActionPreference = 'Stop'
        if (-not $ok) {
            Write-Host 'nexo: the C++ build tools are still missing: the Windows prompt was declined or the install failed.'
            Write-Host 'nexo: run this same command again and allow the prompt.'
            return
        }
        Write-Host 'nexo: C++ build tools ready'
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
