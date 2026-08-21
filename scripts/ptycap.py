"""Run a command on a real pty of fixed size, type a prompt, capture raw output."""
import os, pty, sys, time, select, fcntl, termios, struct, signal

out_path, cols, rows, wait_before, wait_after, *cmd = sys.argv[1:]
cols, rows = int(cols), int(rows)
wait_before, wait_after = float(wait_before), float(wait_after)
prompt = os.environ.get("CAP_PROMPT", "")

pid, fd = pty.fork()
if pid == 0:
    os.execvp(cmd[0], cmd)

fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

buf = bytearray()
deadline = time.time() + wait_before
typed = False
end = time.time() + wait_before + wait_after
while time.time() < end:
    r, _, _ = select.select([fd], [], [], 0.2)
    if r:
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        buf += chunk
    if not typed and time.time() >= deadline and prompt:
        os.write(fd, prompt.encode() + b"\r")
        typed = True

open(out_path, "wb").write(bytes(buf))
try:
    os.kill(pid, signal.SIGTERM)
except ProcessLookupError:
    pass
os.waitpid(pid, os.WNOHANG)
print(f"captured {len(buf)} bytes -> {out_path}")
