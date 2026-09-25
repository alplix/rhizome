"""End-to-end test of the real application, driving its real WebView2 window.

What this covers that no other test can: that the Tauri glue actually works.
The commands the interface calls, the events the backend emits, the window's
capability set, the content security policy and the navigation guard are all
only exercised inside the real webview.

How it works: it starts a scripted IRC server on localhost, launches the app
with WebView2's remote-debugging port open, and speaks the Chrome DevTools
Protocol to the page (with a small WebSocket client written here, since the
standard library has none). The interface's own `api` object is then used to
connect, chat, search and disconnect, and the DOM is inspected afterwards.

Windows only (WebView2). Run from the repository root after `cargo build -p
rhizome-app`:

    python crates/rhizome-app/e2e/webview_e2e.py

It uses the application's real data directories and removes them when it
finishes; it refuses to start if they already exist, so it can never touch
someone's real profiles or message log.
"""

import base64
import hashlib
import json
import os
import shutil
import socket
import struct
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
EXE = ROOT / "target" / "debug" / "rhizome.exe"
IDENTIFIER = "org.rhizome.irc"
DATA_DIRS = [
    Path(os.environ.get("APPDATA", "")) / IDENTIFIER,
    Path(os.environ.get("LOCALAPPDATA", "")) / IDENTIFIER,
]
CDP_PORT = 9333

results = []


def check(name, condition, detail=""):
    results.append((name, bool(condition), detail))
    print(("PASS  " if condition else "FAIL  ") + name + (f"   [{detail}]" if detail and not condition else ""))


# ---- a scripted IRC server -----------------------------------------------------


class FakeIrc(threading.Thread):
    def __init__(self):
        super().__init__(daemon=True)
        self.sock = socket.socket()
        self.sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.listen(1)
        self.port = self.sock.getsockname()[1]
        self.received = []
        self.conn = None

    def send(self, line):
        self.conn.sendall((line + "\r\n").encode("utf-8"))

    def run(self):
        self.conn, _ = self.sock.accept()
        self.conn.settimeout(120)
        buf = b""
        try:
            while True:
                data = self.conn.recv(4096)
                if not data:
                    return
                buf += data
                while b"\r\n" in buf:
                    raw, buf = buf.split(b"\r\n", 1)
                    line = raw.decode("utf-8", "replace")
                    self.received.append(line)
                    self.handle(line)
        except OSError:
            return

    def handle(self, line):
        if line.startswith("CAP LS"):
            self.send(":srv CAP * LS :")
        elif line == "CAP END":
            self.send(":srv 001 alp :Welcome to the test network")
            self.send(":srv 005 alp PREFIX=(ov)@+ CHANTYPES=# CASEMAPPING=rfc1459 NETWORK=E2ENet :are supported by this server")
        elif line.startswith("JOIN "):
            channel = line.split(" ", 1)[1]
            self.send(f":alp!~alp@host JOIN {channel}")
            self.send(f":srv 332 alp {channel} :the e2e topic")
            self.send(f":srv 353 alp = {channel} :@alp bob")
            self.send(f":srv 366 alp {channel} :End of /NAMES list.")
            self.send(
                "@time=2026-09-25T10:00:00.000Z;msgid=e2e-1 :bob!b@h PRIVMSG "
                f"{channel} :alp: \x02hello\x02 from the fake server https://example.com/e2e ş"
            )
        elif line.startswith("PING "):
            self.send("PONG " + line.split(" ", 1)[1])


# ---- a minimal WebSocket client (RFC 6455) --------------------------------------


class WebSocket:
    def __init__(self, url):
        rest = url[len("ws://"):]
        hostport, path = rest.split("/", 1)
        host, port = hostport.split(":")
        self.sock = socket.create_connection((host, int(port)), timeout=60)
        key = base64.b64encode(os.urandom(16)).decode()
        request = (
            f"GET /{path} HTTP/1.1\r\nHost: {hostport}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        self.sock.sendall(request.encode())
        response = b""
        while b"\r\n\r\n" not in response:
            response += self.sock.recv(4096)
        head, self.buffer = response.split(b"\r\n\r\n", 1)
        if b" 101 " not in head.split(b"\r\n")[0]:
            raise RuntimeError("websocket upgrade refused: " + head.decode(errors="replace"))
        accept = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode()).digest()).decode()
        if accept.encode() not in head:
            raise RuntimeError("bad Sec-WebSocket-Accept")

    def _read(self, n):
        while len(self.buffer) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise ConnectionError("websocket closed")
            self.buffer += chunk
        data, self.buffer = self.buffer[:n], self.buffer[n:]
        return data

    def send(self, text):
        payload = text.encode("utf-8")
        header = bytearray([0x81])
        if len(payload) < 126:
            header.append(0x80 | len(payload))
        elif len(payload) < 65536:
            header.append(0x80 | 126)
            header += struct.pack(">H", len(payload))
        else:
            header.append(0x80 | 127)
            header += struct.pack(">Q", len(payload))
        mask = os.urandom(4)
        header += mask
        self.sock.sendall(bytes(header) + bytes(b ^ mask[i % 4] for i, b in enumerate(payload)))

    def recv(self):
        message = b""
        while True:
            b1, b2 = self._read(2)
            fin, opcode = b1 & 0x80, b1 & 0x0F
            length = b2 & 0x7F
            if length == 126:
                (length,) = struct.unpack(">H", self._read(2))
            elif length == 127:
                (length,) = struct.unpack(">Q", self._read(8))
            payload = self._read(length)
            if opcode == 0x9:  # ping
                continue
            if opcode == 0x8:
                raise ConnectionError("websocket closed by the peer")
            message += payload
            if fin:
                return message.decode("utf-8")


class Page:
    """One page of the app, driven over the DevTools Protocol."""

    def __init__(self, ws):
        self.ws = ws
        self.next_id = 0

    def evaluate(self, expression):
        self.next_id += 1
        self.ws.send(json.dumps({
            "id": self.next_id,
            "method": "Runtime.evaluate",
            "params": {"expression": expression, "awaitPromise": True, "returnByValue": True},
        }))
        while True:
            reply = json.loads(self.ws.recv())
            if reply.get("id") == self.next_id:
                break
        if "error" in reply:
            raise RuntimeError(reply["error"])
        result = reply["result"]
        if "exceptionDetails" in result:
            raise RuntimeError(result["exceptionDetails"].get("exception", {}).get("description", "script error"))
        return result["result"].get("value")


# ---- driving the app ---------------------------------------------------------------


def launch():
    env = dict(os.environ)
    env["WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS"] = f"--remote-debugging-port={CDP_PORT} --remote-allow-origins=*"
    proc = subprocess.Popen([str(EXE)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    deadline = time.time() + 45
    while time.time() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"the app exited early with code {proc.returncode}")
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{CDP_PORT}/json", timeout=2) as r:
                targets = json.load(r)
            pages = [t for t in targets if t.get("type") == "page" and "tauri.localhost" in t.get("url", "")]
            if pages:
                return proc, Page(WebSocket(pages[0]["webSocketDebuggerUrl"])), pages[0]
        except OSError:
            pass
        time.sleep(0.5)
    proc.kill()
    raise RuntimeError("the app's page never appeared")


def wait_for(page, expression, seconds=15):
    deadline = time.time() + seconds
    while time.time() < deadline:
        try:
            if page.evaluate(expression):
                return True
        except RuntimeError:
            pass
        time.sleep(0.3)
    return False


def stop(proc):
    proc.kill()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        pass
    time.sleep(1.5)


def main():
    if not EXE.exists():
        sys.exit(f"build first: {EXE} does not exist")
    for d in DATA_DIRS:
        if d.exists():
            sys.exit(f"refusing to run: {d} already exists and may hold real data")

    irc = FakeIrc()
    irc.start()
    proc = None
    try:
        # ---- first run -------------------------------------------------------------
        proc, page, target = launch()
        check("the window serves the interface from the app origin", "tauri.localhost" in target["url"], target["url"])
        if not wait_for(page, "!!window.__rhizome", 20):
            # The module never ran. Say why, since a silent hang teaches nothing.
            diag = page.evaluate("""import('./app.js').then(() => 'imports fine', e => 'import failed: ' + e)""")
            state = page.evaluate("JSON.stringify({ready: document.readyState, tauri: typeof window.__TAURI__, url: location.href})")
            check("the interface script runs", False, f"{diag} {state}")
            return
        boot = json.loads(page.evaluate("JSON.stringify({tauri: !!window.__TAURI__, mode: window.__rhizome.api.mode})"))
        check("the interface finds the real Tauri bridge, not the demo", boot["tauri"] and boot["mode"] == "tauri", str(boot))
        check("the store opened without complaint", page.evaluate("window.__rhizome.api.startupNotices().then(n => n.length === 0)"))
        check("with no saved networks the interface offers to add one", wait_for(page, "document.getElementById('profile-dialog').open"))

        # A profile with a bad nick is rejected by the backend, whatever the interface allows.
        rejected = page.evaluate(
            "window.__rhizome.api.saveProfile({id:'bad',name:'x',host:'h',port:6667,tls:false,nick:'a b',username:'a',realname:'r',channels:[],sasl_account:null}).then(() => 'accepted', e => String(e))"
        )
        check("the backend rejects an invalid profile", rejected != "accepted" and "nick" in rejected, rejected)

        page.evaluate(
            "window.__rhizome.api.saveProfile({id:'e2e',name:'E2E',host:'127.0.0.1',port:%d,tls:false,nick:'alp',username:'alp',realname:'Rhizome',channels:['#e2e'],sasl_account:null})"
            % irc.port
        )
        page.evaluate("document.getElementById('profile-dialog').close()")
        profiles = page.evaluate("window.__rhizome.api.listProfiles().then(p => p.map(x => x.id))")
        check("a saved profile can be listed back over IPC", profiles == ["e2e"], str(profiles))

        page.evaluate("window.__rhizome.api.connect('e2e', null)")
        check("events reach the page and the network registers", wait_for(page, "window.__rhizome.state.networks.get('e2e')?.status === 'registered'"))
        check("the channel is joined and shown", wait_for(page, "window.__rhizome.state.networks.get('e2e')?.buffers.get('#e2e')?.joined === true"))
        check("the member list arrives", wait_for(page, "window.__rhizome.state.networks.get('e2e')?.buffers.get('#e2e')?.members.length === 2"))
        check("the topic arrives", page.evaluate("window.__rhizome.state.networks.get('e2e').buffers.get('#e2e').topic") == "the e2e topic")
        check("a message is delivered with its time and highlight", wait_for(
            page,
            "window.__rhizome.state.networks.get('e2e')?.buffers.get('#e2e')?.lines.some(l => l.kind === 'message' && l.message.highlight && l.message.time_ms === 1790330400000)",
        ))

        dom = json.loads(page.evaluate("""JSON.stringify((() => {
            const row = [...document.querySelectorAll('#messages .line.message')].find(r => r.textContent.includes('fake server'));
            if (!row) return { found: false };
            const link = row.querySelector('a.link');
            return {
                found: true,
                bold: [...row.querySelectorAll('.bold')].map(n => n.textContent),
                link: link && link.dataset.url,
                highlighted: row.classList.contains('highlight'),
                turkish: row.textContent.includes('ş'),
                controlChars: /[\\x02\\x03\\x0f]/.test(row.textContent),
            };
        })())"""))
        check("the message is drawn with styling, a link and no control characters",
              dom.get("found") and dom["bold"] == ["hello"] and dom["link"] == "https://example.com/e2e" and dom["highlighted"] and dom["turkish"] and not dom["controlChars"], str(dom))

        # ---- sending ---------------------------------------------------------------------
        page.evaluate("window.__rhizome.api.sendMessage('e2e', '#e2e', 'merhaba dünya, şu hatayı gördün mü?')")
        page.evaluate("window.__rhizome.api.sendMessage('e2e', '#e2e', 'hi\\r\\nQUIT :pwned')")
        time.sleep(2.5)  # the send queue is rate limited
        sent = [l for l in irc.received if l.startswith("PRIVMSG")]
        check("a message is sent as UTF-8", "PRIVMSG #e2e :merhaba dünya, şu hatayı gördün mü?" in sent, str(sent))
        check("a pasted line break cannot inject a command through the real window",
              not any(l.startswith("QUIT") for l in irc.received) and "PRIVMSG #e2e :QUIT :pwned" in sent, str(irc.received[-4:]))

        # ---- the log, over IPC -----------------------------------------------------------
        check("history is stored and paged back over IPC", wait_for(
            page, "window.__rhizome.api.scrollback('e2e', '#e2e', null, 50).then(m => m.length >= 3)"))
        hit = json.loads(page.evaluate("""window.__rhizome.api.search('hello from:bob', null, false, 10).then(h => JSON.stringify(h.map(x => ({
            buffer: x.message.buffer, sender: x.message.sender, id: x.message.id, marked: x.snippet.filter(p => p.hit).map(p => p.text.toLowerCase()) }))))"""))
        check("search finds it, with the match marked", len(hit) == 1 and hit[0]["sender"] == "bob" and "hello" in hit[0]["marked"], str(hit))
        turkish = page.evaluate("window.__rhizome.api.search('dunya', null, false, 10).then(h => h.length)")
        check("Turkish text is found without typing Turkish characters", turkish == 1, str(turkish))
        context = page.evaluate("window.__rhizome.api.around(%d, 5).then(m => m.length)" % hit[0]["id"])
        check("a hit leads to its surrounding messages", context >= 2, str(context))

        # ---- security boundaries -----------------------------------------------------------
        refused = json.loads(page.evaluate("""Promise.all(['javascript:alert(1)', 'file:///C:/Windows/System32/calc.exe', 'ms-msdt:/id x', 'data:text/html,hi']
            .map(u => window.__rhizome.api.openUrl(u).then(() => 'OPENED ' + u, e => 'refused'))).then(r => JSON.stringify(r))"""))
        check("dangerous addresses are refused by the backend", all(r == "refused" for r in refused), str(refused))

        denied = page.evaluate(
            "window.__TAURI__.core.invoke('plugin:opener|open_url', {url: 'https://example.com/'}).then(() => 'ALLOWED', e => String(e))"
        )
        check("the window cannot call the opener plugin directly (capabilities)", denied != "ALLOWED", str(denied)[:120])
        denied_fs = page.evaluate("window.__TAURI__.core.invoke('plugin:fs|read_dir', {path: 'C:/'}).then(() => 'ALLOWED', e => 'denied')")
        check("there is no filesystem access from the window", denied_fs == "denied", str(denied_fs))

        before = page.evaluate("location.href")
        page.evaluate("setTimeout(() => window.location.assign('https://example.com/'), 0); 0")
        time.sleep(2)
        after = page.evaluate("location.href")
        check("the window refuses to navigate away from the app", before == after and "example.com" not in after, f"{before} -> {after}")

        csp = page.evaluate("""new Promise(resolve => {
            let violations = 0;
            document.addEventListener('securitypolicyviolation', () => violations++);
            const s = document.createElement('script'); s.textContent = 'window.__inlineRan = true'; document.head.append(s);
            setTimeout(() => resolve(JSON.stringify({ inlineRan: !!window.__inlineRan, violations })), 300);
        })""")
        csp = json.loads(csp)
        check("the content security policy blocks inline scripts", not csp["inlineRan"] and csp["violations"] >= 1, str(csp))

        # ---- disconnecting -------------------------------------------------------------------
        page.evaluate("window.__rhizome.api.disconnect('e2e')")
        deadline = time.time() + 10
        while time.time() < deadline and not any(l.startswith("QUIT") for l in irc.received):
            time.sleep(0.2)
        check("disconnecting says goodbye to the server", any(l.startswith("QUIT :Rhizome") for l in irc.received), str(irc.received[-2:]))
        check("the network is shown as closed", wait_for(page, "['idle','failed'].includes(window.__rhizome.state.networks.get('e2e').status)"))

        stop(proc)
        proc = None

        # ---- second run: everything survives a restart -----------------------------------------------
        check("the message log is a file on disk", (DATA_DIRS[0] / "rhizome.sqlite3").exists())
        check("the profile file holds no password", "password" not in (DATA_DIRS[0] / "profiles.json").read_text().lower())

        proc, page, _ = launch()
        check("the saved network is there after a restart", wait_for(page, "window.__rhizome.state.networks.has('e2e')"))
        history = page.evaluate("window.__rhizome.api.scrollback('e2e', '#e2e', null, 50).then(m => m.map(x => x.sender + ': ' + x.plain))")
        check("the conversation is read back from disk", any("fake server" in h for h in history) and any("merhaba" in h for h in history), str(history))
        found = page.evaluate("window.__rhizome.api.search('hello from:bob', null, false, 10).then(h => h.length)")
        check("search works on the reloaded log", found == 1, str(found))
        # FTS5 operators are plain words here, in the real window too: "OR" must
        # be searched for, not obeyed.
        literal = page.evaluate("window.__rhizome.api.search('hello OR nonexistentword', null, false, 10).then(h => h.length)")
        check("a query cannot use FTS5 operators", literal == 0, str(literal))
    finally:
        if proc:
            stop(proc)
        for d in DATA_DIRS:
            shutil.rmtree(d, ignore_errors=True)

    failed = [r for r in results if not r[1]]
    print(f"\n{len(results) - len(failed)}/{len(results)} checks passed")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
