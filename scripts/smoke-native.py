#!/usr/bin/env python3
"""
插件原生层的本地冒烟测试。

不经宿主、直接用 ctypes 调 `otools_plugin_invoke`，覆盖：
  ping → 引擎信息 → 建机 → 克隆（校验写时复制）→ 扩容（含拒绝缩容）→ 删除

为什么不用 cargo test：这些路径绝大多数是对 VZ / 文件系统的真实调用，
单元测试只能覆盖纯逻辑（derive_tag 之类）。这里跑的是**真动态库 + 真磁盘**。

用法：
  cd native && cargo build --release
  cp target/release/libotools_vm_native.dylib ../lib/macOS.dylib
  python3 scripts/smoke-native.py
"""

import ctypes
import json
import os
import pathlib
import sys
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
LIB = ROOT / "lib" / "macOS.dylib"

if not LIB.exists():
    sys.exit(f"找不到动态库 {LIB}，先 cargo build --release 并拷贝到 lib/macOS.dylib")

lib = ctypes.CDLL(str(LIB))
lib.otools_plugin_invoke.restype = ctypes.POINTER(ctypes.c_ubyte)
lib.otools_plugin_invoke.argtypes = [
    ctypes.c_char_p,
    ctypes.c_size_t,
    ctypes.POINTER(ctypes.c_size_t),
]
lib.otools_plugin_free.argtypes = [ctypes.POINTER(ctypes.c_ubyte), ctypes.c_size_t]

failures = []


def call(method, payload=None):
    raw = json.dumps({"method": method, "payload": payload}).encode()
    size = ctypes.c_size_t()
    ptr = lib.otools_plugin_invoke(raw, len(raw), ctypes.byref(size))
    if not ptr:
        raise RuntimeError(f"{method} 返回空指针")
    data = bytes(bytearray(ptr[i] for i in range(size.value)))
    lib.otools_plugin_free(ptr, size.value)
    return json.loads(data)


def check(label, condition, detail=""):
    mark = "✓" if condition else "✗"
    print(f"  {mark} {label}{('  ' + detail) if detail else ''}")
    if not condition:
        failures.append(label)


def free_mb():
    st = os.statvfs(str(pathlib.Path.home()))
    return st.f_bavail * st.f_frsize / 1024 / 1024


def main():
    print("基础命令")
    ping = call("ping")
    check("ping 返回 pong", ping.get("data", {}).get("message") == "pong")

    info = call("vm_engine_info")["data"]
    check("引擎可用", info["available"], f"{info['kind']} / {info['label']} {info['version']}")

    ws = call("vm_get_workspace")["data"]
    check("工作区含能力矩阵", "clone" in ws["engine"]["caps"])

    unknown = call("nope")
    check("未知方法被拒", unknown.get("ok") is False)

    print("Rosetta")
    rosetta = call("vm_rosetta_status")["data"]["status"]
    check("状态是已知枚举", rosetta in ("installed", "notInstalled", "notSupported"), rosetta)

    print("创建与写盘")
    src = call(
        "vm_create",
        {
            "name": "冒烟母机",
            "os": "linux",
            "arch": "arm64",
            "cpus": 2,
            "memoryMb": 2048,
            "diskSizeGb": 40,
            "installerPath": None,
            "sharedDirs": [],
            "audio": False,
            "rosetta": False,
        },
    )
    if not src.get("ok"):
        check("建机", False, str(src.get("error")))
        return
    src = src["data"]
    src_disk = pathlib.Path(src["diskPath"])
    check("磁盘是稀疏文件", src_disk.exists() and src_disk.stat().st_blocks == 0)

    # 写 300MB，克隆才有可共享的块。
    with open(src_disk, "r+b") as handle:
        handle.seek(0)
        handle.write(b"\xAB" * (300 * 1024 * 1024))
    check("写入后占 300MB 量级", 280 <= src_disk.stat().st_blocks * 512 / 1024 / 1024 <= 320)

    print("克隆（写时复制）")
    before = free_mb()
    started = time.time()
    cloned = call("vm_clone", {"id": src["id"], "name": "冒烟副本"})
    elapsed = time.time() - started
    if not cloned.get("ok"):
        check("克隆", False, str(cloned.get("error")))
        return
    clone = cloned["data"]
    used = before - free_mb()

    check("克隆成功", clone["id"] != src["id"])
    check("耗时 < 2s", elapsed < 2.0, f"{elapsed * 1000:.0f} ms")
    # APFS clonefile 共享块，剩余空间几乎不变；整盘复制会吃掉 ~300MB。
    check("几乎没有额外占用", abs(used) < 60, f"{used:+.1f} MB")
    check(
        "副本逻辑容量一致",
        pathlib.Path(clone["diskPath"]).stat().st_size == src_disk.stat().st_size,
    )

    # 副本写入后才有独立块。
    clone_disk = pathlib.Path(clone["diskPath"])
    before = free_mb()
    with open(clone_disk, "r+b") as handle:
        handle.seek(0)
        handle.write(b"\xCD" * (100 * 1024 * 1024))
    grown = before - free_mb()
    check("副本写入只分配新块", 80 <= grown <= 130, f"{grown:.1f} MB")

    print("扩容")
    resized = call("vm_resize_disk", {"id": clone["id"], "diskSizeGb": 80})
    if resized.get("ok"):
        check("扩容到 80GB", resized["data"]["diskSizeGb"] == 80)
        check(
            "文件长度同步",
            pathlib.Path(resized["data"]["diskPath"]).stat().st_size == 80 * 1024**3,
        )
    else:
        check("扩容", False, str(resized.get("error")))

    shrink = call("vm_resize_disk", {"id": clone["id"], "diskSizeGb": 10})
    check("拒绝缩容", shrink.get("ok") is False, str(shrink.get("error")))

    print("清理")
    call("vm_delete", {"id": src["id"]})
    call("vm_delete", {"id": clone["id"]})
    remaining = call("vm_get_workspace")["data"]["vms"]
    check("列表已清空", not any(v["id"] in (src["id"], clone["id"]) for v in remaining))
    check("目录已删除", not pathlib.Path(src["diskPath"]).parent.exists())

    print()
    if failures:
        print(f"✗ {len(failures)} 项失败：{', '.join(failures)}")
        return 1
    print("✓ 全部通过")
    return 0


if __name__ == "__main__":
    sys.exit(main())
