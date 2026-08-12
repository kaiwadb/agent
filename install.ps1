# Bootstrap for the cargo-dist PowerShell installer.
#
# GitHub release assets are served with `Content-Disposition: attachment`,
# which makes PS 5.1's `Invoke-RestMethod` return the body as bytes rather
# than a string, so the standard `irm URL | iex` one-liner breaks. This
# file lives on raw.githubusercontent.com (served as text/plain), so `irm`
# returns a string here, and we then fetch the real installer via
# WebClient.DownloadString which is content-type agnostic.
#
# Usage:
#   powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/kaiwadb/tunnel/main/install.ps1 | iex"

$ErrorActionPreference = 'Stop'
$url = 'https://github.com/kaiwadb/tunnel/releases/latest/download/kaiwadb-tunnel-installer.ps1'
Invoke-Expression ((New-Object System.Net.WebClient).DownloadString($url))
