## Cangjie commands.

cmd-cangjie-new-project = New Cangjie Project
cmd-cangjie-new-file = New Cangjie File
cmd-cangjie-build = Build Cangjie Project
cmd-cangjie-test = Test Cangjie Project
cmd-cangjie-run = Run Cangjie Project
cmd-cangjie-restart = Restart Cangjie Language Server
cmd-toggle-breakpoint = Toggle Breakpoint

## Start page.

workspace-start-new-cangjie = New Cangjie Project
workspace-start-new-cangjie-file = New Cangjie File
status-cangjie-build = Build
status-cangjie-test = Test
status-cangjie-run = Run
status-cangjie-debug = Debug

workspace-start-section-cangjie = Cangjie workflow
workspace-start-cangjie-workflow = Create a project or .cj file, then build, test, and run from the command palette.

## Project creation.

cangjie-project-name = Project name
cangjie-project-location = Location: { $path }
cangjie-project-create-failed = Couldn’t create the Cangjie project: { $error }
cangjie-project-created = Created Cangjie project “{ $name }”
cangjie-project-exists = cjpm.toml already exists in this directory
cangjie-project-invalid-name = Use letters, digits, underscores, and hyphens; don’t start with a digit
cangjie-project-invalid-location = Choose a directory for the new project
cangjie-project-cjpm-missing = cjpm wasn’t found. Install the Cangjie SDK and add its tools/bin directory to PATH

## Language service.

cangjie-lsp-starting = Cangjie LSP: Starting
cangjie-lsp-connected = Cangjie LSP: Connected
cangjie-lsp-unavailable = Cangjie LSP: Unavailable
cangjie-lsp-failed = Cangjie LSP: Failed
cangjie-lsp-problems = Cangjie problems: { $count ->
    [one] { $count }
   *[other] { $count }
}
cangjie-lsp-restart = Restarted the Cangjie language server
cangjie-lsp-unavailable-detail = LSPServer wasn’t found. Install the Cangjie SDK, then set its path in Settings or add tools/bin to PATH.
cangjie-lsp-failed-detail = The Cangjie language server quit unexpectedly: { $error }

cangjie-completion-accepted = Applied completion: { $label }

## Settings.

settings-page-cangjie = Cangjie
settings-group-cangjie-toolchain = Toolchain
settings-group-cangjie-language-service = Language Service
settings-cangjie-sdk-path = Cangjie SDK Path
settings-cangjie-sdk-path-desc = When empty, cjc, cjpm, cjfmt, and LSPServer are found through PATH, CANGJIE_HOME, and ~/.cjv.
settings-cangjie-lsp-path = LSPServer Path
settings-cangjie-lsp-path-desc = When empty, use LSPServer from the Cangjie SDK.
settings-cangjie-lsp-enabled = Enable Language Service
settings-cangjie-lsp-enabled-desc = Provide completion, navigation, references, diagnostics, and hover information for .cj files.
