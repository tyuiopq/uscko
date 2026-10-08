# Сборка установщика: сначала программа (её exe вшивается в установщик),
# потом сам установщик. Результат — dist\uskko-setup-<версия>.exe.

$ErrorActionPreference = "Stop"

Set-Location $PSScriptRoot

cargo build --release -p uskko
if ($LASTEXITCODE -ne 0) { throw "Не удалось собрать uskko" }

cargo build --release -p uskko-setup
if ($LASTEXITCODE -ne 0) { throw "Не удалось собрать установщик" }

$version = (Select-String -Path "Cargo.toml" -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value

New-Item -ItemType Directory -Force "dist" | Out-Null

$target = "dist\uskko-setup-$version.exe"

Copy-Item "target\release\uskko-setup.exe" $target -Force

Write-Host "Готово: $target ($([math]::Round((Get-Item $target).Length / 1MB, 1)) МБ)"
