<#
.SYNOPSIS
    Cross-platform GitHub & Git setup tool for Consilium and AIs (Windows PowerShell).
.DESCRIPTION
    Checks Git and GitHub CLI (gh), sets up credential helpers, ensures safe commit identities,
    and clones/connects repositories without freezing non-interactive sessions.
#>

[CmdletBinding()]
param(
    [Parameter(Position=0)]
    [ValidateSet("Status", "Setup", "Login", "Install", "Clone", "List", "Help")]
    [string]$Action = "Status",

    [Parameter(Position=1)]
    [string]$Target = "",

    [Parameter(Position=2)]
    [string]$Destination = "",

    [switch]$Json
)

function Find-Executable {
    param([string]$Name)
    $cmd = Get-Command $Name -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }

    $paths = @(
        "$env:ProgramFiles\GitHub CLI\gh.exe",
        "$env:LOCALAPPDATA\Programs\GitHub CLI\gh.exe",
        "$env:ProgramFiles\Git\cmd\git.exe",
        "$env:ProgramFiles\Git\bin\git.exe",
        "$env:LOCALAPPDATA\Programs\Git\cmd\git.exe",
        "$env:LOCALAPPDATA\Programs\Git\bin\git.exe",
        "$env:USERPROFILE\.local\bin\$Name.exe",
        "$env:USERPROFILE\.local\bin\$Name"
    )
    foreach ($p in $paths) {
        if ((Test-Path $p) -and ($p -like "*$Name*")) {
            return $p
        }
    }
    return $null
}

$GitExe = Find-Executable "git"
$GhExe  = Find-Executable "gh"

function Get-Status {
    $gitVer = ""
    $userName = ""
    $userEmail = ""
    $credHelper = ""

    if ($GitExe) {
        try { $gitVer = (& $GitExe --version 2>$null).Trim() } catch {}
        try { $userName = (& $GitExe config --global --get user.name 2>$null).Trim() } catch {}
        try { $userEmail = (& $GitExe config --global --get user.email 2>$null).Trim() } catch {}
        try { $credHelper = (& $GitExe config --global --get-all credential.https://github.com.helper 2>$null) } catch {}
    }

    $ghVer = ""
    $ghUser = ""
    if ($GhExe) {
        try { $ghVer = ((& $GhExe --version 2>$null) | Select-Object -First 1).Trim() } catch {}
        try { $ghUser = (& $GhExe api user --jq .login 2>$null).Trim() } catch {}
    }

    $hasGhHelper = ($credHelper -like "*auth git-credential*")

    $inRepo = $false
    $repoOrigin = ""
    $currentBranch = ""
    if ($GitExe) {
        try {
            $isInside = (& $GitExe rev-parse --is-inside-work-tree 2>$null).Trim()
            if ($isInside -eq "true") {
                $inRepo = $true
                $repoOrigin = (& $GitExe remote get-url origin 2>$null).Trim()
                $currentBranch = (& $GitExe branch --show-current 2>$null).Trim()
            }
        } catch {}
    }

    if ($Json) {
        $obj = [PSCustomObject]@{
            git = [PSCustomObject]@{
                available   = [bool]$GitExe
                path        = $GitExe
                version     = $gitVer
                userName    = $userName
                userEmail   = $userEmail
                hasGhHelper = $hasGhHelper
            }
            gh = [PSCustomObject]@{
                available     = [bool]$GhExe
                path          = $GhExe
                version       = $ghVer
                authenticated = [bool]$ghUser
                account       = $ghUser
            }
            currentRepo = [PSCustomObject]@{
                isRepo = $inRepo
                origin = $repoOrigin
                branch = $currentBranch
            }
        }
        $obj | ConvertTo-Json -Depth 4
        return
    }

    Write-Host "=== Consilium GitHub & Git Setup Status (Windows) ===" -ForegroundColor Cyan
    if ($GitExe) {
        Write-Host "✓ Git: $gitVer ($GitExe)" -ForegroundColor Green
        Write-Host "  - user.name:  $(if ($userName) { $userName } else { '<not set>' })"
        Write-Host "  - user.email: $(if ($userEmail) { $userEmail } else { '<not set>' })"
        if ($hasGhHelper) {
            Write-Host "  - Credential helper: configured (gh auth git-credential)" -ForegroundColor Green
        } else {
            Write-Host "  - Credential helper: not yet linked to gh (run .\GitHubSetup.ps1 Setup)" -ForegroundColor Yellow
        }
    } else {
        Write-Host "✗ Git: not found on system (run .\GitHubSetup.ps1 Install)" -ForegroundColor Red
    }

    Write-Host ""
    if ($GhExe) {
        Write-Host "✓ GitHub CLI: $ghVer ($GhExe)" -ForegroundColor Green
        if ($ghUser) {
            Write-Host "✓ Account: @$ghUser (Authenticated)" -ForegroundColor Green
        } else {
            Write-Host "! Account: Not logged in (run .\GitHubSetup.ps1 Login)" -ForegroundColor Yellow
        }
    } else {
        Write-Host "✗ GitHub CLI: not installed (run .\GitHubSetup.ps1 Install)" -ForegroundColor Red
    }

    if ($inRepo) {
        Write-Host ""
        Write-Host "Current Directory Repository:"
        Write-Host "  - Remote: $(if ($repoOrigin) { $repoOrigin } else { '<no remote>' })"
        Write-Host "  - Branch: $currentBranch"
    }
    Write-Host "======================================================" -ForegroundColor Cyan
}

function Invoke-Setup {
    if (-not $GhExe) {
        Write-Error "GitHub CLI (gh) is not installed. Run .\GitHubSetup.ps1 Install first."
        return
    }
    if (-not $GitExe) {
        Write-Error "Git is not installed."
        return
    }

    $ghUser = (& $GhExe api user --jq .login 2>$null).Trim()
    if (-not $ghUser) {
        Write-Error "Not signed in to GitHub. Run .\GitHubSetup.ps1 Login first."
        return
    }

    Write-Host "Configuring Git credential helper for https://github.com..."
    & $GhExe auth setup-git
    Write-Host "✓ Credential helper configured." -ForegroundColor Green

    $userName = (& $GitExe config --global --get user.name 2>$null).Trim()
    $userEmail = (& $GitExe config --global --get user.email 2>$null).Trim()

    if ((-not $userName) -or (-not $userEmail)) {
        Write-Host "Retrieving GitHub identity for safe commit attribution..."
        $tsv = (& $GhExe api user --jq "[.id, .login, (.name // `"`")] | @tsv" 2>$null).Trim()
        $parts = $tsv -split "`t"
        if ($parts.Count -ge 2) {
            $id = $parts[0]
            $login = $parts[1]
            $name = if ($parts.Count -ge 3 -and $parts[2]) { $parts[2] } else { $login }

            if (-not $userName) {
                & $GitExe config --global user.name $name
                Write-Host "✓ Set user.name = '$name'" -ForegroundColor Green
            }
            if (-not $userEmail) {
                $safeEmail = "$($id)+$($login)@users.noreply.github.com"
                & $GitExe config --global user.email $safeEmail
                Write-Host "✓ Set user.email = '$safeEmail' (privacy no-reply)" -ForegroundColor Green
            }
        }
    }

    Write-Host "✓ Setup complete! Git and GitHub CLI are ready." -ForegroundColor Green
}

function Invoke-Login {
    if (-not $GhExe) {
        Write-Error "GitHub CLI (gh) is not installed. Run .\GitHubSetup.ps1 Install first."
        return
    }

    if ([Environment]::UserInteractive) {
        Write-Host "Starting GitHub browser sign-in..."
        & $GhExe auth login --hostname github.com --git-protocol https --web
        if ($LASTEXITCODE -eq 0) {
            & $GhExe auth setup-git
            Write-Host "✓ Successfully authenticated and configured Git." -ForegroundColor Green
        }
    } else {
        Write-Host "Interactive sign-in requires a visible terminal. Run this in PowerShell:"
        Write-Host "  & '$GhExe' auth login --hostname github.com --git-protocol https --web; & '$GhExe' auth setup-git"
    }
}

function Invoke-Install {
    Write-Host "=== GitHub CLI & Git Installation (Windows) ===" -ForegroundColor Cyan
    Write-Host "Installing via Windows Package Manager (winget)..."
    try {
        Start-Process winget -ArgumentList "install --id GitHub.cli -e --source winget --accept-package-agreements" -Wait -NoNewWindow
        Start-Process winget -ArgumentList "install --id Git.Git -e --source winget --accept-package-agreements" -Wait -NoNewWindow
        Write-Host "✓ Installation finished. Restart terminal or re-run Status to verify." -ForegroundColor Green
    } catch {
        Write-Host "Winget failed or unavailable. Please download from:"
        Write-Host "  GitHub CLI: https://cli.github.com"
        Write-Host "  Git:        https://git-scm.com/download/win"
    }
}

function Invoke-Clone {
    param([string]$Slug, [string]$Dest)
    if (-not $Slug) {
        Write-Error "Usage: .\GitHubSetup.ps1 Clone <owner/repo|url> [destination]"
        return
    }

    if ($GhExe) {
        if ($Dest) { & $GhExe repo clone $Slug $Dest } else { & $GhExe repo clone $Slug }
    } elseif ($GitExe) {
        $url = $Slug
        if ($url -notmatch "^https?://" -and $url -notmatch "^git@") {
            $url = "https://github.com/$Slug.git"
        }
        if ($Dest) { & $GitExe clone $url $Dest } else { & $GitExe clone $url }
    } else {
        Write-Error "Neither gh nor git is available."
    }
}

function Invoke-List {
    if (-not $GhExe) {
        Write-Error "GitHub CLI is not installed."
        return
    }
    & $GhExe repo list --limit 30 --json nameWithOwner,isPrivate,updatedAt,description --template '{{range .}}{{printf "%-35s" .nameWithOwner}}{{if .isPrivate}} [private]{{else}} [public]{{end}}{{printf "\n  %s\n" .description}}{{end}}'
}

switch ($Action) {
    "Status"  { Get-Status }
    "Setup"   { Invoke-Setup }
    "Login"   { Invoke-Login }
    "Install" { Invoke-Install }
    "Clone"   { Invoke-Clone -Slug $Target -Dest $Destination }
    "List"    { Invoke-List }
    "Help"    {
        Write-Host "Usage: .\GitHubSetup.ps1 [Status|Setup|Login|Install|Clone|List] [Target] [Destination] [-Json]"
    }
}
