@echo off
rem ------------------------------------------------------------------
rem SpotifyRenderer build script.
rem Usage:
rem   build.cmd              Debug build + run tests
rem   build.cmd quick        Debug build only (skip tests)
rem   build.cmd Release      Release build + run tests
rem   build.cmd package      Release build + copy both executables into dist\
rem Requires: Visual Studio 2022 C++ tools, CMake, Ninja, and Rust/Cargo.
rem The script locates each tool without making machine-wide PATH changes.
rem ------------------------------------------------------------------
setlocal EnableDelayedExpansion

set "VSROOT="
for %%V in ("C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools" ^
            "C:\Program Files\Microsoft Visual Studio\2022\BuildTools" ^
            "C:\Program Files\Microsoft Visual Studio\2022\Community" ^
            "C:\Program Files\Microsoft Visual Studio\2022\Professional" ^
            "C:\Program Files\Microsoft Visual Studio\2022\Enterprise") do (
  if exist "%%~V\VC\Auxiliary\Build\vcvars64.bat" if not defined VSROOT set "VSROOT=%%~V"
)
if not defined VSROOT (
  echo ERROR: Visual Studio 2022 with C++ tools not found.
  exit /b 1
)

call "%VSROOT%\VC\Auxiliary\Build\vcvars64.bat" >nul
if errorlevel 1 (
  echo ERROR: vcvars64.bat failed.
  exit /b 1
)

set "CMAKE="
for %%C in ("%VSROOT%\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe" ^
            "C:\Program Files\CMake\bin\cmake.exe" ^
            "C:\Program Files (x86)\CMake\bin\cmake.exe") do (
  if exist "%%~C" if not defined CMAKE set "CMAKE=%%~C"
)
if not defined CMAKE (
  where cmake.exe >nul 2>nul
  if not errorlevel 1 set "CMAKE=cmake.exe"
)
if not defined CMAKE (
  echo ERROR: CMake not found.
  exit /b 1
)

set "NINJA="
for %%N in ("%VSROOT%\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja\ninja.exe") do (
  if exist "%%~N" if not defined NINJA set "NINJA=%%~N"
)
if not defined NINJA (
  where ninja.exe >nul 2>nul
  if not errorlevel 1 set "NINJA=ninja.exe"
)
if not defined NINJA (
  echo ERROR: Ninja not found.
  exit /b 1
)

set "CARGO="
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" set "CARGO=%USERPROFILE%\.cargo\bin\cargo.exe"
if not defined CARGO (
  where cargo.exe >nul 2>nul
  if not errorlevel 1 set "CARGO=cargo.exe"
)
if not defined CARGO (
  echo ERROR: Cargo not found. Rust 1.85 or newer is required for SpotifyPlaybackEngine.
  exit /b 1
)

set "BUILD_TYPE=Debug"
set "BUILD_DIR=build"
set "DO_PACKAGE=0"
set "RUN_TESTS=1"
if /I "%~1"=="Release"   set "BUILD_TYPE=Release" & set "BUILD_DIR=build-release"
if /I "%~1"=="package"   set "BUILD_TYPE=Release" & set "BUILD_DIR=build-release" & set "DO_PACKAGE=1"
if /I "%~1"=="quick"     set "RUN_TESTS=0"
echo == Configure (%BUILD_TYPE%) ==
"%CMAKE%" -S . -B "%BUILD_DIR%" -G Ninja -DCMAKE_BUILD_TYPE=%BUILD_TYPE% -DCMAKE_MAKE_PROGRAM="%NINJA%" -DCARGO_EXECUTABLE="%CARGO%"
if errorlevel 1 exit /b 1

echo == Build ==
"%CMAKE%" --build "%BUILD_DIR%"
if errorlevel 1 exit /b 1


if "%RUN_TESTS%"=="1" (
  echo == Rust tests ==
  "%CARGO%" test --locked --manifest-path engine\Cargo.toml
  if errorlevel 1 exit /b 1
  echo == C++ tests ==
  "%CMAKE%" --build "%BUILD_DIR%" --target test
  if errorlevel 1 exit /b 1
)
if "%DO_PACKAGE%"=="1" (
  echo == Package ==
  "%CMAKE%" --build "%BUILD_DIR%" --target package
  if errorlevel 1 exit /b 1
  echo.
  echo Packaged artifacts: dist\SpotifyRenderer.exe and dist\SpotifyPlaybackEngine.exe
  exit /b 0
)

echo.
echo Build OK. Artifacts: %BUILD_DIR%\SpotifyRenderer.exe and %BUILD_DIR%\engine-target\release\SpotifyPlaybackEngine.exe
exit /b 0
