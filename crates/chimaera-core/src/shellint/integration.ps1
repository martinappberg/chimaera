# Chimaera shell integration for PowerShell (Windows PowerShell 5.1 and
# PowerShell 7): emits OSC 133 semantic-prompt marks (A=prompt, B=input,
# C=output start, D;exit=done), OSC 633;E command-line reports, and OSC 7
# cwd reports, so the chimaera daemon can keep a per-session command journal
# and know when this shell is at its prompt.
#
# Safe to dot-source more than once; wraps the existing prompt function
# (themes, oh-my-posh, starship keep working) and PSReadLine's
# PSConsoleHostReadLine. Without PSReadLine there is no hook between Enter
# and the command, so only the cwd is reported and execs use sentinel mode.

if ($global:__chimaera_integration) { return }
$global:__chimaera_integration = $true

$global:__chimaera_in_command = $false
$global:__chimaera_lec = $global:LASTEXITCODE
$global:__chimaera_marks = [bool](Get-Module -Name PSReadLine)
$global:__chimaera_prompt = $function:prompt

function global:__chimaera_osc([string]$payload) {
    "$([char]27)]$payload$([char]7)"
}

# VS Code 633;E escaping: backslash, semicolon and control characters.
function global:__chimaera_escape([string]$text) {
    $out = New-Object System.Text.StringBuilder
    foreach ($c in $text.ToCharArray()) {
        if ($c -eq [char]92) { [void]$out.Append('\\') }
        elseif ($c -eq [char]59) { [void]$out.Append('\x3b') }
        elseif ([int]$c -lt 32) { [void]$out.Append(('\x{0:x2}' -f [int]$c)) }
        else { [void]$out.Append($c) }
    }
    $out.ToString()
}

function global:__chimaera_cwd_url {
    if ($PWD.Provider.Name -ne 'FileSystem') { return $null }
    $path = $PWD.ProviderPath -replace '\\', '/'
    if ($path -notmatch '^/') { $path = '/' + $path }
    $encoded = New-Object System.Text.StringBuilder
    foreach ($b in [System.Text.Encoding]::UTF8.GetBytes($path)) {
        $c = [char]$b
        if ($b -lt 128 -and $c -match '[A-Za-z0-9/._~:-]') { [void]$encoded.Append($c) }
        else { [void]$encoded.Append(('%{0:X2}' -f $b)) }
    }
    "file://$env:COMPUTERNAME$($encoded.ToString())"
}

function global:prompt {
    # First statement: anything earlier would overwrite the status.
    $ok = $global:?
    $code = if ($ok) { 0 }
        elseif ($global:LASTEXITCODE -and $global:LASTEXITCODE -ne $global:__chimaera_lec) { $global:LASTEXITCODE }
        else { 1 }
    $marks = ''
    if ($global:__chimaera_in_command) {
        $marks += __chimaera_osc "133;D;$code"
        $global:__chimaera_in_command = $false
    }
    $url = __chimaera_cwd_url
    if ($url) { $marks += __chimaera_osc "7;$url" }
    if ($global:__chimaera_marks) { $marks += __chimaera_osc '133;A' }
    # Hand the original prompt the status it would have seen.
    if (-not $ok) { Write-Error 'failure' -ErrorAction Ignore }
    $inner = if ($global:__chimaera_prompt) { & $global:__chimaera_prompt } else { "PS $($PWD.Path)> " }
    $tail = if ($global:__chimaera_marks) { __chimaera_osc '133;B' } else { '' }
    "$marks$inner$tail"
}

if ($global:__chimaera_marks) {
    $global:__chimaera_readline = $function:PSConsoleHostReadLine
    function global:PSConsoleHostReadLine {
        $line = & $global:__chimaera_readline
        $global:__chimaera_lec = $global:LASTEXITCODE
        if ($line -and $line.Trim()) {
            $global:__chimaera_in_command = $true
            [Console]::Write((__chimaera_osc "633;E;$(__chimaera_escape $line)") + (__chimaera_osc '133;C'))
        }
        $line
    }
}
