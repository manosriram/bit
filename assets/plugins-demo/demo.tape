Output assets/plugins-demo/bit-plugins.mp4
Set Shell "bash"
Set FontSize 20
Set Width 1600
Set Height 1100
Set Padding 30
Set Theme "Catppuccin Mocha"
Set TypingSpeed 1ms
Set Framerate 15
Hide
Type "export PS1='bit-demo $ '"
Enter
Sleep 500ms
Hide
Type "clear; printf '\033[1;36m01 / A tiny plugin system\033[0m\n\n'; target/debug/examples/plugins_demo intro"
Enter
Sleep 500ms
Show
Sleep 13s
Hide
Type "clear; printf '\033[1;36m02 / Every before-hook return value\033[0m\n\n'; target/debug/examples/plugins_demo guards"
Enter
Sleep 500ms
Show
Sleep 19s
Hide
Type "clear; printf '\033[1;36m03 / Ordered guards: first block wins\033[0m\n\n'; target/debug/examples/plugins_demo ordering"
Enter
Sleep 500ms
Show
Sleep 12s
Hide
Type "clear; printf '\033[1;36m04 / Replace, append, print, recover\033[0m\n\n'; target/debug/examples/plugins_demo after"
Enter
Sleep 500ms
Show
Sleep 15s
Hide
Type "clear; printf '\033[1;36m05 / Custom tools and generated schemas\033[0m\n\n'; target/debug/examples/plugins_demo tools"
Enter
Sleep 500ms
Show
Sleep 20s
Hide
Type "clear; printf '\033[1;36m06 / Guards on another plugin tool\033[0m\n\n'; target/debug/examples/plugins_demo cross-file"
Enter
Sleep 500ms
Show
Sleep 12s
Hide
Type "clear; printf '\033[1;36m07 / Interactive and headless questions\033[0m\n\n'; target/debug/examples/plugins_demo ask"
Enter
Sleep 500ms
Show
Sleep 18s
Hide
Type "clear; printf '\033[1;36m08 / Approval policy and headless execution\033[0m\n\n'; target/debug/examples/plugins_demo policy"
Enter
Sleep 500ms
Show
Sleep 19s
Hide
Type "clear; printf '\033[1;36m09 / Shell, argv, exit codes and file reads\033[0m\n\n'; target/debug/examples/plugins_demo api"
Enter
Sleep 500ms
Show
Sleep 18s
Hide
Type "clear; printf '\033[1;36m10 / Persistent state and restart\033[0m\n\n'; target/debug/examples/plugins_demo state"
Enter
Sleep 500ms
Show
Sleep 12s
Hide
Type "clear; printf '\033[1;36m11 / Startup validation and helpful errors\033[0m\n\n'; target/debug/examples/plugins_demo validation"
Enter
Sleep 500ms
Show
Sleep 22s
Hide
Type "clear; printf '\033[1;36m12 / Errors, invalid returns and resource limits\033[0m\n\n'; target/debug/examples/plugins_demo failures"
Enter
Sleep 500ms
Show
Sleep 24s
Hide
Type "clear; printf '\033[1;36m13 / Discovery, diagnostics and recovery\033[0m\n\n'; python3 assets/plugins-demo/cli.py"
Enter
Sleep 500ms
Show
Sleep 24s
Hide
Type "clear; printf '\033[1;36m14 / Recovery and implementation boundaries\033[0m\n\n'; target/debug/examples/plugins_demo limits"
Enter
Sleep 500ms
Show
Sleep 23s
Screenshot assets/plugins-demo/poster.png
