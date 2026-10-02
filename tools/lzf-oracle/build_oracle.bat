@echo off
REM Build the legacy LZF oracle for the gz-lzf differential test.
REM Params are pinned to what lzfP.h defaults to for this tree on x64.
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat" >nul 2>&1
cd /d "C:\Data\04_Server_Goonzu\goonzu-rs\tools\lzf-oracle"
cl /nologo /O2 ^
   /I"C:\Data\04_Server_Goonzu\goonzu-rs\tools\lzf-oracle" ^
   /Fe:"C:\Data\04_Server_Goonzu\goonzu-rs\tools\lzf-oracle\build\lzf_ref.exe" ^
   /Fo:"C:\Data\04_Server_Goonzu\goonzu-rs\tools\lzf-oracle\build\\" ^
   /DHLOG=16 /DVERY_FAST=1 /DULTRA_FAST=0 /DCHECK_INPUT=1 ^
   /DINIT_HTAB=0 /DLZF_STATE_ARG=0 /DSTRICT_ALIGN=0 ^
   lzf_ref.c lzf_c.c lzf_d.c
exit /b %ERRORLEVEL%
