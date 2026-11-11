# Publishes a hyalo release from this machine, as .github/workflows/release.yml
# does on GitHub's: for a tag that names the workspace version it builds the
# release hyalo.exe from the commit checked out, writes hyalo.exe.sha256
# beside it (as sha256sum writes it), and creates the GitHub Release with
# both. The release comes first and creates its tag on that commit; the
# workflow the tag starts finds the release out and publishes nothing.
#
#   powershell -ExecutionPolicy Bypass -File scripts/release.ps1 -Tag v0.0.2 -NotesFile notes.md -DryRun
#   powershell -ExecutionPolicy Bypass -File scripts/release.ps1 -Tag v0.0.2 -NotesFile notes.md
#
# -DryRun does everything but talk to GitHub: the checks, the build, the
# assets in target\release-dist, and what it would publish. Without
# -NotesFile the notes say "hyalo <version>".
param(
    [Parameter(Mandatory = $true)][string]$Tag,
    [string]$NotesFile,
    [switch]$DryRun
)
$ErrorActionPreference = "Stop"
Set-Location (Split-Path -Parent $PSScriptRoot)

function Fail([string]$why) {
    [Console]::Error.WriteLine("release: $why")
    exit 1
}

# The tag names the workspace version: the version the exe knows it is.
$metadata = cargo metadata --no-deps --format-version 1 | ConvertFrom-Json
$version = ($metadata.packages | Where-Object name -eq 'hyalo').version
if ("v$version" -cne $Tag) {
    Fail "the tag $Tag does not name the workspace version ${version}: set the version in Cargo.toml, or release v${version}"
}

# What is built is what is committed, on GitHub already.
if (git status --porcelain) { Fail "the working tree has changes: commit them or set them aside" }
$commit = (git rev-parse HEAD).Trim()
$pushed = git branch --remotes --contains $commit
if (-not $pushed) {
    if (-not $DryRun) { Fail "$commit is not on GitHub yet: push it first" }
    "note: $commit is not on any remote branch as last fetched; push it before the real run"
}

$notes = Join-Path (Get-Location) "target\release-notes.md"
New-Item -ItemType Directory -Force target | Out-Null
if ($NotesFile) {
    if (-not (Test-Path $NotesFile)) { Fail "no notes file $NotesFile" }
    Copy-Item $NotesFile $notes
} else {
    [IO.File]::WriteAllText($notes, "hyalo $version`n")
}

cargo build --release --locked -p hyalo
if ($LASTEXITCODE -ne 0) { Fail "the build failed" }

# The assets, named as the updater downloads them.
$dist = Join-Path (Get-Location) "target\release-dist"
if (Test-Path $dist) { Remove-Item -Recurse -Force $dist }
New-Item -ItemType Directory -Force $dist | Out-Null
$exe = Join-Path $dist "hyalo.exe"
$sum = Join-Path $dist "hyalo.exe.sha256"
Copy-Item target\release\hyalo.exe $exe
$hash = (Get-FileHash $exe -Algorithm SHA256).Hash.ToLowerInvariant()
# As sha256sum writes it, in ASCII without a byte order mark.
[IO.File]::WriteAllText($sum, "$hash  hyalo.exe`n")
$written = (Get-Content $sum -Raw).Split(" ")[0]
if ($written -ne $hash) { Fail "the sum written does not read back" }
"hyalo $version at $commit"
"  $exe ($((Get-Item $exe).Length) bytes)"
"  $sum`: $hash"

if ($DryRun) {
    "dry run: would run gh release view $Tag, then"
    "  gh release create $Tag $exe $sum --target $commit --title `"hyalo $version`" --notes-file $notes"
    exit 0
}

# gh says "release not found" on its error output, and Windows PowerShell
# 5.1 stops on that while errors stop the script.
$ErrorActionPreference = "Continue"
gh release view $Tag *> $null
$out = $LASTEXITCODE -eq 0
$ErrorActionPreference = "Stop"
if ($out) { Fail "the release $Tag is out already" }
gh release create $Tag $exe $sum --target $commit --title "hyalo $version" --notes-file $notes
if ($LASTEXITCODE -ne 0) { Fail "gh could not create the release" }
git fetch --quiet origin "refs/tags/${Tag}:refs/tags/${Tag}"
"published $Tag; the tag is on GitHub and here"
