<#
.SYNOPSIS
  Makes this computer trust ASDReader's private code-signing root, so ASDReader installers and
  programs signed with it open without the "Unknown publisher" warning.

.DESCRIPTION
  Run once per computer, as an administrator (right-click ▸ Run with PowerShell). It adds
  ASDReader-Root-CA.cer, which must sit beside this script, to:
    - Trusted Root Certification Authorities  (so the signature chains to a trusted root)
    - Trusted Publishers                       (so the publisher is accepted without a prompt)
  It checks the certificate's SHA-256 fingerprint first and refuses any other file. Remove it again
  with -Remove.

.EXAMPLE
  .\Install-ASDReaderCertificate.ps1
  .\Install-ASDReaderCertificate.ps1 -Remove
#>
param([switch] $Remove)
$ErrorActionPreference = 'Stop'

# SHA-256 fingerprint of the ASDReader Root CA. Compare it with the one given to you by the
# person who administers ASDReader before running this on a new computer.
$Expected = '49375E2AF6FCAA95B25F219370A7D72C1E40D5AD08540AC0BD5A6F47C2A11CB7'

$principal = [Security.Principal.WindowsPrincipal]::new([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
  throw 'Run this as an administrator (right-click, Run with PowerShell as administrator).'
}
$path = Join-Path $PSScriptRoot 'ASDReader-Root-CA.cer'
$cert = [Security.Cryptography.X509Certificates.X509Certificate2]::new($path)
$sha256 = [BitConverter]::ToString([Security.Cryptography.SHA256]::Create().ComputeHash($cert.RawData)).Replace('-', '')
if ($sha256 -ne $Expected) { throw "This is not the ASDReader Root CA (SHA-256 $sha256). Nothing was changed." }

foreach ($store in 'Root', 'TrustedPublisher') {
  $s = [Security.Cryptography.X509Certificates.X509Store]::new($store, 'LocalMachine')
  $s.Open('ReadWrite')
  try {
    if ($Remove) { $s.Remove($cert); Write-Output "Removed from $store" }
    else { $s.Add($cert); Write-Output "Added to $store" }
  } finally { $s.Close() }
}
if (-not $Remove) { Write-Output "Done. ASDReader files signed by $($cert.Subject) are now trusted on this computer." }
