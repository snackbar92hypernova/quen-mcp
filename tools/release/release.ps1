# Packs quen-mcp's HEAD into the tester release assets. Never publishes: no tag, no upload.
# Usage: powershell -ExecutionPolicy Bypass -File tools\release\release.ps1 -Version <x.y.z> -OutDir <dir> -StreamlineDll <path>
# Every check runs before OutDir is written: a refusal leaves it as it was (cargo's and pack.py's build output aside).
param(
    [Parameter(Mandatory)][string]$Version,
    [Parameter(Mandatory)][string]$OutDir,
    [Parameter(Mandatory)][string]$StreamlineDll
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression.FileSystem

# The interposer build this release ships (tools/streamline): a new build is a new pin here.
$SlVersion = '2.14.1'
$SlSha256 = 'a0483eb3de9063731f0e85e4e9b0f38e9884a33651a53c7b1e07f1d7822b6e24'
# Streamline's 3rd-party-licenses.md at that tag. Not committed: an author's email in it trips the privacy scan.
$SlNoticeSha256 = 'cb251639994465f31d2178c16acedf1ff4f9cef1c370a09782ed36d4c86c1dca'

function Sha($path) { (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash.ToLower() }
# Runs a program, its output to the console. Windows PowerShell turns a program's stderr (cargo's progress) into
# errors, which 'Stop' would throw on: hence the local Continue and the exit code check.
function Native {
    $ErrorActionPreference = 'Continue'
    $exe, $rest = $args
    & $exe @rest 2>&1 | ForEach-Object { Write-Host $_ }
    if ($LASTEXITCODE) { throw "release: $($args -join ' ') failed (exit $LASTEXITCODE)" }
}

$repo = [IO.Path]::GetFullPath((git -C $PSScriptRoot rev-parse --show-toplevel))
# A worktree sits inside the main checkout: OutDir may be in neither.
$main = Split-Path ([IO.Path]::GetFullPath((git -C $repo rev-parse --path-format=absolute --git-common-dir)))
$out = [IO.Path]::GetFullPath([IO.Path]::Combine((Get-Location).Path, $OutDir))
foreach ($root in $repo, $main) {
    if (($out.TrimEnd('\') + '\').StartsWith($root.TrimEnd('\') + '\', 'OrdinalIgnoreCase')) {
        throw "release: OutDir $out is inside the repo ($root): release output never goes in the source tree (AGENTS.md)"
    }
}
if ((Test-Path -LiteralPath $out) -and (Get-ChildItem -LiteralPath $out -Force | Select-Object -First 1)) {
    throw "release: OutDir $out is not empty: give a new or empty folder"
}
if (git -C $repo status --porcelain) { throw "release: $repo has uncommitted changes: a release is built from a commit" }
$crate = [regex]::Match((Get-Content -Raw -LiteralPath "$repo\crates\quen\Cargo.toml"), '(?m)^version = "([^"]+)"').Groups[1].Value
if ($crate -ne $Version) { throw "release: -Version $Version is not crates/quen's version ($crate)" }
if (-not (Test-Path -LiteralPath $StreamlineDll -PathType Leaf)) { throw "release: no file at -StreamlineDll $StreamlineDll" }
if ((Sha $StreamlineDll) -ne $SlSha256) { throw "release: $StreamlineDll is not the pinned interposer build ($SlSha256)" }
if (-not $env:WNB_SDK) { throw "release: set WNB_SDK to the Witcher Native Bridge SDK folder (w3/README.md)" }
foreach ($v in 'JAVA_HOME', 'FLEX_HOME', 'REDKIT') {
    if (-not [Environment]::GetEnvironmentVariable($v)) { throw "release: set $v for w3\hud.py, the HUD line's bundle (w3/README.md)" }
}
# Shipped files must not name this PC or its user: the user and machine names, and the hooks' deny-list.
# The hooks' order (.githooks/deny-scan): QUEN_DENY_LIST, then %USERPROFILE%\.quen, then the pre-move %APPDATA%\quen.
$denyList = if ($env:QUEN_DENY_LIST) { $env:QUEN_DENY_LIST }
    elseif (Test-Path -LiteralPath "$env:USERPROFILE\.quen\deny.txt") { "$env:USERPROFILE\.quen\deny.txt" }
    else { "$env:APPDATA\quen\deny.txt" }
if (-not (Test-Path -LiteralPath $denyList)) { throw "release: no deny-list at $denyList (AGENTS.md)" }
$needles = @($env:USERNAME, $env:COMPUTERNAME) + @(Get-Content -LiteralPath $denyList | Where-Object { $_ -notmatch '^\s*(#|$)' }) |
    ForEach-Object { $_.Trim().ToLowerInvariant() }
$licence = Get-ChildItem -LiteralPath $repo -File | Where-Object Name -Match '^(LICEN[CS]E|COPYING)'
if (-not $licence) { Write-Warning 'no licence file in the repo yet (the maintainer picks one): the quen zip ships without one' }
$notice = "$repo\target\dist\3rd-party-licenses.md"
New-Item -ItemType Directory -Force -Path (Split-Path $notice) | Out-Null
Native curl.exe -sSfL -o $notice "https://raw.githubusercontent.com/NVIDIA-RTX/Streamline/v$SlVersion/3rd-party-licenses.md"
if ((Sha $notice) -ne $SlNoticeSha256) { throw "release: Streamline's 3rd-party-licenses.md isn't v$SlVersion's ($SlNoticeSha256)" }

$head = git -C $repo rev-parse --short=12 HEAD
# Panic messages carry source paths, which name the build machine: map them to neutral names. The encoded form keeps
# paths with spaces whole; its own target dir keeps these flags from rebuilding the everyday build.
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { "$env:USERPROFILE\.cargo" }
$env:CARGO_ENCODED_RUSTFLAGS = @("--remap-path-prefix=$env:USERPROFILE=~", "--remap-path-prefix=$cargoHome=cargo",
    "--remap-path-prefix=$repo=quen") -join [char]0x1f
try { Native cargo build --release --locked -p quen --target-dir "$repo\target\dist" }
finally { Remove-Item Env:CARGO_ENCODED_RUSTFLAGS }
$exe = "$repo\target\dist\release\quen.exe"
$stamp = & $exe --version
if ($stamp -ne "quen $Version ($head)") { throw "release: quen --version says '$stamp', not 'quen $Version ($head)'" }

# The Rust libraries built into quen.exe: every non-dev dependency of the quen crate in Cargo.lock, for Windows, each
# with its licence expression and its packaged licence files' text. THIRD-PARTY-NOTICES.txt, beside quen.exe.
$thirdParty = "$repo\target\dist\THIRD-PARTY-NOTICES.txt"
$json = & cargo metadata --format-version 1 --locked --manifest-path "$repo\Cargo.toml" --filter-platform x86_64-pc-windows-msvc
if ($LASTEXITCODE) { throw 'release: cargo metadata failed' }
$meta = ($json -join "`n") | ConvertFrom-Json
$byId = @{}; foreach ($p in $meta.packages) { $byId[$p.id] = $p }
$nodes = @{}; foreach ($n in $meta.resolve.nodes) { $nodes[$n.id] = $n }
$seen = @{}
$todo = [Collections.Generic.Queue[string]]::new()
$todo.Enqueue(($meta.packages | Where-Object name -eq 'quen').id)
while ($todo.Count) {
    foreach ($d in $nodes[$todo.Dequeue()].deps) {
        if (-not $seen[$d.pkg] -and ($d.dep_kinds | Where-Object { $_.kind -ne 'dev' })) { $seen[$d.pkg] = $true; $todo.Enqueue($d.pkg) }
    }
}
$crates = @($seen.Keys | Where-Object { $_ -notin $meta.workspace_members } | ForEach-Object { $byId[$_] } |
    Sort-Object name, version)
$sb = [Text.StringBuilder]::new()
[void]$sb.AppendLine("Third-party notices for quen $Version ($head)").AppendLine()
[void]$sb.AppendLine('quen itself is licensed under MIT OR Apache-2.0 (LICENSE-MIT, LICENSE-APACHE). quen.exe also contains the Rust')
[void]$sb.AppendLine('standard library (MIT OR Apache-2.0, https://github.com/rust-lang/rust) and the libraries below, each under its')
[void]$sb.AppendLine("own licence: $($crates.Count) crates, with the licence files each one ships.").AppendLine()
foreach ($p in $crates) {
    [void]$sb.AppendLine(('=' * 100)).AppendLine("$($p.name) $($p.version): $($p.license)")
    $files = @(Get-ChildItem -LiteralPath (Split-Path -LiteralPath $p.manifest_path) -File |
        Where-Object Name -Match '^(LICEN[CS]E|COPYING|NOTICE|COPYRIGHT|UNLICENSE)' | Sort-Object Name)
    if (-not $files) { [void]$sb.AppendLine("(the crate ships no licence file; its licence: $($p.license))").AppendLine() }
    foreach ($f in $files) {
        [void]$sb.AppendLine(('-' * 40) + " $($f.Name)").AppendLine([IO.File]::ReadAllText($f.FullName).TrimEnd()).AppendLine()
    }
}
[IO.File]::WriteAllText($thirdParty, $sb.ToString(), [Text.UTF8Encoding]::new($false))

Native python -I "$repo\w3\pack.py"
# The release W3Quen.cpp names (kVersion), as pack.py reads it.
$w3Version = [regex]::Match((Get-Content -Raw -LiteralPath "$repo\w3\asi\W3Quen.cpp"), 'kVersion\[\] = "([0-9.]+)"').Groups[1].Value
if (-not $w3Version) { throw "release: no kVersion in w3\asi\W3Quen.cpp" }
$w3Zip = "$repo\w3\out\quen-w3-$w3Version.zip"
# Both files must be the build the doctor accepts as this version, or testers' doctors would call it the wrong one.
$pinned = [regex]::Matches((Get-Content -Raw -LiteralPath "$repo\crates\quen\src\doctor.rs"),
    'version: "quen-w3 ' + [regex]::Escape($w3Version) + '",\s*sha256: "([0-9a-f]{64})"') | ForEach-Object { $_.Groups[1].Value }
$zip = [IO.Compression.ZipFile]::OpenRead($w3Zip)
try {
    # The four pinned files (W3Quen.asi, quen.ws and, from 0.1.16, the HUD line's blob0.bundle and metadata.store); the
    # zip also holds W3Quen-licences.txt (from 0.1.3) and modQuen's README.txt (from 0.1.16), which the doctor doesn't pin.
    $built = foreach ($entry in $zip.Entries | Where-Object { $_.Name -match '\.(asi|ws|bundle|store)$' }) {
        $s = $entry.Open()
        try { (Get-FileHash -Algorithm SHA256 -InputStream $s).Hash.ToLower() } finally { $s.Dispose() }
    }
} finally { $zip.Dispose() }
if (@($pinned).Count -ne 4 -or @($built).Count -ne 4 -or ($built | Where-Object { $_ -notin $pinned })) {
    throw "release: quen-w3 $w3Version's files ($built) are not the doctor's pins ($pinned, crates/quen/src/doctor.rs)"
}

$quenZip = "quen-$Version-windows-x64.zip"
$slZip = "quen-streamline-dev-interposer-$SlVersion.zip"
$packs = [ordered]@{  # zip: [ordered] name in the zip -> source file
    $quenZip = [ordered]@{ 'quen.exe' = $exe; 'README.md' = "$repo\README.md"; 'INSTALL.md' = "$repo\INSTALL.md";
        'SECURITY.md' = "$repo\SECURITY.md" }
    $slZip = [ordered]@{ 'sl.interposer.dll' = $StreamlineDll; 'license.txt' = "$repo\tools\streamline\LICENSE-Streamline.txt"
        '3rd-party-licenses.md' = $notice; 'README.md' = "$PSScriptRoot\streamline-README.md" }
}
foreach ($l in $licence) { $packs[$quenZip][$l.Name] = $l.FullName }
$packs[$quenZip]['THIRD-PARTY-NOTICES.txt'] = $thirdParty
# pack.py stores quen-w3's files uncompressed, so its zip is scanned whole. Text as UTF-8 and UTF-16 at both alignments.
# ponytail: reads each file whole; fine at these sizes (a few MB).
$leaks = foreach ($file in @($packs.Values | ForEach-Object { $_.Values }) + $w3Zip + "$PSScriptRoot\install.ps1") {
    $b = [IO.File]::ReadAllBytes($file)
    $views = @([Text.Encoding]::UTF8.GetString($b), [Text.Encoding]::Unicode.GetString($b),
        [Text.Encoding]::Unicode.GetString($b, 1, $b.Length - 1)) | ForEach-Object { $_.ToLowerInvariant() }
    $hits = @($needles | Where-Object { $n = $_; $views | Where-Object { $_.Contains($n) } }).Count
    if ($hits) { "$file ($hits of the names)" }
}
if ($leaks) { throw "release: personal data (user or machine name, deny-list) in: $($leaks -join '; '). Nothing was packed." }

New-Item -ItemType Directory -Force -Path $out | Out-Null
foreach ($name in $packs.Keys) {
    $zip = [IO.Compression.ZipFile]::Open("$out\$name", 'Create')
    try {
        foreach ($entry in $packs[$name].GetEnumerator()) {
            [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $entry.Value, $entry.Key, 'Optimal')
        }
    } finally { $zip.Dispose() }
}
Copy-Item -LiteralPath $w3Zip, "$PSScriptRoot\install.ps1" -Destination $out
$assets = $quenZip, (Split-Path -Leaf $w3Zip), $slZip, 'install.ps1'
$sums = foreach ($name in $assets) { "$(Sha "$out\$name")  $name" }
[IO.File]::WriteAllText("$out\SHA256SUMS.txt", ($sums -join "`n") + "`n")

# git prints UTF-8; Windows PowerShell would read it in the console's code page.
$tag = git -C $repo tag --merged HEAD --sort=-creatordate | Select-Object -First 1
$encoding = [Console]::OutputEncoding
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$log = git -C $repo log --first-parent --format=%s $(if ($tag) { "$tag..HEAD" } else { 'HEAD' })
[Console]::OutputEncoding = $encoding
$changes = $log | ForEach-Object { '- ' + ($_ -replace '^Merge (claude/\S+: )?', '') }
$since = if ($tag) { "Changes since $tag." } else { 'The first tester release.' }
$notes = @"
<!-- Draft by tools/release/release.ps1 from git log: rewrite the changes in plain words for testers before publishing. -->
# quen $Version

$since Built from commit ${head}: ``quen --version`` prints ``$stamp``. Paste that into bug reports.

## Downloads

| File | What it is |
|---|---|
| ``$quenZip`` | quen itself: ``quen.exe``, README, INSTALL and SECURITY. Start with INSTALL.md. |
| ``$(Split-Path -Leaf $w3Zip)`` | quen's in-game package for The Witcher 3 (``W3Quen.asi``, the licences of the code compiled into it, and the modQuen script). Its folders mirror the game folder: unzip it there. |
| ``$slZip`` | Optional, for RenderDoc captures only: a build of NVIDIA Streamline $SlVersion's interposer that RenderDoc can see. Not made or endorsed by NVIDIA. See its README. |
| ``install.ps1`` | The installer (INSTALL.md, Quick install): it downloads the latest release, or the one ``-Version`` names, checks it against its ``SHA256SUMS.txt``, and installs quen and its in-game package. |
| ``SHA256SUMS.txt`` | The sha256 of each zip and of ``install.ps1``. In PowerShell: ``Get-FileHash <file>``, and compare. |

## Changes

$($changes -join "`n")
"@
[IO.File]::WriteAllText("$out\RELEASE-NOTES.md", $notes.Replace("`r`n", "`n"))

Write-Host "release: $out"
$sums | ForEach-Object { Write-Host "  $_" }
