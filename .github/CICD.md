# GitHub Actions CI/CD

## 工作流

### CI (ci.yml)
- **触发**: 推送到 `main`/`dev` 分支，或向 `main` 提交 PR
- **内容**: Clippy 检查、单元测试、依赖审计
- **平台**: macOS、Windows

### Release (release.yml)
- **触发**: 推送 `v*` tag 或手动触发
- **构建**: macOS (x64/ARM64)、Windows (x64)
- **产物**: 自动创建 GitHub Release 并上传安装包

## 发布新版本

1. 更新 `src-tauri/tauri.conf.json` 中的版本号
2. 提交并推送到 `main`
3. 创建并推送 tag：
   ```bash
   git tag v2.2.0
   git push origin v2.2.0
   ```

## 本地开发

```bash
cd src-tauri
cargo fmt              # 格式化
cargo clippy           # 静态检查
cargo test             # 运行测试
```
