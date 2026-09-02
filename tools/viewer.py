#!/usr/bin/env python3
"""Приёмник push-стрима synergy и раздача в браузер (zero зависимостей).

Использование:
    python tools/viewer.py                 # push:9000, браузер: http://127.0.0.1:9001/
    python tools/viewer.py 9100 9101       # свои порты

На борту:
    ./target/release/synergy --duration 0 --stream-push <ip-этого-ПК>:9000

Зачем: на vendor-ядре RK3588S при активном NPU не проходят новые входящие
TCP к пользовательским листенерам борта, а исходящие — всегда; поэтому борт
сам подключается сюда (ADR-009), а этот скрипт просто раздаёт поток дальше.
"""
import socket
import sys
import threading

PUSH_PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 9000
HTTP_PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 9001

clients = []  # сокеты браузеров, ждущих /stream
lock = threading.Lock()

PAGE = (
    b"<!DOCTYPE html><html><head><title>synergy (push)</title><style>"
    b"html,body{margin:0;height:100%;background:#000;display:flex;"
    b"align-items:center;justify-content:center}"
    b"img{max-width:100%;max-height:100%}"
    b"</style></head><body><img src='/stream'></body></html>"
)


def handle_push():
    """Принимаем подключение борта и транслируем байты всем браузерам."""
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("0.0.0.0", PUSH_PORT))
    srv.listen(1)
    print(f"[viewer] жду push от борта на :{PUSH_PORT} (браузер: http://127.0.0.1:{HTTP_PORT}/)")
    while True:
        conn, addr = srv.accept()
        print(f"[viewer] push подключился: {addr}")
        try:
            while True:
                chunk = conn.recv(65536)
                if not chunk:
                    break
                with lock:
                    dead = []
                    for c in clients:
                        try:
                            c.sendall(chunk)
                        except OSError:
                            dead.append(c)
                    for c in dead:
                        clients.remove(c)
                        c.close()
        except OSError:
            pass
        finally:
            conn.close()
            print("[viewer] push отключился, жду переподключения (борт ретраит каждые 3 с)")


def serve_browser(conn):
    """HTTP: / — страница-обёртка, /stream — живой multipart."""
    try:
        req = conn.recv(4096).decode("latin1", "replace")
        first = req.split("\r\n")[0]
        conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        if not first.startswith("GET /stream"):
            head = (
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n"
                f"Content-Length: {len(PAGE)}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
            ).encode()
            conn.sendall(head + PAGE)
            conn.close()
            return
        with lock:
            conn.sendall(
                b"HTTP/1.1 200 OK\r\n"
                b"Content-Type: multipart/x-mixed-replace; boundary=frame\r\n"
                b"Cache-Control: no-store\r\nConnection: close\r\n\r\n"
            )
            clients.append(conn)
        # Сокет держим открытым; данные пишет поток push-приёмника.
        while True:
            threading.Event().wait(3600)
    except OSError:
        pass
    finally:
        with lock:
            if conn in clients:
                clients.remove(conn)
        try:
            conn.close()
        except OSError:
            pass


def main():
    threading.Thread(target=handle_push, daemon=True).start()
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("0.0.0.0", HTTP_PORT))
    srv.listen(4)
    print(f"[viewer] HTTP на :{HTTP_PORT}")
    while True:
        conn, _ = srv.accept()
        threading.Thread(target=serve_browser, args=(conn,), daemon=True).start()


if __name__ == "__main__":
    main()
