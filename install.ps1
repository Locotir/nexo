# nexo installer for Windows: downloads the ready-built nexo from the latest GitHub release and puts it
# in %LOCALAPPDATA%\Programs\nexo, on PATH. If that fails (or NEXO_BUILD=1), it installs what the build
# needs (the Visual Studio C++ build tools, Rust; Windows asks once to allow them) and builds nexo.
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

# the release file for this processor, checked against the release's SHA-256 list; $null when none fits
function Get-NexoRelease($repo, $stage) {
    $name = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'nexo-windows-aarch64.exe' } else { 'nexo-windows-x86_64.exe' }
    $base = "$repo/releases/latest/download"
    try {
        Write-Host "nexo: downloading $name from the latest release"
        $sums = Join-Path $stage 'SHA256SUMS'
        $file = Join-Path $stage 'nexo.exe'
        Invoke-WebRequest "$base/SHA256SUMS" -OutFile $sums -UseBasicParsing
        Invoke-WebRequest "$base/$name" -OutFile $file -UseBasicParsing
        $line = Get-Content $sums | Where-Object { ($_ -split '\s+', 2)[1] -replace '^\*', '' -eq $name } | Select-Object -First 1
        if (-not $line) { Write-Host "nexo: SHA256SUMS does not list $name"; return $null }
        $want = ($line -split '\s+', 2)[0].ToLower()
        if ((Get-FileHash $file -Algorithm SHA256).Hash.ToLower() -ne $want) {
            Write-Host "nexo: $name does not match its SHA-256: not used"
            return $null
        }
        return $file
    } catch {
        Write-Host "nexo: no ready-built release to use: $($_.Exception.Message)"
        return $null
    }
}

# what the build needs (the C++ build tools, Rust), then the build itself; $null when it failed
function Build-Nexo($repo, $stage, $cargoBin) {
    # the Rust compiler for Windows links with the Visual Studio C++ build tools
    if (-not (Test-NexoMsvc)) {
        $ErrorActionPreference = 'Continue'
        $ok = Install-NexoMsvc
        $ErrorActionPreference = 'Stop'
        if (-not $ok) {
            Write-Host 'nexo: the C++ build tools are still missing: the Windows prompt was declined or the install failed.'
            Write-Host 'nexo: run this same command again and allow the prompt.'
            return $null
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
    cargo install --quiet --locked --git $repo nexo-cli --root (Join-Path $stage 'build')
    if ($LASTEXITCODE -ne 0) {
        Write-Host 'nexo: the build failed; the lines above say why.'
        return $null
    }
    return (Join-Path $stage 'build\bin\nexo.exe')
}

function Install-Nexo {
    $ErrorActionPreference = 'Stop'
    $repo = 'https://github.com/Locotir/nexo'
    $cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
    # where nexo goes: the per-user program folder of Windows (%LOCALAPPDATA%\Programs), no admin needed
    $dir = if ($env:NEXO_INSTALL_DIR) { $env:NEXO_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\nexo' }
    $stage = Join-Path $env:TEMP "nexo-install-$PID"
    New-Item -ItemType Directory -Force -Path $stage | Out-Null

    # the ready-built release first: seconds, and no compiler; NEXO_BUILD=1 builds from source instead
    $built = $null
    if (-not $env:NEXO_BUILD) { $built = Get-NexoRelease $repo $stage }
    if (-not $built) {
        if (-not $env:NEXO_BUILD) { Write-Host 'nexo: building from source instead' }
        $built = Build-Nexo $repo $stage $cargoBin
    }
    if (-not $built) {
        Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue
        return
    }

    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $target = Join-Path $dir 'nexo.exe'
    # Windows cannot write over a running program, but it can rename one
    if (Test-Path $target) {
        Remove-Item "$target.old" -Force -ErrorAction SilentlyContinue
        Rename-Item $target 'nexo.exe.old'
    }
    Copy-Item $built $target
    Remove-Item $stage -Recurse -Force -ErrorAction SilentlyContinue

    # older installs left nexo in cargo's own folder, where it would come first or confuse
    $old = Join-Path $cargoBin 'nexo.exe'
    if (($cargoBin -ne $dir) -and (Test-Path $old)) {
        $ErrorActionPreference = 'Continue'
        cargo uninstall --quiet nexo-cli 2>$null | Out-Null
        $ErrorActionPreference = 'Stop'
        if (Test-Path $old) { Rename-Item $old 'nexo.exe.old' -Force -ErrorAction SilentlyContinue }
        Write-Host "nexo: removed the old copy in $cargoBin"
    }

    # put the folder on the user's PATH for new terminals, once (no admin needed)
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not (($userPath -split ';') -contains $dir)) {
        $joined = if ($userPath) { "$dir;$userPath" } else { $dir }
        [Environment]::SetEnvironmentVariable('Path', $joined, 'User')
        Write-Host "nexo: added $dir to your PATH for new terminals"
    }
    if (-not (($env:Path -split ';') -contains $dir)) { $env:Path = "$dir;$env:Path" }

    $exe = Get-Command nexo -ErrorAction SilentlyContinue
    if ($exe -and ($exe.Source -eq $target)) {
        Write-Host "nexo: installed: $target. Run: nexo  (a terminal opened before this needs reopening)"
    } else {
        Write-Host "nexo: installed: $target, but another nexo comes first on your PATH: $($exe.Source)"
        Write-Host "nexo: remove that one, or run $target"
    }
}

Install-Nexo
