<#
.SYNOPSIS
  Refuse GNU-built executables that need an unbundled MinGW runtime DLL.
#>
param(
  [Parameter(Mandatory, Position = 0, ValueFromRemainingArguments)] [string[]] $Path
)
$ErrorActionPreference = 'Stop'
$Objdump = Get-Command 'x86_64-w64-mingw32-objdump' -ErrorAction SilentlyContinue
if (-not $Objdump) { $Objdump = Get-Command 'objdump' -ErrorAction SilentlyContinue }
if (-not $Objdump) { throw 'GNU packaging needs MinGW-w64 objdump on PATH to verify runtime DLL dependencies' }

foreach ($Executable in $Path) {
  $Imports = & $Objdump.Source -p $Executable
  if ($LASTEXITCODE -ne 0) { throw "objdump could not inspect $Executable (exit $LASTEXITCODE)" }
  $Dlls = @($Imports | ForEach-Object { if ($_ -match '^\s*DLL Name:\s*(\S+)\s*$') { $Matches[1] } })
  if ($Dlls.Count -eq 0) { throw "no PE imports found in $Executable; runtime dependencies could not be verified" }
  $Runtime = @($Dlls | Where-Object { $_ -match '^lib(?:gcc_s|stdc\+\+|winpthread|c\+\+|unwind|gomp|ssp).*\.dll$' })
  if ($Runtime.Count -gt 0) {
    throw "$Executable requires unbundled GNU runtime DLLs: $($Runtime -join ', '). Rebuild with -C target-feature=+crt-static and statically linked native libraries."
  }
  Write-Output "ok $([IO.Path]::GetFileName($Executable)): no external GNU runtime DLLs"
}
