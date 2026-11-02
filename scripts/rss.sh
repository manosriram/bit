#!/bin/sh
# Idle RSS limit (ARCHITECTURE.md §13): start bit in a pty, let it settle, read its RSS.
set -eu
max=8192
kb=$(python3 - <<'PY'
import fcntl, os, pty, select, struct, subprocess, termios, time
pid, fd = pty.fork()
if pid == 0:
    os.execv("target/release/bit", ["bit", "--model", "m"])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
end = time.time() + 3
while time.time() < end:  # drain output so bit never blocks on a full pty
    if select.select([fd], [], [], 0.1)[0]:
        os.read(fd, 65536)
print(subprocess.check_output(["ps", "-o", "rss=", "-p", str(pid)]).decode().strip())
os.kill(pid, 9)
PY
)
echo "bit idle rss ${kb}/${max} KB"
[ "$kb" -le "$max" ]
