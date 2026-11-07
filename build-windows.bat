@echo off
chcp 65001 >nul
title Сборка Rustcord
cd /d "%~dp0"

where cargo >nul 2>nul
if errorlevel 1 (
    echo.
    echo [!] Rust не установлен.
    echo     1. Сейчас откроется https://rustup.rs - скачайте rustup-init.exe и установите.
    echo        Если установщик предложит Visual Studio Build Tools - соглашайтесь.
    echo     2. Закройте это окно и запустите build-windows.bat ещё раз.
    echo.
    start https://rustup.rs
    pause
    exit /b 1
)

echo Собираю Rustcord. Первый раз это занимает 2-5 минут...
cargo build --release
if errorlevel 1 (
    echo.
    echo [!] Сборка не удалась. Скопируйте текст ошибки выше и пришлите его.
    pause
    exit /b 1
)

copy /y "target\release\rustcord.exe" "rustcord.exe" >nul
echo.
echo [OK] rustcord.exe готов - это всё приложение в одном файле.

set "ISCC=%ProgramFiles(x86)%\Inno Setup 6\ISCC.exe"
if exist "%ISCC%" (
    echo Собираю установщик...
    "%ISCC%" /Q installer.iss
    if not errorlevel 1 (
        copy /y "dist\RustcordSetup.exe" "RustcordSetup.exe" >nul
        echo [OK] RustcordSetup.exe готов - установщик с ярлыками и удалением.
    )
) else (
    echo.
    echo Хотите ещё и установщик RustcordSetup.exe?
    echo Поставьте бесплатный Inno Setup: https://jrsoftware.org/isdl.php
    echo и снова запустите этот файл.
)

echo.
echo Запускаю Rustcord...
start "" "%~dp0rustcord.exe"
pause
