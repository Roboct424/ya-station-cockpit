#!/usr/bin/env python3
"""Fake quasar on OpenSSL with TLS error logging."""
import base64
import datetime
import json
import ssl
import time
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

LOG = open('fake-quasar.log', 'a', buffering=1)

SYNC_INFO = {
    "status": "ok",
    "config": {
        "account_config": {"spotter": "alisa"},
        "device_config": {"name": "quasar"},
        "system_config": {"adbd": {"remoteActivation": True, "network": True}},
    },
    "subscription": {"mode": "transaction"},
}


class H(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        LOG.write(f"{datetime.datetime.now().isoformat()} {self.address_string()} {fmt % args}\n")

    def _json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        p = urlparse(self.path)
        LOG.write(f"GET {self.path} sni={getattr(self.connection, 'server_hostname', None)}\n")
        if p.path.endswith('/get_sync_info'):
            LOG.write("  -> sync info with adbd\n")
            self._json(SYNC_INFO)
        elif p.path.endswith('/check_updates'):
            self._json({"hasUpdate": False})
        else:
            self._json({"status": "ok"})

    def _token(self):
        # OAuth-ответ: без него станция не завершает регистрацию.
        def b64(obj):
            raw = json.dumps(obj, separators=(",", ":")).encode()
            return base64.urlsafe_b64encode(raw).decode().rstrip("=")

        now = int(time.time())
        access = ".".join([
            b64({"alg": "none", "typ": "JWT"}),
            b64({"iss": "cockpit", "sub": "offline", "iat": now, "exp": now + 31536000}),
            "cockpit",
        ])
        refresh = ".".join([
            b64({"alg": "none", "typ": "JWT"}),
            b64({"iss": "cockpit", "typ": "refresh", "iat": now, "exp": now + 31536000}),
            "cockpit",
        ])
        self._json({
            "access_token": access,
            "token_type": "bearer",
            "expires_in": 31536000,
            "refresh_token": refresh,
            "scope": "quasar",
        })

    def do_POST(self):
        length = int(self.headers.get('Content-Length') or 0)
        body = self.rfile.read(length) if length else b''
        LOG.write(f"POST {self.path} {body[:300]!r}\n")
        path = urlparse(self.path).path
        if path.endswith('/token') or path == '/token':
            LOG.write("  -> OAuth token\n")
            self._token()
        elif 'push_subscribe' in path:
            self._json({"status": "ok"})
        else:
            self._json({"status": "ok"})


class Server(ThreadingHTTPServer):
    ctx = None

    def get_request(self):
        sock, addr = super().get_request()
        try:
            return self.ctx.wrap_socket(sock, server_side=True), addr
        except Exception as exc:  # noqa: BLE001
            LOG.write(f"{datetime.datetime.now().isoformat()} TLS fail {addr}: {exc}\n")
            raise


def main():
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 443
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.maximum_version = ssl.TLSVersion.TLSv1_2
    ctx.load_cert_chain('quasar-chain.pem', 'quasar-leaf.key')
    Server.ctx = ctx
    httpd = Server(('0.0.0.0', port), H)
    LOG.write(f"listen on :{port} (TLS 1.2, OpenSSL {ssl.OPENSSL_VERSION})\n")
    httpd.serve_forever()


if __name__ == '__main__':
    main()
