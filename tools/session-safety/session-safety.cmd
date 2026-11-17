@echo off
powershell.exe -NoProfile -ExecutionPolicy Bypass -File "%~dp0SessionSafety.ps1" %*
