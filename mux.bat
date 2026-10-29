@echo off
setlocal

if "%~1"=="" (
    echo Drag an .mp4 file onto this script.
    pause
    exit /b 1
)

rem Work from the video's folder so the subtitles filter gets a plain relative path
rem (avoids having to escape the drive-letter colon in C:\...)
pushd "%~dp1"
set "NAME=%~n1"

if not exist "%NAME%.srt" (
    echo Subtitle file not found: %~dp1%NAME%.srt
    popd
    pause
    exit /b 1
)

ffmpeg -y -hwaccel auto -i "%~nx1" ^
  -filter_complex "[0:v:0]subtitles='%NAME%.srt'[v];[0:a:0][0:a:1]amix=inputs=2:duration=first:normalize=0[a]" ^
  -map "[v]" -map "[a]" ^
  -c:v h264_nvenc -c:a aac -b:a 480k ^
  "%NAME%_muxed.mp4"

if errorlevel 1 pause
popd
endlocal