# 检查 CRF 编解码器分层架构的依赖方向
# 规划文档：docs/codec-architecture-refactor-plan.md §2 依赖方向
#
# 强制规则：
#   - decoder 不得依赖 encoder namespace（生产路径）
#   - encoder 不得依赖 decoder namespace（生产路径）
#   - backend 不得依赖 encoder/decoder session
#   - core 不得依赖 Tauri、UI、文件路径
#   - core 不得反向依赖 format（迁移循环，contract.rs P0 契约冻结豁免）
#   - codec facade 不得包含算法实现
#   - encoder/decoder/core 生产代码不得直接调用具体 CPU/GPU backend kernel
#     （必须经 backend trait / scheduler，规划文档 §2、§7.5）
#   - 应用层（非 crf 目录）不得绕过 codec facade 直接调用旧编解码入口
#
# 用法：pwsh scripts/check_layer_dependencies.ps1
# 退出码：0=通过，1=发现违规

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$crfRoot = Join-Path $repoRoot 'src-tauri\src\crf'
$appRoot = Join-Path $repoRoot 'src-tauri\src'

if (-not (Test-Path $crfRoot)) {
    Write-Error "CRF 源码目录不存在：$crfRoot"
    exit 2
}

$violations = @()

# ===== 测试文件识别辅助 =====
# 判定文件是否整体属于测试模块（文件级或目录级 cfg(test) 引入）：
#   - 文件名 tests.rs / *_tests.rs（父模块 #[cfg(test)] mod tests; 引入）
#   - 位于 tests/ 子目录（父模块 #[cfg(test)] mod tests; 引入，如 encoder/tests/*.rs）
function Test-IsTestFile {
    param([System.IO.FileInfo]$File)
    $baseName = $File.BaseName
    $full = $File.FullName.Replace('\', '/')
    if ($baseName -eq 'tests') { return $true }
    if ($baseName -match '_tests$') { return $true }
    if ($full -match '/tests/') { return $true }
    if ($full -match '/tests\.rs$') { return $true }
    return $false
}

# ===== 测试模块边界跟踪 =====
# 逐行跟踪是否处于 #[cfg(test)] / mod tests 上下文（粗略，与现有规则语义一致）
function Get-InTestModule {
    param([string[]]$Lines, [bool]$StartsInTest)
    $inTest = $StartsInTest
    $result = New-Object bool[] $Lines.Count
    for ($i = 0; $i -lt $Lines.Count; $i++) {
        $line = $Lines[$i]
        if ($line -match '#\[cfg\(test\)\]') { $inTest = $true }
        if ($line -match '^\s*mod\s+\w+_tests?\s*\{' -or $line -match '^\s*mod\s+tests?\s*\{') { $inTest = $true }
        $result[$i] = $inTest
    }
    return ,$result
}

function Add-Violation {
    param([string]$Rule, [string]$File, [int]$Line, [string]$Content)
    $script:violations += [PSCustomObject]@{
        Rule    = $Rule
        File    = $File
        Line    = $Line
        Content = $Content
    }
}

function Get-IsCommentLine {
    param([string]$Line)
    return ($Line -match '^\s*//' -or $Line -match '^\s*///' -or $Line -match '^\s*//!')
}

# ============================================================
# 规则 1：decoder 生产代码不得引用 encoder
# ============================================================
Get-ChildItem -Path (Join-Path $crfRoot 'decoder') -Recurse -Filter '*.rs' | ForEach-Object {
    $file = $_.FullName
    $isTestFile = Test-IsTestFile $_
    $lines = Get-Content $file
    $inTestMap = Get-InTestModule -Lines $lines -StartsInTest $isTestFile
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($inTestMap[$i]) { continue }
        if (Get-IsCommentLine $line) { continue }
        if ($line -match 'crate::crf::encoder') {
            Add-Violation -Rule 'decoder→encoder 生产依赖' -File $file.Replace($repoRoot + '\', '') -Line ($i + 1) -Content $line.Trim()
        }
    }
}

# ============================================================
# 规则 2：core 不得依赖 Tauri / 文件系统
# ============================================================
Get-ChildItem -Path (Join-Path $crfRoot 'core') -Recurse -Filter '*.rs' | ForEach-Object {
    $file = $_.FullName
    $lines = Get-Content $file
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if (Get-IsCommentLine $line) { continue }
        if ($line -match 'tauri' -or $line -match 'std::path::Path' -or $line -match 'std::fs') {
            Add-Violation -Rule 'core 依赖 Tauri/文件系统' -File $file.Replace($repoRoot + '\', '') -Line ($i + 1) -Content $line.Trim()
        }
    }
}

# ============================================================
# 规则 3：backend 不得依赖 encoder/decoder
# ============================================================
Get-ChildItem -Path (Join-Path $crfRoot 'backend') -Recurse -Filter '*.rs' | ForEach-Object {
    $file = $_.FullName
    $lines = Get-Content $file
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($line -match 'crate::crf::encoder' -or $line -match 'crate::crf::decoder') {
            Add-Violation -Rule 'backend 依赖 encoder/decoder session' -File $file.Replace($repoRoot + '\', '') -Line ($i + 1) -Content $line.Trim()
        }
    }
}

# ============================================================
# 规则 4：codec facade 不得包含算法实现关键字
# ============================================================
Get-ChildItem -Path (Join-Path $crfRoot 'codec') -Recurse -Filter '*.rs' | ForEach-Object {
    $file = $_.FullName
    $lines = Get-Content $file
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if (Get-IsCommentLine $line) { continue }
        if ($line -match '\b(dct4x4|dct8x8|cabac_encode|cabac_decode|golomb_encode|golomb_decode)\b') {
            Add-Violation -Rule 'codec facade 包含算法实现' -File $file.Replace($repoRoot + '\', '') -Line ($i + 1) -Content $line.Trim()
        }
    }
}

# ============================================================
# 规则 5：encoder 生产代码不得引用 decoder 容器/session/payload 层
#   例外：decoder::reconstruct（公共重建层，规划文档 §5.4）——编码端本地
#   闭环重建 G_hat 与解码端共享的纯重建契约，规划文档 §8.1 明确要求
#   "local decode/reconstruct G_hat"。
# ============================================================
Get-ChildItem -Path (Join-Path $crfRoot 'encoder') -Recurse -Filter '*.rs' | ForEach-Object {
    $file = $_.FullName
    $isTestFile = Test-IsTestFile $_
    $lines = Get-Content $file
    $inTestMap = Get-InTestModule -Lines $lines -StartsInTest $isTestFile
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($inTestMap[$i]) { continue }
        if (Get-IsCommentLine $line) { continue }
        # 允许 decoder::reconstruct（公共重建契约），其余 decoder 子层禁止
        if ($line -match 'crate::crf::decoder' -and $line -notmatch 'crate::crf::decoder::reconstruct') {
            Add-Violation -Rule 'encoder→decoder 生产依赖（reconstruct 层除外）' -File $file.Replace($repoRoot + '\', '') -Line ($i + 1) -Content $line.Trim()
        }
    }
}

# ============================================================
# 规则 6：core 生产代码不得反向依赖 format（迁移循环）
#   豁免：core/contract.rs（P0 契约冻结，字段引用 format 类型；
#         规划文档 §7 明确 P0 约束，第 7 条整改后移除豁免）
# ============================================================
Get-ChildItem -Path (Join-Path $crfRoot 'core') -Recurse -Filter '*.rs' | ForEach-Object {
    $file = $_.FullName
    if ($_.Name -eq 'contract.rs') { return } # P0 契约冻结豁免
    $lines = Get-Content $file
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if (Get-IsCommentLine $line) { continue }
        if ($line -match 'crate::crf::format') {
            Add-Violation -Rule 'core→format 反向依赖（迁移循环）' -File $file.Replace($repoRoot + '\', '') -Line ($i + 1) -Content $line.Trim()
        }
    }
}

# ============================================================
# 规则 7：encoder/decoder/core 生产代码不得直接调用具体 backend 实现
#   必须经 backend::ops（统一入口）或 BackendKernel trait / scheduler
#   （规划文档 §2、§7.5）
# ============================================================
foreach ($dirName in @('encoder', 'decoder', 'core')) {
    Get-ChildItem -Path (Join-Path $crfRoot $dirName) -Recurse -Filter '*.rs' | ForEach-Object {
        $file = $_.FullName
        $isTestFile = Test-IsTestFile $_
        $lines = Get-Content $file
        $inTestMap = Get-InTestModule -Lines $lines -StartsInTest $isTestFile
        for ($i = 0; $i -lt $lines.Count; $i++) {
            $line = $lines[$i]
            if ($inTestMap[$i]) { continue }
            if (Get-IsCommentLine $line) { continue }
            # 允许 backend::ops（统一入口）；禁止直接引用 cpu/gpu 具体实现
            if ($line -match 'crate::crf::backend::(cpu|gpu)' -and $line -notmatch 'crate::crf::backend::ops') {
                Add-Violation -Rule '直接调用具体 CPU/GPU backend kernel' -File $file.Replace($repoRoot + '\', '') -Line ($i + 1) -Content $line.Trim()
            }
        }
    }
}

# ============================================================
# 规则 8：应用层（非 crf 目录）不得绕过 codec facade
#   直接调用旧编解码入口（规划文档 §2：应用层只能依赖 facade）
# ============================================================
Get-ChildItem -Path $appRoot -Recurse -Filter '*.rs' | ForEach-Object {
    $full = $_.FullName.Replace('\', '/')
    # 仅检查应用层：排除 crf/、test/ 目录
    if ($full -match '/crf/' -or $full -match '/test/') { return }
    $file = $_.FullName
    $lines = Get-Content $file
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if (Get-IsCommentLine $line) { continue }
        if ($line -match 'crf::(encode_sequence|decode_from_bytes|decode_from_file)') {
            Add-Violation -Rule '应用绕过 codec facade 直接调用旧入口' -File $file.Replace($repoRoot + '\', '') -Line ($i + 1) -Content $line.Trim()
        }
    }
}

# 输出结果
if ($violations.Count -eq 0) {
    Write-Host '✓ 依赖方向检查通过' -ForegroundColor Green
    exit 0
} else {
    Write-Host "✗ 发现 $($violations.Count) 处依赖方向违规：" -ForegroundColor Red
    $violations | Format-Table -AutoSize
    exit 1
}
