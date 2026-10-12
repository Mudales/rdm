#!/usr/bin/env python3
"""Renders the store images in store/images/ from the pages in store/shots/.

    python3 store/render.py

Needs a Chromium-based browser (Chrome, Chromium, Edge or Brave; set
$BROWSER to choose). It runs headless with a throwaway profile and is driven
over the DevTools protocol (standard library only). The screenshots show the
real popup and app UI, fed with sample data.
"""
import base64, json, os, shutil, socket, struct, subprocess, sys, tempfile, threading, time, urllib.request
import functools, http.server

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "store", "images")
JOBS = ["popup:1280x800:2000", "capture:1280x800:2000", "batch:1280x800:2500",
        "promo-small:440x280:800", "logo-300:300x300:800"]
CANDIDATES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "google-chrome", "chromium", "chromium-browser", "microsoft-edge", "brave-browser",
]


def find_browser():
    for c in ([os.environ["BROWSER"]] if os.environ.get("BROWSER") else []) + CANDIDATES:
        path = c if os.path.isabs(c) else shutil.which(c)
        if path and os.path.exists(path):
            return path
    sys.exit("no Chromium-based browser found; set $BROWSER")


def serve():
    class Quiet(http.server.SimpleHTTPRequestHandler):
        def log_message(self, *args):
            pass

    handler = functools.partial(Quiet, directory=ROOT)
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server.server_address[1]


class WS:
    def __init__(self, url):
        host, rest = url[len("ws://"):].split("/", 1)
        h, p = host.split(":")
        self.s = socket.create_connection((h, int(p)), timeout=30)
        key = base64.b64encode(os.urandom(16)).decode()
        self.s.sendall((f"GET /{rest} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                        f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
        buf = b""
        while b"\r\n\r\n" not in buf:
            buf += self.s.recv(4096)
        self.buf = buf.split(b"\r\n\r\n", 1)[1]
        self.n = 0
    def _read(self, n):
        while len(self.buf) < n:
            chunk = self.s.recv(1 << 20)
            if not chunk: raise EOFError
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out
    def send(self, method, **params):
        self.n += 1
        data = json.dumps({"id": self.n, "method": method, "params": params}).encode()
        mask = os.urandom(4)
        hdr = bytes([0x81])
        L = len(data)
        hdr += bytes([0x80 | L]) if L < 126 else bytes([0x80 | 126]) + struct.pack(">H", L) if L < 65536 else bytes([0x80 | 127]) + struct.pack(">Q", L)
        self.s.sendall(hdr + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))
        want = self.n
        while True:
            msg = self.recv()
            if msg.get("id") == want:
                if "error" in msg: raise RuntimeError(msg["error"])
                return msg.get("result", {})
    def recv(self):
        payload = b""
        while True:
            b1, b2 = self._read(2)
            L = b2 & 0x7F
            if L == 126: L = struct.unpack(">H", self._read(2))[0]
            elif L == 127: L = struct.unpack(">Q", self._read(8))[0]
            payload += self._read(L)
            if b1 & 0x80: break
        return json.loads(payload)


def main():
    os.makedirs(OUT, exist_ok=True)
    http_port = serve()
    debug_port = 9339
    profile = tempfile.mkdtemp(prefix="rdm-store-")
    proc = subprocess.Popen([find_browser(), "--headless", "--disable-gpu", "--hide-scrollbars", "--no-first-run",
                             "--use-mock-keychain", "--password-store=basic", f"--user-data-dir={profile}",
                             f"--remote-debugging-port={debug_port}", "--force-device-scale-factor=1", "about:blank"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        page = None
        for _ in range(150):
            try:
                targets = json.load(urllib.request.urlopen(f"http://127.0.0.1:{debug_port}/json"))
                page = next(t for t in targets if t["type"] == "page")
                break
            except Exception:
                time.sleep(0.2)
        if page is None:
            sys.exit("the browser did not start")
        ws = WS(page["webSocketDebuggerUrl"])
        ws.send("Page.enable")
        ws.send("Emulation.setEmulatedMedia", features=[{"name": "prefers-color-scheme", "value": "light"}])
        for job in JOBS:
            name, size, wait = job.split(":")
            w, h = map(int, size.split("x"))
            ws.send("Emulation.setDeviceMetricsOverride", width=w, height=h, deviceScaleFactor=1, mobile=False)
            alpha = 0 if name.startswith("logo") else 1
            ws.send("Emulation.setDefaultBackgroundColorOverride", color={"r": 255, "g": 255, "b": 255, "a": alpha})
            ws.send("Page.navigate", url=f"http://127.0.0.1:{http_port}/store/shots/{name}.html")
            time.sleep(int(wait) / 1000)
            shot = ws.send("Page.captureScreenshot", format="png",
                           clip={"x": 0, "y": 0, "width": w, "height": h, "scale": 1})
            path = os.path.join(OUT, f"{name}.png")
            with open(path, "wb") as f:
                f.write(base64.b64decode(shot["data"]))
            print(os.path.relpath(path, ROOT))
    finally:
        proc.terminate()
        try:
            proc.wait(10)
        except Exception:
            proc.kill()
        shutil.rmtree(profile, ignore_errors=True)


if __name__ == "__main__":
    main()
