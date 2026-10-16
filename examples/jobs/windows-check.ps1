$ErrorActionPreference = 'Stop'
# A native Windows smoke job, independent of optional Android SDKs or build tools.
$sum = 0
1..100 | ForEach-Object { $sum += $_ * $_ }
if ($sum -ne 338350) { throw 'Unexpected calculation result.' }
$result = [ordered]@{
    os = [Environment]::OSVersion.Platform.ToString()
    architecture = $env:PROCESSOR_ARCHITECTURE
    sum_of_squares = $sum
}
$result | ConvertTo-Json | Set-Content -LiteralPath result.json -Encoding UTF8
