"""本地假 OneBot v11 服务，用来在没有 NapCat 的情况下验证 codex-notify。

    python tests/mock_onebot.py          # 监听 127.0.0.1:3000
    codex-notify.exe --check
    codex-notify.exe --test

收到的每个请求都会打印方法和 body，能直接看到最终发出去的文案。
"""
import json
from http.server import BaseHTTPRequestHandler, HTTPServer

OK = {"status": "ok", "retcode": 0, "data": {"message_id": 1}}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _reply(self, payload):
        raw = json.dumps(payload).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length).decode("utf-8", "replace")
        print(f"POST {self.path}", flush=True)
        print(f"  Authorization: {self.headers.get('Authorization')}", flush=True)
        print(f"  body: {body}", flush=True)
        self._reply(OK)

    def do_GET(self):
        print(f"GET {self.path}", flush=True)
        self._reply(
            {"status": "ok", "retcode": 0, "data": {"user_id": 10001, "nickname": "mock"}}
        )

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    print("mock OneBot listening on http://127.0.0.1:3000", flush=True)
    HTTPServer(("127.0.0.1", 3000), Handler).serve_forever()
