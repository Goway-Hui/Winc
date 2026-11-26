## 调试（DAP）。

debug-start = 开始调试
debug-stop = 停止调试
debug-close = 关闭调试面板
debug-continue = 继续
debug-next = 单步跳过
debug-step-in = 单步进入
debug-step-out = 单步跳出

debug-status-idle = 调试：空闲
debug-status-starting = 调试：启动中
debug-status-running = 调试：运行中
debug-status-stopped = 调试：已暂停
debug-status-terminated = 调试：已停止
debug-status-failed = 调试：失败

debug-call-stack = 调用栈
debug-variables = 变量
debug-output = 调试控制台
debug-stack-empty = 没有调用栈
debug-variables-empty = 没有变量
debug-stopped = 已暂停：{ $reason }

debug-no-project = 请先打开仓颉项目再调试
debug-not-cangjie-project = 当前目录中没有 cjpm.toml
debug-remote-unsupported = 暂不支持调试远端或 WSL 目录
debug-cjpm-missing = 未找到 cjpm，请安装仓颉 SDK 并将其 tools/bin 加入 PATH
debug-adapter-missing = 未找到 lldb-vscode，请设置 debug.adapter 或 WINC_DEBUG_ADAPTER
debug-build-artifact-missing = 调试版本尚未构建，请先运行 cjpm build -g 或设置 debug.program
debug-approval-required = 调试需要窗口确认构建命令
debug-adapter-disconnected = 调试适配器已断开
debug-launch-timeout = 调试适配器启动超时
debug-confirm-detail = 将运行以下命令并启动 LLDB DAP 适配器：
debug-adapter = 调试适配器：{ $path }
