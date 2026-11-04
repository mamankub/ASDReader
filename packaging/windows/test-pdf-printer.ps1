<#
.SYNOPSIS
  Prints a Thai page through the ASD PDF Printer and checks that the saved PDF holds the Thai that
  was printed, as text a viewer can search and copy.

.DESCRIPTION
  End to end on a real Windows machine: installs the queue (asd-pdf-printer.ps1), starts the
  listener, prints with System.Drawing in two Thai fonts, waits for the file the listener saves,
  reads its text with asdreader-cli and compares it with what was drawn, then removes the queue.

.EXAMPLE
  pwsh packaging/windows/test-pdf-printer.ps1 -App target\x86_64-pc-windows-msvc\release\ASDReader.exe -Cli target\x86_64-pc-windows-msvc\release\asdreader-cli.exe
#>
param(
  [Parameter(Mandatory)] [string] $App,
  [Parameter(Mandatory)] [string] $Cli
)
$ErrorActionPreference = 'Stop'
# The CLI writes UTF-8; read it as that, not in the console's code page.
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$Here = $PSScriptRoot
$Out = Join-Path ([IO.Path]::GetTempPath()) "asdreader-printer-test-$([IO.Path]::GetRandomFileName())"
New-Item -ItemType Directory -Force -Path $Out | Out-Null
$Port = 19100

# What is printed, and what a reader must find in the saved file. Tone marks over tall letters and
# over upper vowels, SARA AM, ญ and ฎ with a lower vowel, and Latin and digits beside them.
$lines = @('เอกสารภาษาไทย เจ้าของบ้าน ไฟฟ้า ปั้น น้ำ ญู ฎุ ที่ตั้ง', 'ต.เกิ้ง อ.เมืองมหาสารคาม จ.มหาสารคาม', 'ASDReader 0.13 ทดสอบการพิมพ์')
$fonts = @('Tahoma', 'Leelawadee UI')

& (Join-Path $Here 'asd-pdf-printer.ps1') -Install -Strict -Port $Port
$listener = $null
try {
  if (-not (Get-Printer -Name 'ASD PDF Printer' -ErrorAction SilentlyContinue)) { throw 'the ASD PDF Printer queue was not created' }
  $listener = Start-Process -FilePath $App -ArgumentList @('--printer-listener', '--no-open', '--out-dir', "`"$Out`"", '--port', $Port) -PassThru
  Start-Sleep 3
  if ($listener.HasExited) { throw "the listener exited at once with code $($listener.ExitCode)" }

  Add-Type -AssemblyName System.Drawing
  $pd = New-Object System.Drawing.Printing.PrintDocument
  $pd.PrinterSettings.PrinterName = 'ASD PDF Printer'
  $pd.add_PrintPage({
    param($s, $e)
    $y = 100
    foreach ($name in $fonts) {
      $font = New-Object System.Drawing.Font($name, 20)
      foreach ($l in $lines) { $e.Graphics.DrawString($l, $font, [System.Drawing.Brushes]::Black, 100, $y); $y += 50 }
      $y += 30
    }
    $e.HasMorePages = $false
  })
  $pd.Print()

  $pdf = $null
  for ($i = 0; $i -lt 90 -and -not $pdf; $i++) {
    Start-Sleep 1
    $pdf = Get-ChildItem $Out -Filter 'Printed *.pdf' -ErrorAction SilentlyContinue | Select-Object -First 1
  }
  if (-not $pdf) { throw 'no printed file arrived within 90 seconds' }
  Write-Output "printed file: $($pdf.Name), $($pdf.Length) bytes"

  $text = (& $Cli text $pdf.FullName) -join "`n"
  Write-Output $text
  $failures = @()
  # Word spaces differ with the font's metrics, and Thai has none to speak of: compare without them.
  $flat = $text -replace '\s', ''
  foreach ($l in $lines) {
    $needle = $l -replace '\s', ''
    $count = ([regex]::Matches($flat, [regex]::Escape($needle))).Count
    if ($count -lt $fonts.Count) { $failures += "found '$l' $count times, expected $($fonts.Count)" }
  }
  # No invented code points (private use, or the odd letters a driver borrows) among the Thai.
  if ($text -match '[-Ā-׿Ḁ-ỿ]') { $failures += 'the text holds code points that are not Thai or Latin' }
  if ($failures) { throw ("the Thai text of the printed file is wrong:`n" + ($failures -join "`n")) }
  Write-Output 'ok: every printed line is in the saved file as Thai text'
}
finally {
  if ($listener -and -not $listener.HasExited) { Stop-Process -Id $listener.Id -Force }
  & (Join-Path $Here 'asd-pdf-printer.ps1') -Remove -Port $Port
  Remove-Item -Recurse -Force $Out -ErrorAction SilentlyContinue
}
