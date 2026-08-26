# 检查 CRF 编解码器分层架构的依赖方向
# 规划文档：docs/codec-architecture-refactor-plan.md §2 依赖方向
#
# 强制规则：
#   - decoder 不得依赖 encoder namespace（生产路径）
#   - backend 不得依赖 encoder/decoder session
#   - core 不得依赖 Tauri、UI、文件路径
#   - codec facade 不得包含算法实现
#
# 用法：pwsh scripts/check_layer_dependencies.ps1
# 退出码：0=通过，1=发现违规

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$crfRoot = Join-Path $repoRoot 'src-tauri\src\crf'

if (-not (Test-Path $crfRoot)) {
    Write-Error "CRF 源码目录不存在：$crfRoot"
    exit 2
}

$violations = @()

# 规则 1：decoder 生产代码不得引用 crate::crf::encoder
#   例外：#[cfg(test)] 模块内的测试依赖（测试夹具依赖，规划文档 §6.2 允许迁移期保留）
#   整文件测试模块（tests.rs / *_tests.rs）通过父模块 #[cfg(test)] mod tests; 引入，
#   文件本身无 #[cfg(test)] 标记，按文件名识别。
Get-ChildItem -Path (Join-Path $crfRoot 'decoder') -Recurse -Filter '*.rs' | ForEach-Object {
    $file = $_.FullName
    $baseName = $_.BaseName
    # 整文件测试模块：tests.rs / *_tests.rs（通过父模块 cfg(test) 引入）
    $isTestFile = ($baseName -eq 'tests') -or ($baseName -match '_tests$')
    $lines = Get-Content $file
    $inTestModule = $isTestFile
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        # 跟踪 #[cfg(test)] 模块边界（粗略：遇 mod tests 或 #[cfg(test)] 进入测试上下文）
        if ($line -match '#\[cfg\(test\)\]') { $inTestModule = $true }
        if ($line -match '^\s*mod\s+\w+_tests?\s*\{' -or $line -match '^\s*mod\s+tests?\s*\{') { $inTestModule = $true }

        # 生产路径（非测试模块、非注释行）检查 encoder 依赖
        $isComment = $line -match '^\s*//' -or $line -match '^\s*///' -or $line -match '^\s*//!'
        if (-not $inTestModule -and -not $isComment -and $line -match 'crate::crf::encoder') {
            $violations += [PSCustomObject]@{
                Rule = 'decoder→encoder 生产依赖'
                File = $file.Replace($repoRoot + '\', '')
                Line = $i + 1
                Content = $line.Trim()
            }
        }
    }
}

# 规则 2：core 不得依赖 Tauri / 文件路径
#   跳过注释行（// /// //!）
Get-ChildItem -Path (Join-Path $crfRoot 'core') -Recurse -Filter '*.rs' | ForEach-Object {
    $file = $_.FullName
    $lines = Get-Content $file
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if (-not ($line -match '^\s*//' -or $line -match '^\s*///' -or $line -match '^\s*//!')) {
            if ($line -match 'tauri' -or $line -match 'std::path::Path' -or $line -match 'std::fs') {
                $violations += [PSCustomObject]@{
                    Rule = 'core 依赖 Tauri/文件系统'
                    File = $file.Replace($repoRoot + '\', '')
                    Line = $i + 1
                    Content = $line.Trim()
                }
            }
        }
    }
}

# 规则 3：backend 不得依赖 encoder/decoder session
Get-ChildItem -Path (Join-Path $crfRoot 'backend') -Recurse -Filter '*.rs' | ForEach-Object {
    $file = $_.FullName
    $lines = Get-Content $file
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($line -match 'crate::crf::encoder' -or $line -match 'crate::crf::decoder') {
            $violations += [PSCustomObject]@{
                Rule = 'backend 依赖 encoder/decoder session'
                File = $file.Replace($repoRoot + '\', '')
                Line = $i + 1
                Content = $line.Trim()
            }
        }
    }
}

# 规则 4：codec facade 不得包含算法实现关键字（DCT/CABAC/Golomb/预测矩阵）
#   允许在注释中出现，只检查代码行（不含 // 和 //!）
Get-ChildItem -Path (Join-Path $crfRoot 'codec') -Recurse -Filter '*.rs' | ForEach-Object {
    $file = $_.FullName
    $lines = Get-Content $file
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        # 跳过注释行（用 if/else 嵌套避免 continue/return 控制流问题）
        if (-not ($line -match '^\s*//' -or $line -match '^\s*///' -or $line -match '^\s*//!')) {
            if ($line -match '\b(dct4x4|dct8x8|cabac_encode|cabac_decode|golomb_encode|golomb_decode)\b') {
                $violations += [PSCustomObject]@{
                    Rule = 'codec facade 包含算法实现'
                    File = $file.Replace($repoRoot + '\', '')
                    Line = $i + 1
                    Content = $line.Trim()
                }
            }
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
