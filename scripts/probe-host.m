// 带 virtualization 权限的最小宿主，用来真跑一遍启动路径。
//
// 为什么需要它：`com.apple.security.virtualization` 属于**进程**，Python 解释器
// 拿不到，所以 ctypes 探针只能测到「建机 / 克隆 / 扩容」，永远测不到 start。
// 这个 C 宿主 dlopen 插件动态库、签上权限、驱动主运行循环，把 start 也覆盖到。
//
// 用法（见 scripts/probe-start.sh）：
//   cc -fobjc-arc probe-host.m -framework Foundation -o probe-host
//   codesign -s - --force --options runtime --entitlements ent.plist probe-host
//   ./probe-host <dylib 路径>

#import <Foundation/Foundation.h>
#import <dispatch/dispatch.h>
#include <dlfcn.h>

typedef unsigned char *(*InvokeFn)(const unsigned char *, size_t, size_t *);
typedef void (*FreeFn)(unsigned char *, size_t);

static InvokeFn gInvoke = NULL;
static FreeFn gFree = NULL;

static NSDictionary *Call(NSString *method, id payload) {
  NSDictionary *request = @{@"method": method, @"payload": payload ?: [NSNull null]};
  NSData *json = [NSJSONSerialization dataWithJSONObject:request options:0 error:nil];
  size_t outLen = 0;
  unsigned char *out = gInvoke(json.bytes, json.length, &outLen);
  if (!out) {
    return @{@"ok": @NO, @"error": @"null response"};
  }
  NSData *data = [NSData dataWithBytes:out length:outLen];
  gFree(out, outLen);
  id parsed = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
  return [parsed isKindOfClass:[NSDictionary class]] ? parsed : @{@"ok": @NO};
}

int main(int argc, const char *argv[]) {
  @autoreleasepool {
    if (argc < 2) {
      fprintf(stderr, "用法: probe-host <libotools_vm_native.dylib>\n");
      return 64;
    }

    void *handle = dlopen(argv[1], RTLD_NOW);
    if (!handle) {
      fprintf(stderr, "dlopen 失败: %s\n", dlerror());
      return 65;
    }
    gInvoke = (InvokeFn)dlsym(handle, "otools_plugin_invoke");
    gFree = (FreeFn)dlsym(handle, "otools_plugin_free");
    if (!gInvoke || !gFree) {
      fprintf(stderr, "找不到 FFI 符号\n");
      return 66;
    }

    NSDictionary *ping = Call(@"ping", nil);
    printf("ping: %s\n", [ping[@"data"][@"message"] UTF8String] ?: "(nil)");
    fflush(stdout);

    NSDictionary *created = Call(@"vm_create", @{
      @"name": @"权限探针",
      @"os": @"linux",
      @"arch": @"arm64",
      @"cpus": @2,
      @"memoryMb": @2048,
      @"diskSizeGb": @5,
      @"installerPath": [NSNull null],
      @"sharedDirs": @[],
      @"audio": @NO,
      @"rosetta": @NO,
      @"clipboard": @YES,
    });
    if (![created[@"ok"] boolValue]) {
      printf("建机失败: %s\n", [created[@"error"] UTF8String]);
      return 1;
    }
    NSString *vmId = created[@"data"][@"id"];
    printf("建机 ok: %s\n", [vmId UTF8String]);
    fflush(stdout);

    NSDictionary *started = Call(@"vm_start", @{@"id": vmId});
    printf("start: %s\n", [started[@"ok"] boolValue] ? "ok" : [started[@"error"] UTF8String]);
    fflush(stdout);
    if (![started[@"ok"] boolValue]) {
      Call(@"vm_delete", @{@"id": vmId});
      return 1;
    }

    // 驱动主运行循环，让 VZ 的 completion handler 有机会执行。
    __block BOOL running = NO;
    NSDate *deadline = [NSDate dateWithTimeIntervalSinceNow:20];
    while ([deadline timeIntervalSinceNow] > 0) {
      [[NSRunLoop mainRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.5]];
      NSDictionary *ws = Call(@"vm_get_workspace", nil);
      NSArray *vms = ws[@"data"][@"vms"];
      for (NSDictionary *entry in vms) {
        if ([entry[@"id"] isEqualToString:vmId]) {
          NSString *state = entry[@"state"];
          NSString *lastError = entry[@"lastError"];
          printf("  state=%s%s%s\n", [state UTF8String],
                 lastError.length ? "  err=" : "",
                 lastError.length ? [lastError UTF8String] : "");
          fflush(stdout);
          if ([state isEqualToString:@"running"]) {
            running = YES;
          }
          break;
        }
      }
      if (running) break;
    }

    printf("停止: %s\n", [Call(@"vm_stop", @{@"id": vmId})[@"ok"] boolValue] ? "ok" : "fail");
    [[NSRunLoop mainRunLoop] runUntilDate:[NSDate dateWithTimeIntervalSinceNow:2.0]];
    Call(@"vm_delete", @{@"id": vmId});

    printf("结果: %s\n", running ? "✓ 客户机已进入 running" : "✗ 未进入 running");
    return running ? 0 : 1;
  }
}
