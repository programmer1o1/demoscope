#!/usr/bin/env python3
# Launch the Portal 2 bit-trace using the frida bindings (no frida CLI needed).
#
# Usage (Windows, Portal 2 already running at the menu):
#   python run_trace.py
# then in the Portal 2 console:  playdemo youareamoron
#
# All [TRACE] lines print to this terminal. Copy them and send them back.
# Ctrl-C to stop.

import sys
import frida

SCRIPT = "p2_bittrace.js"
TARGET = "portal2.exe"

def on_message(message, data):
    if message["type"] == "send":
        print(message["payload"])
    elif message["type"] == "log":
        print(message["payload"])
    elif message["type"] == "error":
        print("ERROR:", message.get("stack") or message.get("description"))
    else:
        print(message)

def main():
    try:
        session = frida.attach(TARGET)
    except frida.ProcessNotFoundError:
        print(f"'{TARGET}' not found. Launch Portal 2 first, then run this.")
        sys.exit(1)

    with open(SCRIPT, "r", encoding="utf-8") as f:
        src = f.read()

    script = session.create_script(src)
    script.on("message", on_message)
    script.load()
    print("[run_trace] attached + loaded. In the Portal 2 console run: playdemo youareamoron")
    print("[run_trace] Ctrl-C here to stop.")
    try:
        sys.stdin.read()
    except KeyboardInterrupt:
        pass

if __name__ == "__main__":
    main()
