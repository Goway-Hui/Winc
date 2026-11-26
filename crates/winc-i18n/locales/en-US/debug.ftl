## Debugging (DAP).

debug-start = Start Debugging
debug-stop = Stop Debugging
debug-close = Close Debug Panel
debug-continue = Continue
debug-next = Step Over
debug-step-in = Step Into
debug-step-out = Step Out

debug-status-idle = Debug: Idle
debug-status-starting = Debug: Starting
debug-status-running = Debug: Running
debug-status-stopped = Debug: Paused
debug-status-terminated = Debug: Stopped
debug-status-failed = Debug: Failed

debug-call-stack = Call Stack
debug-variables = Variables
debug-output = Debug Console
debug-stack-empty = No stack frames
debug-variables-empty = No variables
debug-stopped = Paused: { $reason }

debug-no-project = Open a Cangjie project before debugging
debug-not-cangjie-project = This folder has no cjpm.toml
debug-remote-unsupported = Debugging remote and WSL folders is not supported yet
debug-cjpm-missing = cjpm wasn’t found. Install the Cangjie SDK and add its tools/bin directory to PATH
debug-adapter-missing = lldb-vscode wasn’t found. Set debug.adapter or WINC_DEBUG_ADAPTER
debug-build-artifact-missing = The debug executable wasn’t built. Run cjpm build -g or set debug.program
debug-approval-required = Debugging needs a window to confirm the build command
debug-adapter-disconnected = The debug adapter disconnected
debug-launch-timeout = The debug adapter did not start in time
debug-confirm-detail = Start this command and the LLDB DAP adapter?
debug-adapter = Debug adapter: { $path }
