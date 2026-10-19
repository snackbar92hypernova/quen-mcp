# quen's installer (INSTALL.md, Quick install), shipped as a release asset beside the zips. It gets one
# quen release from GitHub, or from a folder of its files, and checks every zip against the release's SHA256SUMS.txt
# before opening it. Then it puts quen.exe in %USERPROFILE%\.quen\bin and, unless -NoGamePackage, quen's in-game package
# in the game folder. It installs no third-party tool, needs no admin rights, asks before each change, and sends what it
# replaces or removes to the Recycle Bin. Every check runs before the first change: a refusal changes nothing.
# Usage: powershell -ExecutionPolicy Bypass -File install.ps1 [-Version <x.y.z>] [-From <folder>] [-GameDir <folder>]
#            [-NoGamePackage] [-WithRenderDoc] [-Update | -Uninstall] [-WhatIf]
# Tested with Windows PowerShell 5.1, the one in Windows. Kept ASCII: it reads a file without a BOM in the ANSI code page.
[CmdletBinding(SupportsShouldProcess)]
param(
    [string]$Version,
    [string]$From,
    [string]$GameDir,
    [switch]$NoGamePackage,
    [switch]$WithRenderDoc,
    [switch]$Update,
    [switch]$Uninstall
)
# The body has its own scope, so `irm ... | iex` leaves only the parameters behind in the caller's session.
& {
    try {
        $ErrorActionPreference = 'Stop'
        $ProgressPreference = 'SilentlyContinue'  # Windows PowerShell's progress bar slows a download many times over
        Add-Type -AssemblyName System.IO.Compression.FileSystem, Microsoft.VisualBasic

        $Repo = 'snackbar92hypernova/quen-mcp'
        # -WhatIf is this script's own: cmdlets here would take it too, and some then return nothing (ForEach-Object's
        # member form). It is global under -File, so a module's functions still take it: see Sha.
        $dry = [bool]$WhatIfPreference
        $WhatIfPreference = $false
        if (-not $env:USERPROFILE) { throw 'install: USERPROFILE is not set' }
        $quenHome = Join-Path $env:USERPROFILE '.quen'
        $bin = Join-Path $quenHome 'bin'
        $exe = Join-Path $bin 'quen.exe'
        $slDir = Join-Path $quenHome 'tools\streamline'
        $envKey = 'Environment'  # HKCU's: the user's environment variables
        $done = New-Object Collections.Generic.List[string]

        function Say($text) { Write-Host "install: $text" }
        function Refuse($why) { throw "install: refused, nothing was changed: $why" }
        function Ask($question) {
            if ($dry) { Write-Host "What if: ask `"$question`" (taken as yes)"; return $true }
            "$(Read-Host "$question [y/N]")".Trim() -match '^(y|yes)$'
        }
        # A change: made now, or with -WhatIf only printed.
        function Change($what, [scriptblock]$do) {
            if ($dry) { Write-Host "What if: $what"; return }
            Say $what
            & $do
            $done.Add($what)
        }
        # A console program in this console (no window of its own): its output lines, stderr folded in, and its exit code.
        # Windows PowerShell turns each stderr line into an error, which 'Stop' would throw on: hence the local Continue.
        function Run {
            $ErrorActionPreference = 'Continue'
            $prog = $args[0]
            $rest = @($args | Select-Object -Skip 1)  # an array even for one: a lone string doesn't splat as one argument
            $out = @(& $prog @rest 2>&1 | ForEach-Object { "$_" })
            [pscustomobject]@{ out = $out; code = $LASTEXITCODE }
        }
        # The JSON of `quen setup` or `quen doctor` (both read-only), or $null. quen prints UTF-8; Windows PowerShell would
        # read it in the console's code page.
        function QuenJson($quen, $command) {
            $encoding = [Console]::OutputEncoding
            [Console]::OutputEncoding = [Text.Encoding]::UTF8
            try {
                $ErrorActionPreference = 'Continue'
                $text = (& $quen $command 2>$null) -join "`n"
            } finally { [Console]::OutputEncoding = $encoding }
            try { $text | ConvertFrom-Json } catch { $null }
        }
        # By stream: Get-FileHash -LiteralPath, under -WhatIf, hashes nothing.
        function Sha($path) {
            $stream = [IO.File]::OpenRead($path)
            try { (Get-FileHash -Algorithm SHA256 -InputStream $stream).Hash.ToLower() } finally { $stream.Dispose() }
        }
        # A zip's files: each one's path in the zip (with \) and the sha256 of its bytes.
        function ZipFiles($zipPath) {
            $zip = [IO.Compression.ZipFile]::OpenRead($zipPath)
            try {
                foreach ($entry in $zip.Entries | Where-Object Name) {
                    $stream = $entry.Open()
                    try { $sha = (Get-FileHash -Algorithm SHA256 -InputStream $stream).Hash.ToLower() } finally { $stream.Dispose() }
                    [pscustomobject]@{ rel = $entry.FullName.Replace('/', '\'); sha = $sha }
                }
            } finally { $zip.Dispose() }
        }
        # Writes one file of a zip to $dest (never over a file) and checks it is the zip's bytes.
        function Place($zipPath, $file, $dest) {
            $zip = [IO.Compression.ZipFile]::OpenRead($zipPath)
            try {
                [void][IO.Directory]::CreateDirectory((Split-Path $dest))
                [IO.Compression.ZipFileExtensions]::ExtractToFile($zip.GetEntry($file.rel.Replace('\', '/')), $dest)
            } finally { $zip.Dispose() }
            $got = Sha $dest
            if ($got -ne $file.sha) { throw "install: $dest has sha256 $got, not the release's $($file.sha)" }
        }
        # Which of $files are missing or other bytes under $root.
        function Differs($files, $root) {
            @($files | Where-Object { $d = Join-Path $root $_.rel; -not (Test-Path -LiteralPath $d) -or (Sha $d) -ne $_.sha })
        }
        # Nothing is deleted for good: it goes to the Recycle Bin. QUEN_TEST_RECYCLE_TO (tests only, as in quen's own
        # win::recycle) moves it into that folder instead.
        function Recycle($path) {
            if ($env:QUEN_TEST_RECYCLE_TO) {
                $name = '{0}-{1}' -f [guid]::NewGuid().ToString('n').Substring(0, 8), (Split-Path -Leaf $path)
                Move-Item -LiteralPath $path -Destination (Join-Path $env:QUEN_TEST_RECYCLE_TO $name) -WhatIf:$false
            } elseif (Test-Path -LiteralPath $path -PathType Container) {
                [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory($path, 'OnlyErrorDialogs', 'SendToRecycleBin')
            } else {
                [Microsoft.VisualBasic.FileIO.FileSystem]::DeleteFile($path, 'OnlyErrorDialogs', 'SendToRecycleBin')
            }
        }
        # A drive that isn't fixed (removable, network) has no Recycle Bin: Windows would delete there for good.
        # ponytail: the drive type only, as quen's win::recycle_bin starts; a fixed drive with its bin turned off deletes.
        function NeedsBin($path) {
            $root = [IO.Path]::GetPathRoot($path)
            $type = try { ([IO.DriveInfo]$root).DriveType } catch { 'unknown' }
            if (-not $env:QUEN_TEST_RECYCLE_TO -and "$type" -ne 'Fixed') {
                Refuse "$root is no fixed drive, so it has no Recycle Bin, and this installer deletes nothing for good. Move $path yourself"
            }
        }
        function Full($path, $what) {
            try { (Resolve-Path -LiteralPath $path).ProviderPath } catch { Refuse "no $what at $path" }
        }
        # quen.exe processes started from $exe: Windows can't replace or remove a running program.
        function Running { @(Get-Process quen -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $exe }) }
        # quen's state, from `quen doctor`: refused while a session runs or its journal is left, since stopping quen or
        # changing the game folder then would leave the folder as the session changed it. Returns the doctor's result.
        function Idle($quen) {
            $doc = QuenJson $quen doctor
            if (-not $doc) { Refuse "$quen doctor answered no JSON" }
            if ($doc.state.config) { Refuse "quen's config doesn't load ($($doc.state.config)): fix it first" }
            if ($doc.state.journal) {
                Refuse "a quen session's journal is there ($($doc.state.journal)): a session is live, or one didn't end. End it (session_end), or run quen recover, then run this again"
            }
            if ($doc.state.lock -is [string]) { Refuse "another quen holds the game lock ($($doc.state.lock)): a session is live there. End it first" }
            $doc
        }
        # The game folder: -GameDir, else the one `quen setup` gives.
        function Find-Game($quen) {
            if ($GameDir) { $dir = Full $GameDir 'folder' } else {
                $g = (QuenJson $quen setup).paths | Where-Object key -eq 'game_dir'
                if (-not $g.path -or $g.problem) {
                    $found = @($g.candidates | ForEach-Object { $_.path }) -join '; '
                    Refuse ("quen setup gives no game folder ($($g.problem))" + $(if ($found) { "; found: $found" }) +
                        '. Run this again with -GameDir <the game folder>, or with -NoGamePackage')
                }
                $dir = [Environment]::ExpandEnvironmentVariables($g.path)
            }
            if (-not (Test-Path -LiteralPath (Join-Path $dir 'bin\x64_dx12') -PathType Container)) {
                Refuse "$dir has no bin\x64_dx12, so it isn't the game folder"
            }
            $w3 = @(Get-Process witcher3 -ErrorAction SilentlyContinue)
            if ($w3) { Refuse "The Witcher 3 is running (witcher3.exe, pid $($w3.Id -join ', ')): close it first" }
            $dir
        }
        # The user PATH as Windows keeps it: %VARIABLES% unexpanded, which [Environment]'s User target would expand for good.
        function UserPath {
            $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($envKey)
            try { [string]$key.GetValue('Path', '', 'DoNotExpandEnvironmentNames') } finally { $key.Close() }
        }
        function IsBin($entry) { [Environment]::ExpandEnvironmentVariables($entry).Trim().TrimEnd('\') -eq $bin }
        function Set-UserPath($value) {
            $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($envKey)
            try { $key.SetValue('Path', $value, 'ExpandString') } finally { $key.Close() }
            # Programs started from now on see it once Windows is told: WM_SETTINGCHANGE "Environment", to every window.
            if (-not ('QuenInstall.Native' -as [type])) {
                Add-Type -Namespace QuenInstall -Name Native -MemberDefinition ('[DllImport("user32.dll", CharSet = CharSet.Unicode)] ' +
                    'public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint msg, UIntPtr wParam, string lParam, ' +
                    'uint flags, uint timeout, out UIntPtr result);')
            }
            $result = [UIntPtr]::Zero
            [void][QuenInstall.Native]::SendMessageTimeout([IntPtr]0xffff, 0x1a, [UIntPtr]::Zero, $envKey, 2, 5000, [ref]$result)
        }
        # A path that goes to the Recycle Bin, with a folder's files.
        function Show($p) {
            Write-Host "  to the Recycle Bin: $p"
            if (Test-Path -LiteralPath $p -PathType Container) {
                Get-ChildItem -LiteralPath $p -Recurse -File | ForEach-Object { Write-Host "    $($_.FullName)" }
            }
        }
        function Find-Claude { (Get-Command claude -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1).Source }
        function Stop-Quen($procs) {
            Change "stop quen.exe (pid $($procs.Id -join ', '))" {
                $procs | Stop-Process -Force
                $procs | Wait-Process -Timeout 15 -ErrorAction SilentlyContinue
            }
        }
        # ponytail: idle is checked once, before the questions; a session launched while they wait isn't seen.
        function Ask-Stop($procs) {
            if ($procs -and -not (Ask ("quen.exe is running from $bin (pid $($procs.Id -join ', '): your MCP clients' quen and " +
                        'its quen serve; no session is live). Stop it? Your clients then reconnect quen (/mcp in Claude Code)'))) {
                Refuse "quen.exe is running from ${bin}: Windows can't replace or remove a running program. Close your MCP clients first"
            }
        }

        if ($Update -and $Uninstall) { Refuse '-Update and -Uninstall exclude each other' }
        if ($Version) { $Version = $Version.TrimStart('v') }  # the tag is v<version>

        if ($Uninstall) {
            if (-not (Test-Path -LiteralPath $exe)) { Refuse "quen isn't installed at $exe" }
            [void](Idle $exe)
            $procs = Running
            $remove = @($bin)
            if (-not $NoGamePackage) {
                $game = Find-Game $exe
                # Mods first: script mods left calling an ASI that isn't there fail the game's script compile.
                $remove += @(@('Mods\modQuen', 'Mods\modQuenRpc', 'bin\x64_dx12\W3Quen.asi') | ForEach-Object { Join-Path $game $_ } |
                    Where-Object { Test-Path -LiteralPath $_ })
            }
            $remove | ForEach-Object { NeedsBin $_ }
            $path = UserPath
            $onPath = @($path -split ';' | Where-Object { IsBin $_ }).Count -gt 0
            $claude = Find-Claude
            Say 'uninstall:'
            if ($procs) { Write-Host "  stop: quen.exe (pid $($procs.Id -join ', '): your MCP clients' quen and its quen serve; no session is live)" }
            $remove | ForEach-Object { Show $_ }
            if ($onPath) { Write-Host "  out of your user PATH: $bin" }
            if ($claude) { Write-Host '  out of Claude Code: claude mcp remove --scope user quen' }
            if (-not (Ask 'Uninstall quen as listed?')) { Say 'nothing was changed'; return }
            if ($claude) {
                Change 'remove quen from Claude Code: claude mcp remove --scope user quen' {
                    Write-Host "  $((Run $claude mcp remove --scope user quen).out -join ' ')"
                }
            }
            if ($procs) { Stop-Quen $procs }
            foreach ($p in $remove) { Change "move $p to the Recycle Bin" { Recycle $p } }
            if ($onPath) {
                Change "take $bin out of your user PATH" { Set-UserPath (@($path -split ';' | Where-Object { -not (IsBin $_) }) -join ';') }
            }
            Say "$quenHome stays: your config, ledger, logs and captures. Move it to the Recycle Bin yourself once you no longer need them"
            return
        }

        $tmp = Join-Path ([IO.Path]::GetTempPath()) ('quen-install-' + [guid]::NewGuid().ToString('n'))
        [void][IO.Directory]::CreateDirectory($tmp)
        try {
            # The release: the folder -From names, else downloaded into the new temp folder.
            $want = @('quen-*-windows-x64.zip', 'SHA256SUMS.txt')
            if (-not $NoGamePackage) { $want += 'quen-w3-*.zip' }
            if ($WithRenderDoc) { $want += 'quen-streamline-dev-interposer-*.zip' }
            if ($From) { $dir = Full $From 'folder' } else {
                $dir = Join-Path $tmp 'release'
                [void][IO.Directory]::CreateDirectory($dir)
                $tag = if ($Version) { "v$Version" } else { $null }
                $which = if ($tag) { "release $tag" } else { 'latest release' }
                $api = "https://api.github.com/repos/$Repo/releases/" + $(if ($tag) { "tags/$tag" } else { 'latest' })
                $release = $null
                try {
                    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor 'Tls12'
                    $release = Invoke-RestMethod -UseBasicParsing -Uri $api
                } catch { $https = $_.Exception.Message }
                if ($release) {
                    foreach ($asset in $release.assets) {
                        if (-not ($want | Where-Object { $asset.name -like $_ })) { continue }
                        Say "downloading $($asset.name)"
                        Invoke-WebRequest -UseBasicParsing -Uri $asset.browser_download_url -OutFile (Join-Path $dir $asset.name)
                    }
                } else {
                    # A private repo answers an anonymous request with 404: then the user's own gh login.
                    if (-not (Get-Command gh -CommandType Application -ErrorAction SilentlyContinue)) {
                        Refuse ("$Repo's $which isn't reachable over HTTPS ($https). While the repo is private, install the GitHub " +
                            'CLI (https://cli.github.com), run gh auth login with the account that has access to it, and run this again')
                    }
                    Say "$which not reachable over HTTPS ($https): downloading it with gh"
                    $ghArgs = @('release', 'download') + @($tag | Where-Object { $_ }) + @('--repo', $Repo, '--dir', $dir) +
                        @($want | ForEach-Object { '--pattern'; $_ })
                    $r = Run gh @ghArgs
                    if ($r.code) {
                        Refuse "gh release download: $($r.out -join ' '). Check that gh auth login was done with the account that has access to $Repo"
                    }
                }
            }
            $sumsPath = Join-Path $dir 'SHA256SUMS.txt'
            if (-not (Test-Path -LiteralPath $sumsPath -PathType Leaf)) { Refuse "no SHA256SUMS.txt in $dir" }
            $sums = @{}
            foreach ($line in Get-Content -LiteralPath $sumsPath) {
                if ($line -match '^([0-9a-fA-F]{64}) [ *](.+)$') { $sums[$Matches[2]] = $Matches[1].ToLower() }
            }
            # Every zip this run uses is checked before any is opened.
            $assets = foreach ($pattern in $want | Where-Object { $_ -ne 'SHA256SUMS.txt' }) {
                $names = @($sums.Keys | Where-Object { $_ -like $pattern })
                if ($names.Count -ne 1) { Refuse "$sumsPath lists $($names.Count) files named like $pattern, not one" }
                $path = Join-Path $dir $names[0]
                if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { Refuse "$($names[0]) isn't in $dir" }
                if ((Sha $path) -ne $sums[$names[0]]) { Refuse "$path doesn't match SHA256SUMS.txt: a damaged or changed file" }
                $path
            }
            $quenZip = $assets | Where-Object { $_ -like '*-windows-x64.zip' }
            $w3Zip = $assets | Where-Object { (Split-Path -Leaf $_) -like 'quen-w3-*.zip' }
            $slZip = $assets | Where-Object { $_ -like '*streamline-dev-interposer-*' }
            $v = [regex]::Match((Split-Path -Leaf $quenZip), '^quen-(.+)-windows-x64\.zip$').Groups[1].Value
            if ($Version -and $v -ne $Version) { Refuse "$dir holds quen $v, not $Version" }

            $exeFile = ZipFiles $quenZip | Where-Object rel -eq 'quen.exe'
            if (-not $exeFile) { Refuse "$quenZip holds no quen.exe" }
            $newExe = Join-Path $tmp 'quen\quen.exe'
            Place $quenZip $exeFile $newExe
            $stamp = (Run $newExe --version).out -join ' '
            if ($stamp -notlike "quen $v *") { Refuse "the release's quen.exe says '$stamp', not quen $v" }
            Say "release: $stamp, from $dir"
            $doc = Idle $newExe

            # quen.exe: replaced only with -Update, stopped first if it runs.
            $hasExe = Test-Path -LiteralPath $exe
            $newQuen = -not $hasExe -or (Sha $exe) -ne $exeFile.sha
            if ($hasExe -and $newQuen -and -not $Update) {
                Refuse "quen is installed at $exe ($((Run $exe --version).out -join ' ')): run this again with -Update to replace it"
            }
            if ($hasExe -and $newQuen) { NeedsBin $exe }
            $procs = if ($newQuen) { Running } else { @() }

            # The in-game package: placed whole after quen's old package files go to the Recycle Bin.
            $package = $false
            if ($w3Zip) {
                $game = Find-Game $newExe
                $x64 = Join-Path $game 'bin\x64_dx12'
                $loader = (Test-Path -LiteralPath "$x64\dinput8.dll") -or (Test-Path -LiteralPath "$x64\winmm.dll")
                if (-not ((Test-Path -LiteralPath "$x64\WitcherNativeBridge.asi") -and $loader)) {
                    $how = ($doc.dependencies | Where-Object { $_.name -like 'Witcher Native Bridge*' }).how_to_get
                    Refuse ("the Witcher Native Bridge isn't in $x64 (WitcherNativeBridge.asi and its ASI loader, dinput8.dll or " +
                        "winmm.dll), and quen's package needs it. Install it yourself: $how")
                }
                $w3Files = @(ZipFiles $w3Zip)
                foreach ($f in $w3Files) {
                    if ($f.rel -notmatch '^(bin\\x64_dx12\\[^\\]+|Mods\\modQuen\\[^:]+)$' -or $f.rel -match '\\\.\.(\\|$)') {
                        Refuse "$w3Zip holds $($f.rel), outside quen's package layout"
                    }
                }
                $w3Name = (Split-Path -Leaf $w3Zip) -replace '^quen-w3-(.+)\.zip$', 'quen-w3 $1'
                $modFiles = @(Get-ChildItem -LiteralPath (Join-Path $game 'Mods\modQuen') -Recurse -File -ErrorAction SilentlyContinue)
                $inPlace = -not (Differs $w3Files $game) -and -not (Test-Path -LiteralPath (Join-Path $game 'Mods\modQuenRpc')) -and
                    $modFiles.Count -eq @($w3Files | Where-Object { $_.rel -like 'Mods\*' }).Count
                if ($inPlace) { Say "$w3Name is already in $game" } else {
                    # Whole mod folders, Mods first: an old modQuen's other files, or modQuenRpc, fail the game's script compile.
                    $old = @(@('Mods\modQuen', 'Mods\modQuenRpc') + @($w3Files | Where-Object { $_.rel -notlike 'Mods\*' } | ForEach-Object { $_.rel }) |
                        ForEach-Object { Join-Path $game $_ } | Where-Object { Test-Path -LiteralPath $_ })
                    $old | ForEach-Object { NeedsBin $_ }
                    $package = $true
                }
            }

            # quen's build of the Streamline dev interposer, for RenderDoc: replaced only with -Update.
            $sl = $false
            if ($slZip) {
                $slFiles = @(ZipFiles $slZip)
                if ($slFiles | Where-Object { $_.rel -match '[\\:]|\.\.' }) { Refuse "$slZip holds a file in a folder: its files go side by side" }
                $sl = [bool](Differs $slFiles $slDir)
                if ($sl -and (Test-Path -LiteralPath $slDir)) {
                    if (-not $Update) { Refuse "$slDir holds another build: run this again with -Update to replace it" }
                    NeedsBin $slDir
                }
                if (-not $sl) { Say "$slDir already holds this build" }
            }

            $path = UserPath
            $addPath = -not @($path -split ';' | Where-Object { IsBin $_ }).Count
            $claude = Find-Claude

            # The questions, all before the first change.
            Ask-Stop $procs
            if ($package) {
                Say "$w3Name, into ${game}:"
                $old | ForEach-Object { Show $_ }
                $w3Files | ForEach-Object { Write-Host "  place: $(Join-Path $game $_.rel)" }
                $package = Ask "Install quen's in-game package ($w3Name) as listed?"
            }
            $addPath = $addPath -and (Ask "Add $bin to your user PATH, so a terminal finds quen?")
            $register = $claude -and (Ask "Register quen in Claude Code for all your projects (claude mcp add --scope user quen -- $exe mcp)? It replaces a user-scope server named quen")

            try {
                if ($procs) { Stop-Quen $procs }
                if ($newQuen) {
                    if ($hasExe) { Change "move the old $exe to the Recycle Bin" { Recycle $exe } }
                    Change "place quen $v at $exe" { Place $quenZip $exeFile $exe }
                } else { Say "$exe is already quen $v" }
                if ($addPath) { Change "add $bin to your user PATH" { Set-UserPath ((@($path.TrimEnd(';')) + $bin | Where-Object { $_ }) -join ';') } }
                if ($register) {
                    Change "register quen in Claude Code: claude mcp add --scope user quen -- $exe mcp" {
                        [void](Run $claude mcp remove --scope user quen)  # a user-scope quen from before, if any
                        $r = Run $claude mcp add --scope user quen '--' $exe mcp
                        Write-Host "  $($r.out -join ' ')"
                        if ($r.code) { Say "claude mcp add failed (exit $($r.code)): register quen yourself (INSTALL.md section 4)" }
                    }
                }
                if ($package) {
                    foreach ($p in $old) { Change "move $p to the Recycle Bin" { Recycle $p } }
                    # The ASI first: the script mod is never there without it.
                    foreach ($f in $w3Files | Sort-Object { $_.rel -like 'Mods\*' }) {
                        Change "place $(Join-Path $game $f.rel)" { Place $w3Zip $f (Join-Path $game $f.rel) }
                    }
                }
                if ($sl) {
                    if (Test-Path -LiteralPath $slDir) { Change "move the old $slDir to the Recycle Bin" { Recycle $slDir } }
                    foreach ($f in $slFiles) { Change "place $(Join-Path $slDir $f.rel)" { Place $slZip $f (Join-Path $slDir $f.rel) } }
                }
            } catch {
                if ($done.Count) {
                    Say 'done before that (what it replaced is in the Recycle Bin):'
                    $done | ForEach-Object { Write-Host "  $_" }
                }
                throw
            }
            if ($slZip) { Say "quen's Streamline dev interposer is in ${slDir}: quen_setup will offer it for RenderDoc captures" }

            # What's still missing: quen installs none of it.
            $doc = QuenJson $(if ($dry) { $newExe } else { $exe }) doctor
            if (-not $doc) { Say 'quen doctor answered no JSON: run quen doctor in a terminal' }
            foreach ($required in $true, $false) {
                $missing = @($doc.dependencies | Where-Object { $_.status -ne 'found' -and [bool]$_.required -eq $required })
                if (-not $missing) { continue }
                Say ('quen doctor, ' + $(if ($required) { 'needed for every session' } else { 'optional, each for some tools' }) +
                    ': missing or the wrong version (install these yourself)')
                foreach ($d in $missing) {
                    Write-Host "  $($d.name): $($d.status)$(if ($d.note) { ", $($d.note)" })"
                    Write-Host "    get it: $($d.how_to_get)"
                }
            }
            $others = @(Get-Process quen -ErrorAction SilentlyContinue | Where-Object { $_.Path -and $_.Path -ne $exe } | ForEach-Object { $_.Path } | Sort-Object -Unique)
            foreach ($o in $others) {
                Say ("quen.exe also runs from $o. While a quen serve from there runs for the same config, this install's quen " +
                    "can't connect to it (each end checks the other is the same quen.exe): close the MCP clients that use it")
            }
            if (-not $claude) { Say "Claude Code (claude) isn't on PATH. Once it is: claude mcp add --scope user quen -- `"$exe`" mcp" }
            $json = $exe.Replace('\', '\\')
            Say 'other MCP clients (INSTALL.md section 4):'
            Write-Host ('  VS Code, in mcp.json''s "servers": "quen": {{ "type": "stdio", "command": "{0}", "args": ["mcp"] }}' -f $json)
            Write-Host ('  Cursor, in %USERPROFILE%\.cursor\mcp.json''s "mcpServers": "quen": {{ "command": "{0}", "args": ["mcp"] }}' -f $json)
            Write-Host ('  Codex CLI: run "{0}" serve --stay in a terminal first, then Codex (INSTALL.md section 4)' -f $exe)
            Say 'next: ask your agent to run quen_setup, then quen_doctor'
        } finally {
            try { Recycle $tmp } catch { Say "the temp folder $tmp stays: $($_.Exception.Message)" }
        }
    } catch {
        Write-Host $_.Exception.Message -ForegroundColor Red
        if ($_.Exception.Message -notlike 'install: refused*') { Write-Host $_.InvocationInfo.PositionMessage }
        # Run as a file, it ends with exit code 1; through iex, exit would close the user's PowerShell.
        if ($PSCommandPath) { exit 1 }
    }
}
