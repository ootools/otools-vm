#!/bin/bash
# 真启动路径验证：签上 virtualization 权限的 C 宿主加载动态库并起虚拟机。
#
# ctypes 探针（smoke-native.py）拿不到这个权限，只能测到建机/克隆/扩容；
# start 必须由带权限的进程执行，所以有这份脚本。
set -euo pipefail
cd "$(dirname "$0")/.."

LIB="lib/macOS.dylib"
[ -f "$LIB" ] || { echo "缺少 $LIB，先构建 native"; exit 1; }

OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

cc -fobjc-arc scripts/probe-host.m -framework Foundation -o "$OUT/probe-host"

cat > "$OUT/ent.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>com.apple.security.virtualization</key>
    <true/>
    <!-- 插件动态库是 ad-hoc 签名，Team ID 与宿主不同；
         真实 OTools 宿主同样靠这条放行未公证的插件库。 -->
    <key>com.apple.security.cs.disable-library-validation</key>
    <true/>
</dict>
</plist>
PLIST

codesign -s - --force --options runtime --entitlements "$OUT/ent.plist" "$OUT/probe-host" 2>/dev/null
"$OUT/probe-host" "$PWD/$LIB"
