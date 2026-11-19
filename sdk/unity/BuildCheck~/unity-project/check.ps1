# Builds the package in a Unity editor with no manual steps: check.ps1 <path to Unity.exe> [web]
# One line per check on the output (AURIX <check> PASS|FAIL ...), the editor logs under Logs/, exit code 1 if any failed.
param([Parameter(Mandatory = $true)][string]$Unity, [string]$Target = "")
Set-Location $PSScriptRoot
New-Item -ItemType Directory -Force -Path Logs | Out-Null
$checks = @("Compile", "BuildPlayer")
if ($Target -eq "web") { $checks += "BuildWeb" }
# The editor upgrades a project of another version, or of none, and adds to its manifest the
# packages and built-in modules that came since: the project names the editor that opens it.
$version = [regex]::Match((& $Unity -version 2>$null | Out-String), "[0-9]+\.[0-9]+\.[0-9]+[a-z][0-9]+").Value
if (-not $version) { "AURIX Version FAIL $Unity -version printed no editor version"; exit 1 }
New-Item -ItemType Directory -Force -Path ProjectSettings | Out-Null
Set-Content -Path ProjectSettings/ProjectVersion.txt -Value "m_EditorVersion: $version"
# A lock file another editor left would keep its versions of what the package names.
Remove-Item Packages/packages-lock.json -ErrorAction SilentlyContinue
Copy-Item Packages/manifest.json Logs/manifest.json -Force
$failed = 0
foreach ($check in $checks) {
    $log = "Logs/$check.log"
    $run = Start-Process -FilePath $Unity -Wait -PassThru -ArgumentList @("-batchmode", "-nographics", "-quit", "-projectPath", ".", "-executeMethod", "AurixBuildCheck.$check", "-logFile", $log)
    # first of all: a check that ran with packages the editor added says nothing about the package
    $added = Compare-Object (Get-Content Logs/manifest.json) (Get-Content Packages/manifest.json)
    if ($added) {
        "AURIX Manifest FAIL the editor changed Packages/manifest.json, $check is not judged: the project held more than the package names"
        $added | Select-Object -First 20 | ForEach-Object { "$($_.SideIndicator) $($_.InputObject)" }
        Copy-Item Logs/manifest.json Packages/manifest.json -Force
        exit 1
    }
    $lines = Select-String -Path $log -Pattern "^AURIX " | ForEach-Object { $_.Line }
    if ($lines) { $lines } else { "AURIX $check FAIL the editor left with code $($run.ExitCode) before the check ran: see $log" }
    Select-String -Path $log -Pattern "error CS|Scripts have compiler errors" | Select-Object -First 20 | ForEach-Object { $_.Line }
    if ($run.ExitCode -ne 0) { $failed = 1 }
}
exit $failed
