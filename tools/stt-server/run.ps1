# SPDX-License-Identifier: MIT
# Part of Voice Command AI (VCA). See the LICENSE file in the repository root.
#
# Windows (native): starts the speech-to-text server from an install folder
# with the same layout as install.sh creates (app\, venv\, data\, hf\, stt.env).
#
#   powershell -ExecutionPolicy Bypass -File C:\stt-server\run.ps1
#
# Settings come from stt.env (VCA_STT_* lines), then the environment.
$ErrorActionPreference = "Stop"
$Dir = $PSScriptRoot

$EnvFile = Join-Path $Dir "stt.env"
if (Test-Path $EnvFile) {
    foreach ($line in Get-Content $EnvFile) {
        $t = $line.Trim()
        if ($t -eq "" -or $t.StartsWith("#") -or -not $t.Contains("=")) { continue }
        $name, $value = $t.Split("=", 2)
        [Environment]::SetEnvironmentVariable($name.Trim(), $value.Trim(), "Process")
    }
}
if (-not $env:VCA_STT_HOME) { $env:VCA_STT_HOME = Join-Path $Dir "data" }
if (-not $env:HF_HOME) { $env:HF_HOME = Join-Path $Dir "hf" }

# CUDA libraries installed with pip (nvidia-cublas-cu12, nvidia-cudnn-cu12) ship DLLs.
$Nvidia = Join-Path $Dir "venv\Lib\site-packages\nvidia"
if (Test-Path $Nvidia) {
    $bins = Get-ChildItem -Path $Nvidia -Directory | ForEach-Object { Join-Path $_.FullName "bin" } |
        Where-Object { Test-Path $_ }
    if ($bins) { $env:PATH = ($bins -join ";") + ";" + $env:PATH }
}

Set-Location (Join-Path $Dir "app")
& (Join-Path $Dir "venv\Scripts\python.exe") -m vca_stt_server
exit $LASTEXITCODE
