@echo off
cd /d "%~dp0"
call "C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if errorlevel 1 exit /b 1
set PATH=C:\VulkanSDK\1.4.341.1\Bin;C:\Users\adria\OneDrive\Documents\LLVM\bin;C:\Program Files\CMake\bin;C:\Users\adria\.cargo\bin;C:\Users\adria\AppData\Local\Microsoft\WinGet\Packages\Ninja-build.Ninja_Microsoft.Winget.Source_8wekyb3d8bbwe;%PATH%
set LIBCLANG_PATH=C:\Users\adria\OneDrive\Documents\LLVM\bin
set VULKAN_SDK=C:\VulkanSDK\1.4.341.1
set CMAKE_GENERATOR=Ninja
set Platform=
set CMAKE_GENERATOR_PLATFORM=
cargo build -j 2 --color=never
