#!/usr/bin/env python3
"""Fake cast devices for developing: a Chromecast (mDNS + TLS) and a DLNA
renderer (SSDP + SOAP) on this computer. They speak the real protocols and
really fetch and decode what they are told to play (with ffmpeg, in real
time), so casting can be tried without a TV.

    scripts/fake-cast-devices.py [--refuse TEXT] [--no-mkv]

--refuse TEXT: the Chromecast refuses content types containing TEXT.
--no-mkv:      the DLNA renderer doesn't list Matroska.
Only this computer's own searches are answered. Needs openssl and ffmpeg.
"""
import argparse, html, http.server, json, re, socket, ssl, struct, subprocess, sys, tempfile, threading, time, urllib.request

ap = argparse.ArgumentParser()
ap.add_argument("--refuse", default="\0")
ap.add_argument("--no-mkv", action="store_true")
ap.add_argument("--cast-port", type=int, default=8109)
ap.add_argument("--dlna-port", type=int, default=8110)
ap.add_argument("--prefix", default="Fake ", help="before the devices' names")
args = ap.parse_args()


def device(n):
    n = args.prefix + n
    return n[0].upper() + n[1:]


s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.connect(("192.0.2.1", 80))
IP = s.getsockname()[0]


def log(*a):
    print(time.strftime("%H:%M:%S"), *a, flush=True)


class Play:
    """A file playing: ffmpeg decodes it in real time, a clock keeps the position."""

    def __init__(self, url, start, paused=False):
        self.url, self.pos, self.at, self.paused = url, start, time.time(), paused
        self.state, self.length = "LOADING", 0.0
        self.proc = None
        self.run(start)

    def run(self, start):
        if self.proc:
            self.proc.kill()
        cmd = ["ffmpeg", "-v", "error", "-nostdin", "-re"] + (["-ss", str(start)] if start > 0 else []) + ["-i", self.url, "-f", "null", "-"]
        self.proc = subprocess.Popen(cmd, stderr=subprocess.PIPE, text=True)
        self.state = "PLAYING"
        self.pos, self.at = start, time.time()
        threading.Thread(target=self.wait, args=(self.proc,), daemon=True).start()
        if self.paused:
            self.pause()

    def wait(self, proc):
        err = proc.stderr.read()
        code = proc.wait()
        if proc is self.proc and self.state != "IDLE":
            self.pos, self.state = self.now(), "IDLE"
            self.ok = code == 0 and not err
            log("  decoded to the end" if self.ok else f"  decode failed ({code}): {err.strip()[:300]}")

    def now(self):
        return self.pos if self.paused or self.state == "IDLE" else self.pos + time.time() - self.at

    def pause(self):
        self.pos, self.paused = self.now(), True
        self.proc.send_signal(19)

    def play(self):
        self.at, self.paused = time.time(), False
        self.proc.send_signal(18)

    def seek(self, t):
        self.run(t)

    def stop(self):
        self.pos, self.state = self.now(), "IDLE"
        self.ok = True
        self.proc.kill()


def fetch(url):
    with urllib.request.urlopen(url, timeout=60) as r:
        return r.headers, r.read()


def check_extras(*urls):
    for u in urls:
        if u:
            h, b = fetch(u)
            log("  extra", h["Content-Type"], len(b), "bytes:", b[:90] if b"-->" in b else "")


# ---- Chromecast ----

def varint(n):
    out = b""
    while n >= 0x80:
        out += bytes([n & 0x7F | 0x80])
        n >>= 7
    return out + bytes([n])


def pack(src, dst, ns, body):
    b = b"\x08\x00"
    for f, v in ((2, src), (3, dst), (4, ns)):
        v = v.encode()
        b += bytes([f << 3 | 2]) + varint(len(v)) + v
    v = json.dumps(body).encode()
    b += b"\x28\x00\x32" + varint(len(v)) + v
    return struct.pack(">I", len(b)) + b


def unpack(b):
    i, out = 0, {}
    def vi():
        nonlocal i
        n = sh = 0
        while True:
            c = b[i]; i += 1
            n |= (c & 0x7F) << sh; sh += 7
            if not c & 0x80:
                return n
    while i < len(b):
        k = vi()
        if k & 7 == 0:
            vi()
        else:
            n = vi()
            out[k >> 3] = b[i:i + n].decode()
            i += n
    return out


class Chromecast:
    NS = "urn:x-cast:com.google.cast."

    def __init__(self):
        self.volume, self.muted, self.app, self.play, self.msid = 0.35, False, None, None, 0
        d = tempfile.mkdtemp()
        subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", d + "/k", "-out", d + "/c",
                        "-days", "2", "-subj", "/CN=fake"], check=True, capture_output=True)
        self.ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        self.ctx.load_cert_chain(d + "/c", d + "/k")
        self.sock = socket.create_server((IP, args.cast_port))
        threading.Thread(target=self.accept, daemon=True).start()
        threading.Thread(target=self.mdns, daemon=True).start()

    def accept(self):
        while True:
            c, a = self.sock.accept()
            threading.Thread(target=self.client, args=(c, a), daemon=True).start()

    def status(self, rid=0):
        apps = [{"appId": "CC1AD845", "sessionId": self.app, "transportId": "web-" + self.app}] if self.app else []
        return {"type": "RECEIVER_STATUS", "requestId": rid,
                "status": {"applications": apps, "volume": {"level": self.volume, "muted": self.muted}}}

    def media(self, rid=0):
        p = self.play
        if not p:
            return {"type": "MEDIA_STATUS", "requestId": rid, "status": []}
        st = {"mediaSessionId": self.msid, "playerState": "PAUSED" if p.paused and p.state != "IDLE" else p.state, "currentTime": p.now(),
              "volume": {"level": 1, "muted": False}}
        if p.state == "IDLE":
            st["idleReason"] = "FINISHED" if p.ok else "ERROR"
        return {"type": "MEDIA_STATUS", "requestId": rid, "status": [st]}

    def client(self, c, a):
        try:
            c = self.ctx.wrap_socket(c, server_side=True)
            log("cast: connected", a[0])
            c.settimeout(0.5)
            buf, told = b"", None
            send = lambda src, ns, body: c.sendall(pack(src, "*" if not body.get("requestId") else "sender-smp", self.NS + ns, body))
            while True:
                try:
                    x = c.recv(65536)
                    if not x:
                        break
                    buf += x
                except (TimeoutError, ssl.SSLWantReadError):
                    pass
                if self.play and self.play.state == "IDLE" and told is not self.play:
                    told = self.play
                    send("web-" + (self.app or ""), "media", self.media())
                while len(buf) >= 4 and len(buf) >= 4 + struct.unpack(">I", buf[:4])[0]:
                    n = struct.unpack(">I", buf[:4])[0]
                    m = unpack(buf[4:4 + n]); buf = buf[4 + n:]
                    b = json.loads(m[6]); ns = m[4][len(self.NS):]; t = b["type"]; rid = b.get("requestId", 0)
                    if t not in ("PING", "GET_STATUS"):
                        log("cast:", ns, json.dumps(b, ensure_ascii=False)[:900])
                    if t == "PING":
                        send("receiver-0", "tp.heartbeat", {"type": "PONG"})
                    elif ns == "receiver":
                        if t == "LAUNCH":
                            time.sleep(0.4)
                            self.app = "s%d" % int(time.time())
                        elif t == "STOP":
                            self.app = None
                            if self.play:
                                self.play.stop()
                                self.play = None
                        elif t == "SET_VOLUME":
                            self.volume = b["volume"].get("level", self.volume)
                            self.muted = b["volume"].get("muted", self.muted)
                        send("receiver-0", "receiver", self.status(rid))
                    elif ns == "media":
                        src = m[3]
                        if t == "LOAD":
                            md = b["media"]
                            if self.play:
                                self.play.stop()
                                st = self.media(); st["status"][0]["idleReason"] = "INTERRUPTED"; send(src, "media", st)
                            if args.refuse in md["contentType"]:
                                self.play = None
                                log("  refused")
                                send(src, "media", {"type": "LOAD_FAILED", "requestId": rid})
                                continue
                            self.msid += 1
                            self.play = Play(md["contentUrl"], b["currentTime"], not b["autoplay"])
                            threading.Thread(target=check_extras, args=[x["trackContentId"] for x in md.get("tracks", [])] + [x["url"] for x in md["metadata"].get("images", [])], daemon=True).start()
                        elif t == "PLAY": self.play.play()
                        elif t == "PAUSE": self.play.pause()
                        elif t == "SEEK": self.play.seek(b["currentTime"])
                        send(src, "media", self.media(rid))
        except Exception as e:
            log("cast: client gone:", repr(e))
        log("cast: disconnected")

    def mdns(self):
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
        s.bind(("", 5353))
        s.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, socket.inet_aton("224.0.0.251") + socket.inet_aton(IP))
        def name(n):
            return b"".join(bytes([len(x.encode())]) + x.encode() for x in n.split(".")) + b"\0"
        def rr(n, t, d):
            return name(n) + struct.pack(">HHIH", t, 1, 120, len(d)) + d
        svc, inst, host = "_googlecast._tcp.local", "fake-1._googlecast._tcp.local", "fake-host.local"
        txt = b"".join(bytes([len(x.encode())]) + x.encode() for x in ("id=fake1", "fn=" + device("living room TV"), "md=Chromecast", "ca=4101"))
        ans = struct.pack(">HHHHHH", 0, 0x8400, 0, 1, 0, 3) + rr(svc, 12, name(inst)) + rr(inst, 33, struct.pack(">HHH", 0, 0, args.cast_port) + name(host)) + rr(inst, 16, txt) + rr(host, 1, socket.inet_aton(IP))
        while True:
            d, a = s.recvfrom(9000)
            if a[0] == IP and a[1] != 5353 and b"_googlecast" in d:
                s.sendto(ans, a)


# ---- DLNA ----

class Renderer(http.server.BaseHTTPRequestHandler):
    uri, meta, play, volume, mute, ended = "", "", None, 20, 0, False

    def log_message(self, *a):
        pass

    def reply(self, body, code=200):
        b = body.encode()
        self.send_response(code)
        self.send_header("Content-Type", "text/xml")
        self.send_header("Content-Length", str(len(b)))
        self.end_headers()
        self.wfile.write(b)

    def do_GET(self):
        if self.path == "/rc.xml":
            return self.reply("<scpd><serviceStateTable><stateVariable><name>Volume</name><allowedValueRange><minimum>0</minimum><maximum>50</maximum></allowedValueRange></stateVariable></serviceStateTable></scpd>")
        svc = "".join(f"<service><serviceType>urn:schemas-upnp-org:service:{n}:1</serviceType><controlURL>/{n}</controlURL><SCPDURL>/rc.xml</SCPDURL></service>" for n in ("AVTransport", "RenderingControl", "ConnectionManager"))
        self.reply(f'<?xml version="1.0"?><root xmlns="urn:schemas-upnp-org:device-1-0"><device><deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType><friendlyName>{device("bedroom TV")}</friendlyName><manufacturer>{args.prefix or "Samsung"}</manufacturer><modelName>UE40</modelName><UDN>uuid:fake2</UDN><serviceList>{svc}</serviceList></device></root>')

    def do_POST(self):
        R = Renderer
        body = self.rfile.read(int(self.headers["Content-Length"])).decode()
        action = self.headers["SOAPAction"].strip('"').split("#")[1]
        arg = lambda n: html.unescape((re.search(f"<{n}>(.*?)</{n}>", body, re.S) or [0, ""])[1])
        if not action.startswith("Get"):
            log("dlna:", action, {n: arg(n)[:700] for n in ("CurrentURI", "CurrentURIMetaData", "Target", "DesiredVolume", "DesiredMute") if arg(n)})
        out = ""
        p = R.play
        if action == "SetAVTransportURI":
            R.uri, R.meta = arg("CurrentURI"), arg("CurrentURIMetaData")
            if p: p.stop()
            R.play = None
            # Like real TVs: look first, then open it again to play.
            q = urllib.request.Request(R.uri, method="HEAD", headers={"getcontentFeatures.dlna.org": "1", "getCaptionInfo.sec": "1"})
            with urllib.request.urlopen(q) as r:
                log("  HEAD", r.status, r.headers["Content-Type"], r.headers["contentFeatures.dlna.org"], r.headers["CaptionInfo.sec"])
            threading.Thread(target=check_extras, args=re.findall(r"(?:text/srt:\*\">|albumArtURI>)([^<]+)<", R.meta), daemon=True).start()
        elif action == "Play":
            if p and p.paused: p.play()
            elif not p or p.state == "IDLE": R.play = Play(R.uri, 0)
        elif action == "Pause" and p: p.pause()
        elif action == "Stop" and p:
            p.stop(); R.play = None
        elif action == "Seek" and p:
            h, m, s = arg("Target").split(":")
            p.seek(int(h) * 3600 + int(m) * 60 + int(s))
        elif action == "SetVolume": R.volume = int(arg("DesiredVolume"))
        elif action == "SetMute": R.mute = int(arg("DesiredMute"))
        elif action == "GetVolume": out = f"<CurrentVolume>{R.volume}</CurrentVolume>"
        elif action == "GetMute": out = f"<CurrentMute>{R.mute}</CurrentMute>"
        elif action == "GetTransportInfo":
            st = "NO_MEDIA_PRESENT" if not R.uri else "STOPPED" if not p or p.state == "IDLE" else "PAUSED_PLAYBACK" if p.paused else "PLAYING"
            out = f"<CurrentTransportState>{st}</CurrentTransportState><CurrentTransportStatus>OK</CurrentTransportStatus>"
        elif action == "GetPositionInfo":
            t = int(p.now()) if p else 0
            out = f"<RelTime>{t//3600}:{t//60%60:02}:{t%60:02}</RelTime>"
        elif action == "GetProtocolInfo":
            m = ["video/mp4", "video/vnd.dlna.mpeg-tts", "audio/mpeg", "audio/flac"] + ([] if args.no_mkv else ["video/x-matroska"])
            out = "<Sink>" + ",".join(f"http-get:*:{x}:*" for x in m) + "</Sink>"
        self.reply(f'<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/"><s:Body><u:{action}Response xmlns:u="x">{out}</u:{action}Response></s:Body></s:Envelope>')


def ssdp():
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEPORT, 1)
    s.bind(("", 1900))
    s.setsockopt(socket.IPPROTO_IP, socket.IP_ADD_MEMBERSHIP, socket.inet_aton("239.255.255.250") + socket.inet_aton(IP))
    while True:
        d, a = s.recvfrom(4096)
        if a[0] == IP and b"M-SEARCH" in d and b"MediaRenderer" in d:
            s.sendto((f"HTTP/1.1 200 OK\r\nST: urn:schemas-upnp-org:device:MediaRenderer:1\r\nLOCATION: http://{IP}:{args.dlna_port}/desc.xml\r\n\r\n").encode(), a)


Chromecast()
threading.Thread(target=ssdp, daemon=True).start()
log(f"fake Chromecast on {IP}:{args.cast_port}, fake DLNA TV on {IP}:{args.dlna_port}")
try:
    http.server.ThreadingHTTPServer((IP, args.dlna_port), Renderer).serve_forever()
except KeyboardInterrupt:
    sys.exit()
