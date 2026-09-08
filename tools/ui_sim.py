#!/usr/bin/env python3
"""Headless-симулятор пульта оператора — автономная отладка контура.

Ведёт себя как настоящий operator-ui (ADR-016): слушает :9000 (видео-push)
и :9010 (control); борт подключается сам (push-архитектура). Держит
keep-alive `{"t":"ping"}` каждые 300 мс — fail-safe борта: тишина UI >1 c
при armed = авто-СТОП.

Сценарий передаётся аргументами:
    arm | stop | unlock | lock=X,Y,SIZE | wait=SEC | status | quit
Пример:
    python tools/ui_sim.py wait=3 status arm wait=8 status stop status quit

Вывод: строки статусов борта (JSON одной строкой) и сводка на выходе
(видео-кадры/байты, armed-переходы, команды).
"""
import json
import os
import socket
import sys
import threading
import time

CTRL_PORT = 9010
VID_PORT = 9000
CONN_TIMEOUT = 25.0


class UiSim:
    def __init__(self):
        self.status = {}
        self.status_lines = []
        self.video_bytes = 0
        self.video_jpegs = 0
        self.ctrl_sock = None
        self.events = []
        self._writer_stop = threading.Event()
        self._send_lock = threading.Lock()
        self._ctrl_ready = threading.Event()
        self._vid_ready = threading.Event()

    def _log(self, msg):
        print('[%6.2f] %s' % (time.time() - self.t0, msg), flush=True)

    def _ctrl_server(self):
        srv = socket.socket()
        srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        srv.bind(('0.0.0.0', CTRL_PORT))
        srv.listen(1)
        srv.settimeout(CONN_TIMEOUT)
        try:
            conn, addr = srv.accept()
        except Exception as e:
            self._log('control: нет подключения борта: %r' % e)
            return
        self._log('control: борт подключился %s' % (addr[0],))
        self.ctrl_sock = conn
        self._ctrl_ready.set()
        conn.settimeout(0.2)
        buf = b''
        while not self._writer_stop.is_set():
            try:
                d = conn.recv(4096)
                if not d:
                    self._log('control: борт закрыл соединение')
                    break
                buf += d
                while b'\n' in buf:
                    line, buf = buf.split(b'\n', 1)
                    if not line.strip():
                        continue
                    try:
                        st = json.loads(line)
                    except ValueError:
                        continue
                    prev_armed = self.status.get('armed')
                    self.status = st
                    self.status_lines.append(st)
                    if prev_armed is not None and st.get('armed') != prev_armed:
                        self._log('armed: %s -> %s' % (prev_armed, st.get('armed')))
                        self.events.append((time.time(), 'armed=%s' % st.get('armed')))
            except socket.timeout:
                pass
            except OSError:
                break
        try:
            conn.close()
        except OSError:
            pass

    def _video_server(self):
        srv = socket.socket()
        srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        srv.bind(('0.0.0.0', VID_PORT))
        srv.listen(1)
        srv.settimeout(CONN_TIMEOUT)
        try:
            conn, addr = srv.accept()
        except Exception:
            return
        self._log('video: борт подключился %s' % (addr[0],))
        self._vid_ready.set()
        conn.settimeout(1.0)
        while not self._writer_stop.is_set():
            try:
                d = conn.recv(65536)
                if not d:
                    break
                self.video_bytes += len(d)
                self.video_jpegs += d.count(b'\xff\xd8\xff')
            except socket.timeout:
                continue
            except OSError:
                break
        try:
            conn.close()
        except OSError:
            pass

    def _frame(self, cmd_json):
        """Команда/ping с общим секретом канала (safety §6.3), если задан."""
        tok = os.environ.get('SYNERGY_TOKEN', '')
        if tok:
            obj = json.loads(cmd_json)
            obj['auth'] = tok
            cmd_json = json.dumps(obj)
        return (cmd_json + '\n').encode()

    def _keepalive(self):
        last = time.time()
        while not self._writer_stop.is_set():
            time.sleep(0.05)
            if time.time() - last >= 0.3 and self.ctrl_sock:
                with self._send_lock:
                    try:
                        self.ctrl_sock.sendall(self._frame('{"t":"ping"}'))
                    except OSError:
                        return
                last = time.time()

    def send(self, cmd_json):
        if not self._ctrl_ready.wait(5.0):
            self._log('send: control-канал не готов — команда пропущена')
            return False
        with self._send_lock:
            try:
                self.ctrl_sock.sendall(self._frame(cmd_json))
                self._log('>> ' + cmd_json)
                self.events.append((time.time(), cmd_json))
                return True
            except OSError as e:
                self._log('send ошибка: %r' % e)
                return False

    def run(self, script):
        self.t0 = time.time()
        threading.Thread(target=self._video_server, daemon=True).start()
        threading.Thread(target=self._ctrl_server, daemon=True).start()
        threading.Thread(target=self._keepalive, daemon=True).start()

        for item in script:
            if item.startswith('wait='):
                time.sleep(float(item[5:]))
            elif item.startswith('lock='):
                x, y, s = item[5:].split(',')
                self.send('{"t":"lock","x":%s,"y":%s,"size":%s}' % (x, y, s))
            elif item == 'arm':
                self.send('{"t":"arm","on":true}')
            elif item == 'stop':
                self.send('{"t":"stop"}')
            elif item == 'unlock':
                self.send('{"t":"unlock"}')
            elif item == 'status':
                st = dict(self.status)
                st.pop('dets', None)
                self._log('status: ' + json.dumps(st, ensure_ascii=False))
            elif item == 'quit':
                break
            else:
                self._log('неизвестная команда: ' + item)
        self._writer_stop.set()
        time.sleep(0.3)
        print('== сводка: видео %.1f МБ, JPEG-маркеров %d, статусов %d' %
              (self.video_bytes / 1e6, self.video_jpegs, len(self.status_lines)))
        for ts, ev in self.events:
            print('== %6.2f %s' % (ts - self.t0, ev))


if __name__ == '__main__':
    args = sys.argv[1:]
    if '--token' in args:
        i = args.index('--token')
        os.environ['SYNERGY_TOKEN'] = args[i + 1]
        del args[i:i + 2]
    if not args:
        print(__doc__)
        sys.exit(1)
    UiSim().run(args)
