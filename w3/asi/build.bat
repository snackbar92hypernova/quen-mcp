@echo off
rem Builds w3\out\W3Quen.asi with the static CRT, against the PUBLIC Witcher Native Bridge SDK only (its release zip:
rem the header and an import library, no code; not in this repo), with Tracy's client and Detours compiled in from
rem w3\third_party (its README: versions and licences).
rem   build.bat        build only
rem   build.bat test   also builds w3\out\test (test.exe, a copy of the ASI and wnb_stub.cpp as a stand-in
rem                    WitcherNativeBridge.asi: never install anything from out\test) and runs test.exe in every mode,
rem                    then the Tracy modes in tracyhost.exe (test.exe with witcher3.exe 5.01's PE stamp)
rem   WNB_SDK        folder with include\WitcherNativeBridgeApi.h and lib\WitcherNativeBridge.lib (required)
rem   TRACY_CAPTURE  optional, for build.bat test: Tracy 0.14.1's tracy-capture.exe, which then captures from tracyhost.exe
setlocal
cd /d "%~dp0"
if not defined WNB_SDK (echo set WNB_SDK to the folder of the Witcher Native Bridge release zip & exit /b 1)
set OUT=..\out
set TRACY=..\third_party\tracy\public
set DETOURS=..\third_party\detours
rem Two files of Tracy 0.14.1 are not in git (an author's email in each): ..\third_party\README.md says where from.
for %%f in ("..\third_party\tracy\LICENSE" "%TRACY%\client\tracy_SPSCQueue.h") do (
  if not exist %%f (echo %%f is missing: copy it from Tracy 0.14.1's source, see w3\third_party\README.md & exit /b 1)
)
rem MSVC: the one on PATH, else the newest Visual Studio with the C++ x64 tools (vswhere)
where cl >nul 2>nul && goto build
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%VSWHERE%" (echo MSVC not found: run this from an "x64 Native Tools Command Prompt" & exit /b 1)
for /f "usebackq delims=" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSDIR=%%i"
if not defined VSDIR (echo no Visual Studio with the C++ x64 tools found & exit /b 1)
set "PATH=%PATH%;%ProgramFiles(x86)%\Microsoft Visual Studio\Installer"
call "%VSDIR%\VC\Auxiliary\Build\vcvars64.bat" >nul || exit /b 1
:build
if not exist "%OUT%\obj" mkdir "%OUT%\obj"
if not exist "%OUT%\test\obj" mkdir "%OUT%\test\obj"
rem Tracy 0.14.1's client with the W3Trace ASI's defines. What each does in 0.14.1's source:
rem   TRACY_ENABLE             the client exists, and Tracy.hpp's zone macros record (without it they are empty)
rem   TRACY_DELAYED_INIT, TRACY_MANUAL_LIFETIME  no profiler and no thread at load: they exist only from
rem                            tracy::StartupProfiler(), which trace.h calls only in a tracy launch
rem   TRACY_ON_DEMAND          events are queued only while a capture is connected
rem   TRACY_ONLY_LOCALHOST     the listening socket is IPv4 on the loopback address only (TracySocket.cpp)
rem   TRACY_NO_BROADCAST       no UDP announcement
rem   TRACY_NO_SYSTEM_TRACING, TRACY_NO_SAMPLING  no ETW session: no sampling, context switches or vsync events
rem   TRACY_NO_CALLSTACK       no call stacks, and no symbol thread
rem   TRACY_NO_FRAME_IMAGE     no frame images, and no compression thread
rem   TRACY_NO_CODE_TRANSFER   the client sends no code bytes
rem   TRACY_NO_CRASH_HANDLER   no exception handler of Tracy's
rem and NDEBUG: Tracy's asserts off (each would put its source file's path into the ASI)
set TRACY_DEFS=/DNDEBUG /DTRACY_ENABLE /DTRACY_ON_DEMAND /DTRACY_ONLY_LOCALHOST /DTRACY_NO_BROADCAST /DTRACY_DELAYED_INIT /DTRACY_MANUAL_LIFETIME /DTRACY_NO_SAMPLING /DTRACY_NO_SYSTEM_TRACING /DTRACY_NO_CALLSTACK /DTRACY_NO_FRAME_IMAGE /DTRACY_NO_CODE_TRANSFER /DTRACY_NO_CRASH_HANDLER
set BASE=/nologo /O2 /MT /std:c++20 /EHsc /permissive- /Brepro
cl %BASE% %TRACY_DEFS% /w /c /Fo"%OUT%\obj\\" "%TRACY%\TracyClient.cpp" || exit /b 1
cl %BASE% /w /c /DWIN32_LEAN_AND_MEAN /D_WIN32_WINNT=0x501 /Fo"%OUT%\obj\\" "%DETOURS%\detours.cpp" "%DETOURS%\disasm.cpp" "%DETOURS%\modules.cpp" || exit /b 1
set CFLAGS=%BASE% /W4 /WX /I "%WNB_SDK%\include" %TRACY_DEFS% /external:I "%TRACY%" /external:I "%DETOURS%" /external:W0
set THIRD="%OUT%\obj\TracyClient.obj" "%OUT%\obj\detours.obj" "%OUT%\obj\disasm.obj" "%OUT%\obj\modules.obj"
rem Winsock and the other DLLs only Tracy uses load when Tracy first calls them: a dormant W3Quen.asi loads what 0.1.2 did.
cl %CFLAGS% /LD /Fo"%OUT%\obj\\" W3Quen.cpp %THIRD% "%WNB_SDK%\lib\WitcherNativeBridge.lib" ^
  /link /Brepro /DELAYLOAD:ws2_32.dll /DELAYLOAD:user32.dll /DELAYLOAD:secur32.dll delayimp.lib /OUT:"%OUT%\W3Quen.asi" /IMPLIB:"%OUT%\obj\W3Quen.lib" || exit /b 1
if /i not "%~1"=="test" exit /b 0
set CFLAGS=%BASE% /W4 /WX /I "%WNB_SDK%\include"
cl %CFLAGS% /LD /Fo"%OUT%\test\obj\\" wnb_stub.cpp ^
  /link /Brepro /OUT:"%OUT%\test\WitcherNativeBridge.asi" /IMPLIB:"%OUT%\test\obj\WitcherNativeBridge.lib" || exit /b 1
cl %CFLAGS% /Fo"%OUT%\test\obj\\" test.cpp /link /Brepro /OUT:"%OUT%\test\test.exe" || exit /b 1
copy /y "%OUT%\W3Quen.asi" "%OUT%\test\" >nul || exit /b 1
for %%m in (notoken channel badtoken wrongpid squat tracy-stamp exec mkhost) do (
  "%OUT%\test\test.exe" %%m || exit /b 1
)
for %%m in (tracy-none tracy-token tracy-notoken tracy-on exec) do (
  "%OUT%\test\tracyhost.exe" %%m || exit /b 1
)
if defined TRACY_CAPTURE (
  "%OUT%\test\tracyhost.exe" tracy-capture || exit /b 1
)
echo build.bat test: ALL PASSED
exit /b 0
