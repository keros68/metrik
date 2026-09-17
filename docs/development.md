# Metrik 开发说明

## 环境依赖

依赖 Node.js 22+、Rust 1.88+。Ubuntu 24.04 还需安装 Tauri 的 WebKitGTK 与 AppIndicator 构建依赖：

```bash
sudo apt-get update
sudo apt-get install --no-install-recommends \
  build-essential curl wget file libssl-dev libwebkit2gtk-4.1-dev \
  libayatana-appindicator3-dev librsvg2-dev libxdo-dev patchelf
```

## 常用命令

```bash
npm install
npm run desktop:dev    # 桌面开发模式，读取本机真实日志
npm run dev            # 浏览器预览，仅演示数据
npm run desktop:build  # 构建安装包

npm run build
cd src-tauri && cargo test && cargo clippy -- -D warnings && cargo fmt --check
cargo test live_snapshot_smoke_test -- --ignored --nocapture  # 读本机真实日志的烟测
```

## 相关文档

- [ARCHITECTURE.md](ARCHITECTURE.md)：数据流、事件标识、存储与适配器边界。
- [PRODUCT-CONSTRAINTS.md](PRODUCT-CONSTRAINTS.md)：产品与各平台行为约束。
- [AGENTS.md](../AGENTS.md)：变更范围、平台边界与验证基线。
