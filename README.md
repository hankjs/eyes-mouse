# Eyes Mouse

Eyes Mouse 是一款面向 Apple Silicon Mac 的单屏眼动指针工具。Vue 显示九点校准、注视预览和五点验证，Tauri/Rust 接管系统指针，OpenCV 和 MediaPipe 在本机处理摄像头画面。进程之间只传输眼动特征和坐标。首版使用实体鼠标或触控板点击。

## 使用

1. 将应用窗口放在主显示器，开启相机并授予 macOS 相机权限。
2. 选择「开始九点校准」。看准每个亮点后按空格，保持注视约一秒。按 Esc 可退出。
3. 在预览中检查注视点。选择「验证精度」可以在五个额外位置测量定位误差，单位为 macOS 逻辑点。
4. 选择「启用指针」，在「系统设置 → 隐私与安全性 → 辅助功能」授予权限后再次启用。按 Control + Option + P 可随时暂停。持续丢失跟踪会自动暂停。

移动摄像头、改变坐姿或切换主显示器后，重新校准。重新启动应用后会重新校准。

## 本地开发和打包

在 Apple Silicon macOS 15 或更新版本上准备 Node.js 22.12+、Rust 1.93+、Xcode 命令行工具、arm64 Python 3.14 和 uv。

```sh
npm ci
sh tracker/setup.sh
npm run tauri -- dev
```

开发模式下，系统可能将相机授权归到启动应用的终端。测试实际权限时可使用打包后的应用。

```sh
npm run bundle:mac
```

产物为 `src-tauri/target/release/bundle/macos/Eyes Mouse.app`。跟踪组件及模型随应用打包，并使用本地 ad hoc 代码签名。构建需要下载依赖和模型，应用运行时的眼动跟踪在本机完成。

## 资源参考

在 Apple M2、macOS 15.6.1 上，用同一张测试人像模拟 640×480、24 FPS 的单脸追踪。20 秒内识别了 480 帧，Python 追踪进程平均占用 0.21 个 CPU 核，RSS 中位数 138 MiB、采样峰值 170 MiB。BGR 转 RGB 和模型推理的 P95 耗时为 10.84 ms。

整应用的前期预算是 0.25–0.45 个 CPU 核和 250–450 MiB 合计 RSS。这个范围是预算。实机摄像头采集、Tauri WebKit 开销、功耗及端到端指针延迟仍需测量。

当前应用包大小约 108 MiB。跟踪进程启动后会临时解包约 246 MiB 文件，进程退出时会清理。
