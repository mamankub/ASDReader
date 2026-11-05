<#
.SYNOPSIS
  Adds or removes the "ASD PDF Printer" print queue.

.DESCRIPTION
  The queue uses Windows' own "Microsoft Print To PDF" driver (nothing is bundled or signed here) on
  a TCP port at 127.0.0.1:19100. The driver turns whatever is printed into a PDF and the spooler sends
  it to that port, where `ASDReader.exe --printer-listener` (started at sign-in) repairs the Thai text
  layer, asks where to save the file (a Save dialog) and opens it.

  Run by the installer (as SYSTEM) and usable by hand as an administrator:
    powershell -NoProfile -ExecutionPolicy Bypass -File asd-pdf-printer.ps1 -Install
    powershell -NoProfile -ExecutionPolicy Bypass -File asd-pdf-printer.ps1 -Remove

  Both are idempotent and never fail the installer: a problem is written to
  %ProgramData%\ASDReader\printer-install.log and the exit code stays 0 unless -Strict is given
  (the CI test uses it).
#>
param(
  [switch] $Install,
  [switch] $Remove,
  [switch] $Strict,
  [string] $Name = 'ASD PDF Printer',
  [string] $PortName = 'ASDPDF:',
  [int] $Port = 19100
)
$ErrorActionPreference = 'Stop'
$Driver = 'Microsoft Print To PDF'
$LogDir = Join-Path $env:ProgramData 'ASDReader'
function Write-Log([string] $Message) {
  Write-Output $Message
  try {
    New-Item -ItemType Directory -Force -Path $LogDir | Out-Null
    Add-Content -Path (Join-Path $LogDir 'printer-install.log') -Value "$(Get-Date -Format s) $Message"
  } catch { }
}

try {
  if ($Install -eq $Remove) { throw 'give exactly one of -Install or -Remove' }
  if ($Remove) {
    if (Get-Printer -Name $Name -ErrorAction SilentlyContinue) { Remove-Printer -Name $Name; Write-Log "removed the $Name queue" }
    if (Get-PrinterPort -Name $PortName -ErrorAction SilentlyContinue) { Remove-PrinterPort -Name $PortName; Write-Log "removed the $PortName port" }
  }
  else {
    # The driver ships with Windows 10 and 11 (optional feature Printing-PrintToPDFServices-Features).
    if (-not (Get-PrinterDriver -Name $Driver -ErrorAction SilentlyContinue)) {
      Enable-WindowsOptionalFeature -Online -FeatureName 'Printing-PrintToPDFServices-Features' -NoRestart -ErrorAction SilentlyContinue | Out-Null
    }
    if (-not (Get-PrinterDriver -Name $Driver -ErrorAction SilentlyContinue)) {
      throw "the '$Driver' driver is not available on this computer (Windows 10 or later is needed)"
    }
    if (-not (Get-PrinterPort -Name $PortName -ErrorAction SilentlyContinue)) {
      Add-PrinterPort -Name $PortName -PrinterHostAddress '127.0.0.1' -PortNumber $Port
      Write-Log "added the $PortName port (127.0.0.1:$Port)"
    }
    if (-not (Get-Printer -Name $Name -ErrorAction SilentlyContinue)) {
      Add-Printer -Name $Name -DriverName $Driver -PortName $PortName
      Write-Log "added the $Name queue"
    }
  }
}
catch {
  Write-Log "error: $($_.Exception.Message)"
  if ($Strict) { throw }
}
