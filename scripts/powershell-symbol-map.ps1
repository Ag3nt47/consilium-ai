param(
    [Parameter(Mandatory, Position = 0)][string]$Path,
    [string]$Filter = ''
)

$source = Get-Item -LiteralPath $Path -ErrorAction Stop
if ($source -isnot [System.IO.FileInfo] -or $source.Length -gt 1MB) {
    throw 'Use a regular PowerShell source file up to 1 MiB.'
}

$stream = $source.OpenRead()
try {
    $buffer = [byte[]]::new(1MB + 1)
    $size = 0
    while ($size -lt $buffer.Length) {
        $count = $stream.Read($buffer, $size, $buffer.Length - $size)
        if (!$count) { break }
        $size += $count
    }
    if ($size -gt 1MB) { throw 'The source exceeds 1 MiB.' }
} finally {
    $stream.Dispose()
}

$bytes = [IO.MemoryStream]::new($buffer, 0, $size, $false)
$reader = [IO.StreamReader]::new($bytes)
try { $text = $reader.ReadToEnd() }
finally { $reader.Dispose() }

$tokens = $null
$errors = $null
$ast = [System.Management.Automation.Language.Parser]::ParseInput($text, [ref]$tokens, [ref]$errors)
if ($errors) { throw "PowerShell parse error at line $($errors[0].Extent.StartLineNumber)." }
$definitions = $ast.FindAll({
    param($node)
    $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -or
        $node -is [System.Management.Automation.Language.TypeDefinitionAst] -or
        $node -is [System.Management.Automation.Language.FunctionMemberAst]
}, $true)

$total = 0
foreach ($node in $definitions) {
    if ($node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
        $node.Parent -is [System.Management.Automation.Language.FunctionMemberAst]) { continue }
    if ($node.Name.IndexOf($Filter, [StringComparison]::OrdinalIgnoreCase) -lt 0) { continue }
    $total++
    if ($total -le 100) {
        $name = $node.Name.Substring(0, [Math]::Min(140, $node.Name.Length))
        $kind = switch ($node) {
            { $_ -is [System.Management.Automation.Language.TypeDefinitionAst] } { 'type'; break }
            { $_ -is [System.Management.Automation.Language.FunctionMemberAst] } { 'member'; break }
            default { 'function' }
        }
        '{0}-{1} {2} {3}' -f $node.Extent.StartLineNumber, $node.Extent.EndLineNumber, $kind, $name
    }
}
if ($total -gt 100) { "$($total - 100) additional rows omitted." }
