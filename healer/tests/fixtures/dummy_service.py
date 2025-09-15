import http.server, socketserver, sys
socketserver.TCPServer.allow_reuse_address = True
PORT=8080
class H(http.server.SimpleHTTPRequestHandler):
    def do_GET(self):
        if self.path == '/health':
            self.send_response(200); self.end_headers(); self.wfile.write(b'OK')
        elif self.path == '/crash':
            self.send_response(200); self.end_headers(); self.wfile.write(b'DIE'); sys.exit(1)
        else:
            self.send_response(404); self.end_headers()
with socketserver.TCPServer(("", PORT), H) as srv:
    srv.serve_forever()
