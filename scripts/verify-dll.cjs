// 加载 dll 并真调一次命令，确认产物可用。
//
// 用法：node scripts/verify-dll.cjs [dll 路径]
//   默认验证仓库里的 lib/Windows.dll；传路径可验证「装到 ~/.otools 的那份」。
//
// 为什么不用「搜 dll 里的字符串」：Rust 会做字符串前缀共享与内联，
// `vm_set_engine_preference` 这种早就在的老命令都搜不到明文，结论完全不可信。
// 唯一可靠的验证是真的把 dll 加载起来调一遍。
//
// 注意：.ps1 里的内容必须是纯 ASCII —— 本机 PowerShell 按 ANSI 读脚本文件，
// 混入中文会直接把解析器搞崩（报一堆「缺少 }」的假错误）。

const path = require('path');
const fs = require('fs');
const os = require('os');
const { spawn } = require('child_process');

const target = process.argv[2] || path.resolve(__dirname, '..', 'lib', 'Windows.dll');
if (!fs.existsSync(target)) {
  console.error('找不到 dll:', target);
  process.exit(1);
}
const DLL = path.resolve(target).replace(/\\/g, '\\\\');
console.log('验证目标:', path.resolve(target));

const script = `
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class OtoolsFfi {
    [DllImport(@"${DLL}", CallingConvention = CallingConvention.Cdecl)]
    public static extern IntPtr otools_plugin_invoke(byte[] input, UIntPtr inputLen, out UIntPtr outputLen);
    [DllImport(@"${DLL}", CallingConvention = CallingConvention.Cdecl)]
    public static extern void otools_plugin_free(IntPtr ptr, UIntPtr len);

    public static string Call(string json) {
        byte[] bytes = Encoding.UTF8.GetBytes(json);
        UIntPtr outLen;
        IntPtr result = otools_plugin_invoke(bytes, (UIntPtr)bytes.Length, out outLen);
        if (result == IntPtr.Zero) return null;
        int len = (int)outLen;
        byte[] outBytes = new byte[len];
        Marshal.Copy(result, outBytes, 0, len);
        otools_plugin_free(result, outLen);
        return Encoding.UTF8.GetString(outBytes);
    }
}
"@

# UTF-8 输出，避免中文提示被控制台代码页弄成乱码
[Console]::OutputEncoding = [System.Text.Encoding]::UTF8

$cases = @(
    @{ n = 'vm_gl_support'; j = '{"method":"vm_gl_support","payload":{}}' },
    @{ n = 'vm_engine_info'; j = '{"method":"vm_engine_info","payload":{}}' },
    @{ n = 'vm_hcs_orphans'; j = '{"method":"vm_hcs_orphans","payload":{}}' }
)

foreach ($c in $cases) {
    try {
        $r = [OtoolsFfi]::Call($c.j)
        if ($null -eq $r) { Write-Output ($c.n + ' => NULL POINTER'); continue }
        Write-Output ($c.n + ' => ' + $r)
    } catch {
        Write-Output ($c.n + ' => EXCEPTION: ' + $_.Exception.Message)
    }
}
`;

const file = path.join(os.tmpdir(), `verify-dll-${Date.now()}.ps1`);
fs.writeFileSync(file, script, 'ascii');

const child = spawn('powershell', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', file], {
  stdio: 'inherit',
});
child.on('exit', (code) => {
  try {
    fs.unlinkSync(file);
  } catch {}
  process.exit(typeof code === 'number' ? code : 0);
});
