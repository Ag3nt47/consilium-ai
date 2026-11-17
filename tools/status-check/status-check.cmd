@echo off
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0StatusCheck.ps1" %*
