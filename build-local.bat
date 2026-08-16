@echo off
rem SPDX-License-Identifier: MIT
rem Build the Windows desktop launcher (DSHLauncher.exe) from a local checkout.
setlocal EnableExtensions
cd /d "%~dp0"

set "BUILD_VENV=%CD%\.build-venv"
set "PIP_CACHE_DIR=%CD%\build-work\pip-cache"
set "PYINSTALLER_CONFIG_DIR=%CD%\build-work\pyinstaller-config"

py -3.11 -c "import tkinter" >nul 2>&1
if errorlevel 1 (
  echo Python 3.11 with Tk is required. Install it with: winget install Python.Python.3.11
  exit /b 1
)

if not exist "%BUILD_VENV%\Scripts\python.exe" (
  echo [1/3] Creating packaging environment...
  py -3.11 -m venv "%BUILD_VENV%"
)

echo [2/3] Preparing PyInstaller...
"%BUILD_VENV%\Scripts\python.exe" -m pip install --upgrade pip wheel setuptools
"%BUILD_VENV%\Scripts\python.exe" -m pip install -r requirements-runtime.txt
if errorlevel 1 exit /b 1
"%BUILD_VENV%\Scripts\python.exe" -m pip install -r requirements-build.txt
if errorlevel 1 exit /b 1

echo [3/3] Building the Windows executable...
"%BUILD_VENV%\Scripts\python.exe" -m PyInstaller --clean --noconfirm --distpath dist --workpath build-work build\windows.spec
if errorlevel 1 exit /b 1

echo.
echo Build complete: %CD%\dist\DSHLauncher.exe
endlocal
