"""CUDA vector addition through the installed NVIDIA driver; no Python packages or nvcc needed."""
import ctypes as C
import json

cuda = C.CDLL("libcuda.so.1")

def call(name, *args):
    status = getattr(cuda, name)(*args)
    if status:
        raise RuntimeError(f"{name} failed with CUDA error {status}")

# An explicit PTX kernel runs on the GPU; copying CPU-computed answers is not a GPU test.
ptx = b"""
.version 6.0
.target sm_52
.address_size 64
.visible .entry add(.param .u64 a, .param .u64 b, .param .u64 out) {
.reg .u32 %r<8>;
.reg .u64 %rd<8>;
ld.param.u64 %rd1, [a];
ld.param.u64 %rd2, [b];
ld.param.u64 %rd3, [out];
mov.u32 %r1, %ctaid.x;
mov.u32 %r2, %ntid.x;
mov.u32 %r3, %tid.x;
mad.lo.u32 %r4, %r1, %r2, %r3;
mul.wide.u32 %rd4, %r4, 4;
add.u64 %rd5, %rd1, %rd4;
add.u64 %rd6, %rd2, %rd4;
add.u64 %rd7, %rd3, %rd4;
ld.global.u32 %r5, [%rd5];
ld.global.u32 %r6, [%rd6];
add.u32 %r7, %r5, %r6;
st.global.u32 [%rd7], %r7;
ret;
}
"""

call("cuInit", C.c_uint(0))
device = C.c_int()
call("cuDeviceGet", C.byref(device), C.c_int(0))
name = C.create_string_buffer(256)
call("cuDeviceGetName", name, C.c_int(len(name)), device)
context = C.c_void_p()
call("cuCtxCreate_v2", C.byref(context), C.c_uint(0), device)
allocations = []
module = C.c_void_p()
try:
    call("cuModuleLoadData", C.byref(module), C.c_char_p(ptx))
    kernel = C.c_void_p()
    call("cuModuleGetFunction", C.byref(kernel), module, C.c_char_p(b"add"))
    count = 4096
    array = C.c_uint32 * count
    a = array(*range(count))
    b = array(*(i * 2 for i in range(count)))
    out = array()
    for host in (a, b, out):
        pointer = C.c_uint64()
        call("cuMemAlloc_v2", C.byref(pointer), C.c_size_t(C.sizeof(host)))
        allocations.append(pointer)
    for pointer, host in zip(allocations, (a, b)):
        call("cuMemcpyHtoD_v2", pointer, C.cast(host, C.c_void_p), C.c_size_t(C.sizeof(host)))
    parameters = (C.c_void_p * 3)(*(C.cast(C.byref(p), C.c_void_p) for p in allocations))
    call("cuLaunchKernel", kernel, C.c_uint(count // 256), C.c_uint(1), C.c_uint(1), C.c_uint(256), C.c_uint(1), C.c_uint(1), C.c_uint(0), C.c_void_p(), parameters, C.c_void_p())
    call("cuCtxSynchronize")
    call("cuMemcpyDtoH_v2", C.cast(out, C.c_void_p), allocations[2], C.c_size_t(C.sizeof(out)))
    assert list(out) == [i * 3 for i in range(count)], "GPU returned incorrect results"
    report = {"backend": "cuda", "device": name.value.decode(), "count": count, "checksum": sum(out)}
    with open("result.json", "w", encoding="utf-8") as result:
        json.dump(report, result, sort_keys=True)
        result.write("\n")
    print(json.dumps(report, sort_keys=True))
finally:
    for pointer in allocations:
        call("cuMemFree_v2", pointer)
    if module:
        call("cuModuleUnload", module)
    call("cuCtxDestroy_v2", context)
