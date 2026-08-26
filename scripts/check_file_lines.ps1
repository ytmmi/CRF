# 检查 CRF 源码文件行数，执行规划文档 §12 的门禁
# 规划文档：docs/codec-architecture-refactor-plan.md §12
#
# 门禁规则：
#   - >1000 行：禁止构建、合并和发布
#   - >=800 行：只能修复、拆分和清理，不得继续加入新功能
#   - mod.rs 只做模块导出和少量 facade，不承载算法
#
# 用法：pwsh scripts/check_file_lines.ps1
# 退出码：0=通过（无超限），1=有 >1000 行文件，2=仅有 >=800 行预警

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$crfRoot = Join-Path $repoRoot 'src-tauri\src\crf'
$testRoot = Join-Path $repoRoot 'src-tauri\src\test'

$hardLimit = 1000
$warnLimit = 800

$files = @()
$searchPaths = @($crfRoot)
if (Test-Path $testRoot) { $searchPaths += $testRoot }

foreach ($path in $searchPaths) {
    $files += Get-ChildItem -Path $path -Recurse -Filter '*.rs' -File
}

$results = @()
$hasViolation = $false
$hasWarning = $false

foreach ($file in $files) {
    $lineCount = (Get-Content $file.FullName | Measure-Object -Line).Lines
    $relPath = $file.FullName.Replace($repoRoot + '\', '')

    if ($lineCount -gt $hardLimit) {
        $hasViolation = $true
        $results += [PSCustomObject]@{
            File = $relPath
            Lines = $lineCount
            Status = 'VIOLATION (>1000)'
        }
    } elseif ($lineCount -ge $warnLimit) {
        $hasWarning = $true
        $results += [PSCustomObject]@{
            File = $relPath
            Lines = $lineCount
            Status = 'WARNING (>=800)'
        }
    }
}

# 输出
if ($results.Count -eq 0) {
    Write-Host '✓ 源码行数检查通过（无超限文件）' -ForegroundColor Green
    exit 0
}

if ($hasViolation) {
    Write-Host '✗ 发现超过 1000 行硬限制的文件（禁止构建/合并/发布）：' -ForegroundColor Red
} else {
    Write-Host '⚠ 发现 >=800 行的预警文件（只能修复/拆分，不得新增功能）：' -ForegroundColor Yellow
}

$results | Sort-Object Lines -Descending | Format-Table -AutoSize

if ($hasViolation) { exit 1 } else { exit 2 }
