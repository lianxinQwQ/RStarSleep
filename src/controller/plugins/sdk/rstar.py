"""Synchronous save of the current RStarSleep plugin's persistent volumes."""
import json
import os
import socket


def save():
    """Flush and quiesce plugin writers before calling; raise on any unconfirmed save."""
    address = os.environ.get("RSTAR_CONTROL_SOCKET")
    if not address:
        raise RuntimeError('RStarSleep 保存功能在受管理的插件调用之外不可用 / RStarSleep save is unavailable outside a managed plugin call')
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.connect(address)
        stream.sendall(b'{"version":1,"id":1,"operation":"save"}\n')
        response = bytearray()
        while b"\n" not in response:
            chunk = stream.recv(4096)
            if not chunk:
                raise RuntimeError('保存确认丢失；请停止此操作 / Save acknowledgement lost; stop this operation')
            response.extend(chunk)
            if len(response) > 16384:
                raise RuntimeError('保存确认无效 / Invalid save acknowledgement')
    result = json.loads(response.split(b"\n", 1)[0])
    if (not isinstance(result, dict) or type(result.get("id")) is not int
            or result["id"] != 1 or type(result.get("ok")) is not bool):
        raise RuntimeError('保存确认无效 / Invalid save acknowledgement')
    if not result["ok"]:
        raise RuntimeError(result.get("error", '保存失败 / Save failed'))
