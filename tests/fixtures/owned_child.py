#!/usr/bin/env python3
"""Isolated systemd KillMode acceptance fixture; no Gateway service involved."""
import os
import subprocess
import sys

child = subprocess.Popen(["/usr/bin/sleep", "120"])
with open(sys.argv[1], "w", encoding="ascii") as output:
    output.write(f"{os.getpid()} {child.pid}\n")
    output.flush()
    os.fsync(output.fileno())
child.wait()
