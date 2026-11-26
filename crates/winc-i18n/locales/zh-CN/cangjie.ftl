## 设置项与交互命令的中文文案。

cmd-cangjie-new-project = 新建仓颉项目
cmd-cangjie-new-file = 新建仓颉文件
cmd-cangjie-build = 构建仓颉项目
cmd-cangjie-test = 测试仓颉项目
cmd-cangjie-run = 运行仓颉项目
cmd-cangjie-restart = 重启仓颉语言服务
cmd-toggle-breakpoint = 切换断点

workspace-start-new-cangjie = 新建仓颉项目
workspace-start-new-cangjie-file = 新建仓颉文件
status-cangjie-build = 构建
status-cangjie-test = 测试
status-cangjie-run = 运行
status-cangjie-debug = 调试

workspace-start-section-cangjie = 仓颉工作流
workspace-start-cangjie-workflow = 新建项目或 .cj 文件，使用命令面板构建、测试与运行。

cangjie-project-name = 项目名称
cangjie-project-location = 所在目录：{ $path }
cangjie-project-create-failed = 无法创建仓颉项目：{ $error }
cangjie-project-created = 已创建仓颉项目「{ $name }」
cangjie-project-exists = 该目录中已有 cjpm.toml
cangjie-project-invalid-name = 项目名称只能包含字母、数字、下划线和连字符，且不能以数字开头
cangjie-project-invalid-location = 请选择新项目所在的目录
cangjie-project-cjpm-missing = 未找到 cjpm，请安装仓颉 SDK 并将其 tools/bin 加入 PATH

cangjie-lsp-starting = 仓颉 LSP：启动中
cangjie-lsp-connected = 仓颉 LSP：已连接
cangjie-lsp-unavailable = 仓颉 LSP：不可用
cangjie-lsp-failed = 仓颉 LSP：失败
cangjie-lsp-problems = 仓颉问题：{ $count ->
    [one] { $count } 个
   *[other] { $count } 个
}
cangjie-lsp-restart = 已重新启动仓颉语言服务
cangjie-lsp-unavailable-detail = 未找到 LSPServer。请安装仓颉 SDK，并在设置中指定路径或将其 tools/bin 加入 PATH。
cangjie-lsp-failed-detail = 仓颉语言服务意外退出：{ $error }

cangjie-completion-accepted = 已应用补全：{ $label }

settings-page-cangjie = 仓颉
settings-group-cangjie-toolchain = 工具链
settings-group-cangjie-language-service = 语言服务
settings-cangjie-sdk-path = 仓颉 SDK 路径
settings-cangjie-sdk-path-desc = 留空时从 PATH、CANGJIE_HOME 和 ~/.cjv 中查找 cjc、cjpm、cjfmt 与 LSPServer。
settings-cangjie-lsp-path = LSPServer 路径
settings-cangjie-lsp-path-desc = 留空时使用仓颉 SDK 中的 LSPServer。
settings-cangjie-lsp-enabled = 启用语言服务
settings-cangjie-lsp-enabled-desc = 为 .cj 文件提供补全、跳转、引用、诊断和悬浮提示。
