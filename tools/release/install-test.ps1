# Tests install.ps1 on synthetic files only: a release built here around a quen.exe you give, a scratch USERPROFILE, a
# fake game folder with a fake Native Bridge, another whose package a synthetic Vortex deployed, a stand-in claude that only logs its arguments, and a stand-in process
# named witcher3.exe. Nothing touches this PC's game folder, its .quen or its Claude config; the user PATH is tested on a
# scratch registry key. Exit code 0 when every check passes.
# Usage: powershell -ExecutionPolicy Bypass -File tools\release\install-test.ps1 -Quen <quen.exe> [-Release <folder>]
#   -Release: a folder release.ps1 wrote, installed instead of the synthetic release (its quen-w3 package, its interposer).
# The scratch folder goes to the Recycle Bin when every check passes; after a failure it stays, and its path is printed.
param(
    [Parameter(Mandatory)][string]$Quen,
    [string]$Release
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem, Microsoft.VisualBasic
$install = Join-Path $PSScriptRoot 'install.ps1'
$Quen = (Resolve-Path -LiteralPath $Quen).ProviderPath
if (Get-Process witcher3 -ErrorAction SilentlyContinue) { throw 'a witcher3.exe runs on this PC: close it, the test needs a stand-in of that name' }

$root = Join-Path ([IO.Path]::GetTempPath()) ('quen-install-test-' + [guid]::NewGuid().ToString('n').Substring(0, 8))
$rel = Join-Path $root 'release'
$home_ = Join-Path $root 'home'
$game = Join-Path $root 'game'
$recycle = Join-Path $root 'recycled'  # QUEN_TEST_RECYCLE_TO: the Recycle Bin's stand-in
$shim = Join-Path $root 'shim'
$x64 = Join-Path $game 'bin\x64_dx12'
$exe = Join-Path $home_ '.quen\bin\quen.exe'
foreach ($d in $rel, $x64, $recycle, $shim, (Join-Path $home_ '.quen')) { [void][IO.Directory]::CreateDirectory($d) }
$failed = 0
$started = Get-Date

function Sha($path) { (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLower() }
function Put($path, $text) { [void][IO.Directory]::CreateDirectory((Split-Path $path)); [IO.File]::WriteAllText($path, $text) }
function Check($ok, $what) {
    if ($ok) { Write-Host "  ok   $what" } else { Write-Host "  FAIL $what" -ForegroundColor Red; $script:failed++ }
}
function Zip($path, $files) {  # [ordered] name in the zip -> source file
    $zip = [IO.Compression.ZipFile]::Open($path, 'Create')
    try { foreach ($e in $files.GetEnumerator()) { [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $e.Value, $e.Key) } }
    finally { $zip.Dispose() }
}
# A zip's files: path (with \) and sha256.
function ZipShas($path) {
    $zip = [IO.Compression.ZipFile]::OpenRead($path)
    try {
        foreach ($e in $zip.Entries | Where-Object Name) {
            $s = $e.Open()
            try { [pscustomobject]@{ rel = $e.FullName.Replace('/', '\'); sha = (Get-FileHash -Algorithm SHA256 -InputStream $s).Hash.ToLower() } }
            finally { $s.Dispose() }
        }
    } finally { $zip.Dispose() }
}
function Sums($dir) {
    $lines = Get-ChildItem -LiteralPath $dir -File | Where-Object Name -ne 'SHA256SUMS.txt' | ForEach-Object { "$(Sha $_.FullName)  $($_.Name)" }
    [IO.File]::WriteAllText((Join-Path $dir 'SHA256SUMS.txt'), ($lines -join "`n") + "`n")
}
# Every file the installer could touch, with its sha256: what "nothing changed" compares. Not in it: the test's logs, and
# each run's own temp folder, which ends in the Recycle Bin's stand-in.
function Tree([string[]]$dirs = @($home_, $game, $rel, $recycle)) {
    $files = Get-ChildItem -LiteralPath $dirs -Recurse -File -Force |
        Where-Object FullName -notmatch '\\[0-9a-f]{8}-quen-install-[0-9a-f]{32}\\'
    ($files | ForEach-Object { "$($_.FullName)=$(Sha $_.FullName)" }) -join "`n"
}
# install.ps1 in a new Windows PowerShell with this test's environment, the answers on its stdin: its output and exit code.
function Install([string[]]$answers, [string[]]$params) {
    $saved = @{}
    $vars = @{ USERPROFILE = $home_; QUEN_TEST_RECYCLE_TO = $recycle; PATH = "$shim;$env:PATH"; QUEN_CONFIG = $null; QUEN_TEST_PINS = $pins }
    foreach ($k in @($vars.Keys)) { $saved[$k] = [Environment]::GetEnvironmentVariable($k); [Environment]::SetEnvironmentVariable($k, $vars[$k]) }
    try {
        $ErrorActionPreference = 'Continue'
        $out = @(@($answers) + '' | powershell -NoProfile -ExecutionPolicy Bypass -File $install @params 2>&1 | ForEach-Object { "$_" })
        $code = $LASTEXITCODE
    } finally { foreach ($k in @($saved.Keys)) { [Environment]::SetEnvironmentVariable($k, $saved[$k]) } }
    $text = $out -join "`n"
    Add-Content -LiteralPath (Join-Path $root 'install.log') -Value "### install.ps1 $params (answers: $answers) -> exit $code`n$text`n"
    [pscustomobject]@{ out = $text; code = $code }
}
function ClaudeLog { $p = Join-Path $root 'claude.log'; if (Test-Path -LiteralPath $p) { (Get-Content -LiteralPath $p) -join "`n" } else { '' } }

# The release: synthetic, or release.ps1's.
if ($Release) {
    Get-ChildItem -LiteralPath $Release -File | Copy-Item -Destination $rel
} else {
    $src = Join-Path $root 'src'
    Put "$src\README.md" 'synthetic'
    Copy-Item -LiteralPath $Quen -Destination "$src\quen.exe"
    Put "$src\W3Quen.asi" 'synthetic W3Quen.asi'
    Put "$src\quen.ws" 'synthetic quen.ws'
    Put "$src\sl.interposer.dll" 'synthetic interposer'
    Put "$src\license.txt" 'synthetic licence'
    # Named for the given quen.exe's version, which install.ps1 checks against the zip's name.
    $given = ("$(& $Quen --version)" -split ' ')[1]
    Zip "$rel\quen-$given-windows-x64.zip" ([ordered]@{ 'quen.exe' = "$src\quen.exe"; 'README.md' = "$src\README.md" })
    Zip "$rel\quen-w3-9.9.9.zip" ([ordered]@{ 'bin/x64_dx12/W3Quen.asi' = "$src\W3Quen.asi"; 'Mods/modQuen/content/scripts/local/quen.ws' = "$src\quen.ws" })
    Zip "$rel\quen-streamline-dev-interposer-2.14.1.zip" ([ordered]@{ 'sl.interposer.dll' = "$src\sl.interposer.dll"; 'license.txt' = "$src\license.txt" })
    Copy-Item -LiteralPath $install -Destination $rel
    Sums $rel
}
$quenZip = (Get-ChildItem -LiteralPath $rel -Filter 'quen-*-windows-x64.zip').FullName
$version = $quenZip -replace '^.*quen-(.+)-windows-x64\.zip$', '$1'
$quenSha = (ZipShas $quenZip | Where-Object rel -eq 'quen.exe').sha
$pkg = @(ZipShas (Get-ChildItem -LiteralPath $rel -Filter 'quen-w3-*.zip').FullName)
# The doctor accepts the synthetic package as found.
$pins = ($pkg | ForEach-Object { "$($_.rel)=$($_.sha)" }) -join ';'

# The fake game: an exe, the Native Bridge and its loader, an old pure-script modQuen with a file the package lacks,
# and a leftover modQuenRpc. quen's config names it, so quen setup never picks this PC's own game.
Put "$x64\witcher3.exe" 'synthetic, not a game'
Put "$x64\WitcherNativeBridge.asi" 'synthetic Native Bridge'
Put "$x64\dinput8.dll" 'synthetic ASI loader'
Put "$game\Mods\modQuen\content\scripts\local\quen.ws" 'synthetic old modQuen'
Put "$game\Mods\modQuen\content\scripts\local\old_extra.ws" 'synthetic old file'
Put "$game\Mods\modQuenRpc\content\scripts\local\rpc.ws" 'synthetic old modQuenRpc'
Put "$home_\.quen\config.toml" "game_dir = '$game'`n"
Put "$shim\claude.cmd" "@echo %*>>`"$root\claude.log`"`r`n@echo synthetic claude: %*`r`n"
# Claude Code's settings with a setting and a hook of the user's own: quen's hook goes in beside them.
$settings = Join-Path $home_ '.claude\settings.json'
$userSettings = '{"model": "synthetic", "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "echo synthetic"}]}]}}'
Put $settings $userSettings
$at = @('-From', $rel, '-GameDir', $game)

Write-Host "install-test: $root (release: $(Split-Path -Leaf $quenZip))"

Write-Host '-WhatIf: every step printed, nothing changed'
$before = Tree
$r = Install @() ($at + '-WithRenderDoc', '-WhatIf')
Check ($r.code -eq 0) "exit 0 ($($r.code))"
Check ($r.out -match 'What if: place .*W3Quen\.asi' -and $r.out -match 'What if: move .*modQuenRpc to the Recycle Bin') 'the package steps are printed'
Check ($r.out -match "What if: place quen $([regex]::Escape($version)) at") 'the quen.exe step is printed'
Check ($r.out -match 'What if: register quen in Claude Code') 'the Claude Code step is printed'
Check ($r.out -match "What if: add quen's hook to .*settings\.json") 'the hook step is printed'
Check ($r.out -match 'What if: place .*tools\\streamline\\sl\.interposer\.dll') 'the interposer step is printed'
Check ($r.out -match 'next: ask your agent to run quen_setup, then quen_doctor') 'the next step is printed'
Check ((Tree) -eq $before) 'no file changed (home, game, release, Recycle Bin)'
Check (-not (ClaudeLog)) 'claude was not run'

Write-Host 'refused: a zip that does not match SHA256SUMS.txt'
$bad = Join-Path $root 'release-bad'
Copy-Item -LiteralPath $rel -Destination $bad -Recurse
[IO.File]::AppendAllText((Get-ChildItem -LiteralPath $bad -Filter 'quen-w3-*.zip').FullName, 'x')
$before = Tree
$r = Install @() @('-From', $bad, '-GameDir', $game)
Check ($r.code -ne 0 -and $r.out -match '(?s)refused, nothing was changed: .*quen-w3-.*doesn''t match SHA256SUMS\.txt') "refused ($($r.code))"
Check ((Tree) -eq $before) 'nothing changed'

Write-Host 'refused: no Native Bridge'
$game2 = Join-Path $root 'game-nownb'
Put "$game2\bin\x64_dx12\witcher3.exe" 'synthetic, not a game'
$before = Tree
$r = Install @() @('-From', $rel, '-GameDir', $game2)
Check ($r.code -ne 0 -and $r.out -match 'refused, nothing was changed: the Witcher Native Bridge isn''t in' -and $r.out -match 'Nexus Mods') "refused, with where to get it ($($r.code))"
Check ((Tree) -eq $before) 'nothing changed'

Write-Host 'refused: the game running (a stand-in named witcher3.exe)'
$standin = Join-Path $root 'standin\witcher3.exe'
[void][IO.Directory]::CreateDirectory((Split-Path $standin))
Copy-Item -LiteralPath "$env:windir\System32\PING.EXE" -Destination $standin
$p = Start-Process -FilePath $standin -ArgumentList '-n', '120', '127.0.0.1' -WindowStyle Hidden -PassThru
try {
    $before = Tree
    $r = Install @() $at
    Check ($r.code -ne 0 -and $r.out -match "refused, nothing was changed: The Witcher 3 is running \(witcher3\.exe, pid $($p.Id)\)") "refused ($($r.code))"
    Check ((Tree) -eq $before) 'nothing changed'
} finally { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue; $p.WaitForExit() }

Write-Host 'refused: a live journal'
$journal = Join-Path $home_ '.quen\state\journal.jsonl'
Put $journal "{`"synthetic`": true}`n"
$before = Tree
$r = Install @() $at
Check ($r.code -ne 0 -and $r.out -match 'refused, nothing was changed: a quen session''s journal is there') "refused ($($r.code))"
Check ((Tree) -eq $before) 'nothing changed'
Move-Item -LiteralPath $journal -Destination (Join-Path $root 'test-journal.jsonl')

Write-Host 'install: package yes, PATH no, Claude Code yes, hook yes'
$oldMod = Get-ChildItem -LiteralPath "$game\Mods" -Recurse -File | ForEach-Object Name
$r = Install @('y', 'n', 'y', 'y') ($at + '-WithRenderDoc')
Check ($r.code -eq 0) "exit 0 ($($r.code))"
Check ((Test-Path -LiteralPath $exe) -and (Sha $exe) -eq $quenSha) 'quen.exe placed, the release''s bytes'
foreach ($f in $pkg) { Check ((Sha (Join-Path $game $f.rel)) -eq $f.sha) "placed $($f.rel)" }
Check (-not (Test-Path -LiteralPath "$game\Mods\modQuenRpc") -and -not (Test-Path -LiteralPath "$game\Mods\modQuen\content\scripts\local\old_extra.ws")) 'modQuenRpc and the old modQuen are gone from the game folder'
$recycled = Get-ChildItem -LiteralPath $recycle -Recurse -File | ForEach-Object Name
Check (-not ($oldMod | Where-Object { $_ -notin $recycled })) "the old files are in the Recycle Bin stand-in ($($oldMod -join ', '))"
Check ((ClaudeLog) -match ('mcp add --scope user quen -- "?' + [regex]::Escape($exe) + '"? mcp')) 'claude mcp add --scope user quen -- <exe> mcp'
$backups = @(Get-ChildItem -LiteralPath (Split-Path $settings) -Filter 'settings.json.quen-backup-*')
Check ($backups.Count -eq 1 -and [IO.File]::ReadAllText($backups[0].FullName) -eq $userSettings) 'settings.json backed up first, as it was'
$s = [IO.File]::ReadAllText($settings) | ConvertFrom-Json
$pre = @($s.hooks.PreToolUse)
Check ($s.model -eq 'synthetic' -and $pre.Count -eq 2 -and $pre[0].matcher -eq 'Bash' -and $pre[0].hooks[0].command -eq 'echo synthetic') 'the user''s setting and hook stay'
Check ($pre[1].matcher -like '^mcp__quen__(*' -and $pre[1].hooks[0].command -eq $exe -and "$($pre[1].hooks[0].args)" -eq 'focus --to client' -and $pre[1].hooks[0].timeout -eq 5) 'quen''s hook comes last, with this quen.exe'
Check (Test-Path -LiteralPath "$home_\.quen\tools\streamline\sl.interposer.dll") 'the interposer is in .quen\tools\streamline'
Check ($r.out -match 'quen_setup will offer it') 'the interposer message'
Check ($r.out -match 'quen doctor, needed for every session' -and $r.out -notmatch 'quen in-game package: ') 'quen doctor ran, and finds the package'
$temps = Get-ChildItem -LiteralPath ([IO.Path]::GetTempPath()) -Directory | Where-Object { $_.Name -match '^quen-install-[0-9a-f]{32}$' -and $_.CreationTime -gt $started }
Check (-not $temps) 'its temp folders are gone from %TEMP%'

Write-Host 'again, no -Update, hook yes: nothing to change'
$before = Tree
$r = Install @('n', 'n', 'y') $at
Check ($r.code -eq 0 -and $r.out -match 'is already quen' -and $r.out -match 'is already in' -and $r.out -match 'quen''s hook is already in') "exit 0, already in place ($($r.code))"
Check ((Tree) -eq $before) 'nothing changed'

Write-Host 'refused: hook yes, settings.json not JSON'
$good = [IO.File]::ReadAllText($settings)
Put $settings '{"hooks": '
$before = Tree
$r = Install @('n', 'n', 'y') $at
Check ($r.code -ne 0 -and $r.out -match 'refused, nothing was changed: .*settings\.json isn''t valid JSON') "refused ($($r.code))"
Check ((Tree) -eq $before) 'nothing changed'
Put $settings $good

Write-Host 'refused: another quen.exe installed, no -Update'
$rel2 = Join-Path $root 'release-2'
[void][IO.Directory]::CreateDirectory($rel2)
Get-ChildItem -LiteralPath $rel -File | Where-Object Name -notlike 'quen-*-windows-x64.zip' | Copy-Item -Destination $rel2
$exe2 = Join-Path $root 'src2\quen.exe'
[void][IO.Directory]::CreateDirectory((Split-Path $exe2))
Copy-Item -LiteralPath $Quen -Destination $exe2
[IO.File]::AppendAllText($exe2, 'synthetic overlay')  # other bytes, the same program
Zip (Join-Path $rel2 (Split-Path -Leaf $quenZip)) ([ordered]@{ 'quen.exe' = $exe2 })
Sums $rel2
$at2 = @('-From', $rel2, '-GameDir', $game)
$before = Tree
$r = Install @() $at2
Check ($r.code -ne 0 -and $r.out -match 'run this again with -Update') "refused ($($r.code))"
Check ((Tree) -eq $before) 'nothing changed'

Write-Host '-Update with quen.exe running from .quen\bin: stop yes, PATH no, Claude Code no'
$saved = $env:USERPROFILE
$env:USERPROFILE = $home_
try { $serve = Start-Process -FilePath $exe -ArgumentList 'serve' -WindowStyle Hidden -PassThru } finally { $env:USERPROFILE = $saved }
Start-Sleep -Milliseconds 500
Check (-not $serve.HasExited) "a quen serve runs from .quen\bin (pid $($serve.Id))"
$r = Install @('y', 'n', 'n') ($at2 + '-Update')
Check ($r.code -eq 0 -and $r.out -match "stop quen\.exe \(pid $($serve.Id)\)") "stopped and updated ($($r.code))"
Check ($serve.HasExited) 'that quen serve is gone'
Check ((Sha $exe) -eq (Sha $exe2)) 'quen.exe is the new build'
Check (@(Get-ChildItem -LiteralPath $recycle -Filter '*-quen.exe').Count -eq 1) 'the old quen.exe is in the Recycle Bin stand-in'
if (-not $serve.HasExited) { Stop-Process -Id $serve.Id -Force }

Write-Host '-Uninstall -WhatIf: nothing changed'
$before = Tree
$r = Install @() @('-Uninstall', '-GameDir', $game, '-WhatIf')
Check ($r.code -eq 0 -and $r.out -match 'What if: move .*\\bin to the Recycle Bin' -and $r.out -match 'What if: remove quen from Claude Code') "the steps are printed ($($r.code))"
Check ((Tree) -eq $before) 'nothing changed'

Write-Host '-Uninstall: yes'
$r = Install @('y') @('-Uninstall', '-GameDir', $game)
Check ($r.code -eq 0) "exit 0 ($($r.code))"
Check (-not (Test-Path -LiteralPath (Split-Path $exe))) '.quen\bin is gone'
Check (-not (Test-Path -LiteralPath "$game\Mods\modQuen") -and -not (Test-Path -LiteralPath "$x64\W3Quen.asi")) 'the package is gone'
Check ((Test-Path -LiteralPath "$x64\WitcherNativeBridge.asi") -and (Test-Path -LiteralPath "$x64\witcher3.exe")) 'the Native Bridge and the game stay'
Check ((Test-Path -LiteralPath "$home_\.quen\config.toml") -and $r.out -match 'stays: your config') '.quen stays, and the message names it'
Check ((ClaudeLog) -match 'mcp remove --scope user quen') 'claude mcp remove --scope user quen'

# A game whose quen package Vortex installed: a synthetic deployment manifest lists it, and its files are hardlinks to a
# staging stand-in, as Vortex deploys. Every run leaves it, and Vortex's copy, as they were, and says why.
$gameV = Join-Path $root 'game-vortex'
$source = 'quen-w3 synthetic Vortex mod'
$staging = Join-Path $root "vortex-staging\$source"
Put "$gameV\bin\x64_dx12\witcher3.exe" 'synthetic, not a game'
Put "$gameV\bin\x64_dx12\WitcherNativeBridge.asi" 'synthetic Native Bridge'
Put "$gameV\bin\x64_dx12\dinput8.dll" 'synthetic ASI loader'
foreach ($f in 'bin\x64_dx12\W3Quen.asi', 'Mods\modQuen\content\scripts\local\quen.ws') {
    Put "$staging\$f" "synthetic Vortex copy of $f"
    [void][IO.Directory]::CreateDirectory((Split-Path "$gameV\$f"))
    [void](New-Item -ItemType HardLink -Path "$gameV\$f" -Target "$staging\$f")
}
$manifest = "$gameV\vortex.deployment.witcher3tl.json"
$manifestText = ConvertTo-Json -Depth 5 ([ordered]@{
        instance = 'synthetic'; deploymentMethod = 'hardlink_activator'; stagingPath = (Split-Path $staging)
        files = @('bin\x64_dx12\W3Quen.asi', 'Mods\modQuen\content\scripts\local\quen.ws' | ForEach-Object { [ordered]@{ relPath = $_; source = $source; target = ''; time = 1 } })
    })
Put $manifest $manifestText
$vortexDirs = $gameV, (Split-Path $staging)
function Recycled { @(Get-ChildItem -LiteralPath $recycle -Name | Where-Object { $_ -notmatch '-quen-install-[0-9a-f]{32}$' }) }
$says = "quen's in-game package in .*game-vortex is Vortex's \($([regex]::Escape($source))\): update it in Vortex"

Write-Host 'Vortex''s package, install: quen.exe placed, the package left'
$beforeV = Tree $vortexDirs
$binBefore = Recycled
$r = Install @('n', 'n', 'n') @('-From', $rel, '-GameDir', $gameV)
Check ($r.code -eq 0 -and $r.out -match $says) "exit 0, update it in Vortex ($($r.code))"
Check ((Sha $exe) -eq $quenSha) 'quen.exe placed'
Check ((Tree $vortexDirs) -eq $beforeV -and ((Recycled) -join '|') -eq ($binBefore -join '|')) 'the package and Vortex''s copy as they were, nothing recycled'

Write-Host 'Vortex''s package, -Update: quen.exe replaced, the package left'
$r = Install @('n', 'n', 'n') @('-From', $rel2, '-GameDir', $gameV, '-Update')
Check ($r.code -eq 0 -and $r.out -match $says) "exit 0, update it in Vortex ($($r.code))"
Check ((Sha $exe) -eq (Sha $exe2)) 'quen.exe is the new build'
$new = @(Recycled | Where-Object { $_ -notin $binBefore })
Check ((Tree $vortexDirs) -eq $beforeV -and $new.Count -eq 1 -and $new[0] -like '*-quen.exe') "the package as it was, only the old quen.exe recycled ($new)"

Write-Host 'refused: a Vortex manifest that does not parse'
Put $manifest '{"files": '
$before = Tree ($vortexDirs + $home_, $recycle)
$r = Install @() @('-From', $rel2, '-GameDir', $gameV)
Check ($r.code -ne 0 -and $r.out -match 'refused, nothing was changed: .*vortex\.deployment\.witcher3tl\.json doesn''t parse') "refused ($($r.code))"
Check ((Tree ($vortexDirs + $home_, $recycle)) -eq $before) 'nothing changed'

Write-Host 'Vortex''s hardlinks, no manifest: the package left'
[IO.File]::Delete($manifest)  # this test's own file
$before = Tree ($vortexDirs + $home_, $recycle)
$r = Install @('n', 'n', 'n') @('-From', $rel2, '-GameDir', $gameV)
Check ($r.code -eq 0 -and $r.out -match "quen's in-game package in .*game-vortex is Vortex's \(a hardlink\): update it in Vortex") "exit 0, update it in Vortex ($($r.code))"
Check ((Tree ($vortexDirs + $home_, $recycle)) -eq $before) 'nothing changed'

Write-Host 'Vortex''s package, -Uninstall: yes'
Put $manifest $manifestText
$beforeV = Tree $vortexDirs
$r = Install @('y') @('-Uninstall', '-GameDir', $gameV)
Check ($r.code -eq 0 -and $r.out -match "stays: quen's in-game package in .*game-vortex, which is Vortex's \($([regex]::Escape($source))\): uninstall it in Vortex") "exit 0, uninstall it in Vortex ($($r.code))"
Check (-not (Test-Path -LiteralPath (Split-Path $exe))) '.quen\bin is gone'
Check ((Tree $vortexDirs) -eq $beforeV) 'the package and Vortex''s copy as they were'

Write-Host 'the user PATH, on a scratch registry key'
$ast = [Management.Automation.Language.Parser]::ParseFile($install, [ref]$null, [ref]$null)
$ast.FindAll({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -in 'UserPath', 'IsBin', 'Set-UserPath' }, $true) |
    ForEach-Object { . ([scriptblock]::Create($_.Extent.Text)) }
$envKey = 'Software\quen-install-test-' + [guid]::NewGuid().ToString('n').Substring(0, 8)  # what those functions read
$bin = Join-Path $home_ '.quen\bin'
try {
    $k = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($envKey)
    $k.SetValue('Path', '%SystemRoot%\system32;C:\synthetic', 'ExpandString'); $k.Close()
    $path = UserPath
    Check (-not @($path -split ';' | Where-Object { IsBin $_ }).Count) 'not on it yet'
    Set-UserPath ((@($path.TrimEnd(';')) + $bin | Where-Object { $_ }) -join ';')
    $k = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey($envKey)
    Check ($k.GetValue('Path', '', 'DoNotExpandEnvironmentNames') -eq "%SystemRoot%\system32;C:\synthetic;$bin" -and $k.GetValueKind('Path') -eq 'ExpandString') 'added, %SystemRoot% kept unexpanded'
    $k.Close()
    $path = UserPath
    Check (@($path -split ';' | Where-Object { IsBin $_ }).Count -eq 1) 'found on it'
    Set-UserPath (@($path -split ';' | Where-Object { -not (IsBin $_) }) -join ';')
    Check ((UserPath) -eq '%SystemRoot%\system32;C:\synthetic') 'taken out again, the rest as it was'
} finally { [Microsoft.Win32.Registry]::CurrentUser.DeleteSubKeyTree($envKey, $false) }

Write-Host 'quen''s hook in Claude Code''s settings, on synthetic files'
$ast.FindAll({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -in 'Refuse', 'With-Hook' }, $true) |
    ForEach-Object { . ([scriptblock]::Create($_.Extent.Text)) }
$f = Join-Path $root 'hook\settings.json'
$quenAt = 'C:\synthetic\quen.exe'
function Quens($text) { @(($text | ConvertFrom-Json).hooks.PreToolUse | ForEach-Object { $_.hooks } | Where-Object { "$($_.args)" -eq 'focus --to client' }) }
Check (-not (Test-Path -LiteralPath $f) -and @(Quens (With-Hook $f $quenAt)).Count -eq 1) 'no file: the hook alone'
Put $f "  `r`n"
Check (@(Quens (With-Hook $f $quenAt)).Count -eq 1) 'an empty file: the hook alone'
Put $f ('{"env": {"A": "1"}, "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "echo synthetic"}, ' +
    '{"type": "command", "command": "C:\\old\\quen.exe", "args": ["focus", "--to", "client"]}]}], "Stop": [{"hooks": [{"type": "command", "command": "echo stop"}]}]}}')
$t = With-Hook $f $quenAt
$s = $t | ConvertFrom-Json
Check ($s.env.A -eq '1' -and $s.hooks.Stop[0].hooks[0].command -eq 'echo stop' -and $s.hooks.PreToolUse[0].matcher -eq 'Bash' -and
    @($s.hooks.PreToolUse[0].hooks).Count -eq 1 -and ($s.hooks.PSObject.Properties.Name -join ',') -eq 'PreToolUse,Stop') 'existing hooks: the others stay in their order, the old quen hook leaves its group'
Check (@(Quens $t).Count -eq 1 -and @(Quens $t)[0].command -eq $quenAt) 'a changed quen path: one hook, with the new path'
Put $f $t
Check ($null -eq (With-Hook $f $quenAt)) 'already there: nothing to write'
foreach ($bad in '{"hooks": ', '[1, 2]', '{"hooks": []}') {
    Put $f $bad
    $refused = try { [void](With-Hook $f $quenAt); '' } catch { $_.Exception.Message }
    Check ($refused -like 'install: refused, nothing was changed: *' -and [IO.File]::ReadAllText($f) -eq $bad) "refused, the file as it was: $bad"
}

if ($failed) { Write-Host "install-test: $failed failed. Kept for a look: $root (install.log has every run)" -ForegroundColor Red; exit 1 }
[Microsoft.VisualBasic.FileIO.FileSystem]::DeleteDirectory($root, 'OnlyErrorDialogs', 'SendToRecycleBin')
Write-Host 'install-test: all passed (the scratch folder went to the Recycle Bin)'
