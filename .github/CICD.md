# GitHub Actions CI/CD 配置

本项目使用 GitHub Actions 进行自动化测试、代码检查和构建发布。

## 工作流说明

### 1. 测试工作流 (test.yml)

**触发条件：**
- 推送到 `main` 或 `dev` 分支
- 向 `main` 分支提交 Pull Request

**执行内容：**
- ✅ 在多平台（Ubuntu、macOS、Windows）运行 Rust 单元测试
- ✅ 运行集成测试
- ✅ 执行 Python 测试脚本（如果存在）
- ✅ 使用 Cargo 缓存加速构建

**查看状态：**
```
https://github.com/<your-username>/gemini-collector/actions/workflows/test.yml
```

### 2. 代码检查工作流 (lint.yml)

**触发条件：**
- 推送到 `main` 或 `dev` 分支
- 向 `main` 分支提交 Pull Request

**执行内容：**
- ✅ Rust 代码格式检查 (`cargo fmt`)
- ✅ Clippy 静态分析 (`cargo clippy`)
- ✅ 依赖安全性审计 (`cargo audit`)

### 3. 构建和发布工作流 (build.yml)

**触发条件：**
- 推送 tag（如 `v1.0.0`）
- 手动触发（workflow_dispatch）

**执行内容：**
- 🔨 构建多平台二进制文件：
  - Linux x64
  - macOS x64 (Intel)
  - macOS ARM64 (Apple Silicon)
  - Windows x64
- 📦 自动打包并上传到 GitHub Releases
- 🚀 生成 Release Notes

## 使用指南

### 本地开发

1. **运行测试**
   ```bash
   cd src-tauri
   cargo test
   ```

2. **代码格式化**
   ```bash
   cargo fmt
   ```

3. **代码检查**
   ```bash
   cargo clippy
   ```

### 发布新版本

1. **更新版本号**
   ```bash
   # 编辑 src-tauri/Cargo.toml
   # 编辑 src-tauri/tauri.conf.json
   ```

2. **创建并推送 tag**
   ```bash
   git tag v1.0.0
   git push origin v1.0.0
   ```

3. **自动构建**
   - GitHub Actions 会自动构建所有平台
   - 构建完成后自动创建 Release
   - 下载链接会出现在 Releases 页面

### 手动触发构建

如果需要手动触发构建（不创建 Release）：

1. 进入 Actions 页面
2. 选择 "Build and Release" 工作流
3. 点击 "Run workflow"
4. 选择分支并运行

## 状态徽章

在 README.md 中添加状态徽章：

```markdown
![Test](https://github.com/<username>/gemini-collector/workflows/Test/badge.svg)
![Lint](https://github.com/<username>/gemini-collector/workflows/Lint/badge.svg)
![Build](https://github.com/<username>/gemini-collector/workflows/Build%20and%20Release/badge.svg)
```

## 缓存策略

为了加速构建，我们使用了以下缓存：

- **Cargo 依赖缓存**：缓存 `~/.cargo` 和 `target/` 目录
- **缓存键**：基于 `Cargo.lock` 文件的哈希值
- **恢复策略**：优先使用完全匹配，其次使用前缀匹配

## 故障排查

### 测试失败

1. 查看 Actions 日志
2. 本地运行 `cargo test --verbose`
3. 检查是否有平台特定的问题

### 构建失败

1. 检查依赖是否正确安装
2. 确认 Rust 版本兼容性
3. 查看特定平台的构建日志

### Release 未创建

1. 确认 tag 格式正确（必须以 `v` 开头）
2. 检查 `GITHUB_TOKEN` 权限
3. 确认所有平台构建成功

## 配置文件

- `.github/workflows/test.yml` - 测试工作流
- `.github/workflows/lint.yml` - 代码检查工作流
- `.github/workflows/build.yml` - 构建和发布工作流

## 环境变量

无需配置额外的 secrets，所有工作流都使用 GitHub 自动提供的 `GITHUB_TOKEN`。

## 性能优化

- ✅ 使用 Cargo 增量编译
- ✅ 缓存依赖减少下载时间
- ✅ 并行运行多个测试任务
- ✅ 仅在必要时运行完整构建

## 更新日志

- 2024-03-23: 初始化 CI/CD 配置
  - 添加测试工作流
  - 添加代码检查工作流
  - 添加多平台构建和发布工作流
