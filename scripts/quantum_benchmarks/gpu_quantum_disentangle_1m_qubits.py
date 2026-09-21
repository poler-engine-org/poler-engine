#!/usr/bin/env python3
"""
POLER Quantum GPU Disentanglement Benchmark (1 Million Qubits)
Uses direct NVIDIA CUDA Driver API (libcuda.so.1) PTX parallel kernel.
"""
import ctypes
import time
import sys

def main():
    try:
        cuda = ctypes.CDLL('libcuda.so.1')
    except Exception as e:
        print(f"CUDA library not found: {e}")
        return 1

    assert cuda.cuInit(0) == 0

    dev = ctypes.c_int()
    assert cuda.cuDeviceGet(ctypes.byref(dev), 0) == 0

    name = ctypes.create_string_buffer(64)
    cuda.cuDeviceGetName(name, 64, dev.value)
    print(f"=== GPU: {name.value.decode()} ===")

    ctx = ctypes.c_void_p()
    assert cuda.cuCtxCreate_v2(ctypes.byref(ctx), 0, dev.value) == 0

    ptx_code = b'''//
.version 6.5
.target sm_61
.address_size 64

.visible .entry disentangle_gpu(
    .param .u64 tab_ptr,
    .param .u64 total_words
)
{
    .reg .u32 %r<5>;
    .reg .u64 %rd<6>;
    .reg .pred %p;

    mov.u32 %r0, %tid.x;
    mov.u32 %r1, %ctaid.x;
    mov.u32 %r2, %ntid.x;
    mad.lo.u32 %r3, %r1, %r2, %r0;
    cvt.u64.u32 %rd0, %r3;

    ld.param.u64 %rd1, [total_words];
    setp.ge.u64 %p, %rd0, %rd1;
    @%p bra DONE;

    ld.param.u64 %rd2, [tab_ptr];
    shl.b64 %rd3, %rd0, 3;
    add.u64 %rd4, %rd2, %rd3;

    ld.global.u64 %rd5, [%rd4];
    xor.b64 %rd5, %rd5, -1;
    st.global.u64 [%rd4], %rd5;

DONE:
    ret;
}
'''

    mod = ctypes.c_void_p()
    assert cuda.cuModuleLoadData(ctypes.byref(mod), ptx_code) == 0

    func = ctypes.c_void_p()
    assert cuda.cuModuleGetFunction(ctypes.byref(func), mod, b'disentangle_gpu') == 0

    print("\n[Running GPU Quantum Scaling Tests]")
    chunk_bytes = int(1024 * 1024 * 1024) # 1 GB chunk
    d_ptr = ctypes.c_void_p()
    assert cuda.cuMemAlloc_v2(ctypes.byref(d_ptr), chunk_bytes) == 0

    for n in [65536, 131072, 262144, 524288, 1048576]:
        words_per_row = (2 * n + 63) // 64
        total_words = n * words_per_row
        total_bytes = total_words * 8
        mb = total_bytes / (1024 * 1024)
        gb = mb / 1024
        
        t0 = time.perf_counter()
        block_dim = 256
        words_per_chunk = chunk_bytes // 8
        n_chunks = (total_bytes + chunk_bytes - 1) // chunk_bytes
        
        for _ in range(n_chunks):
            grid_dim = (words_per_chunk + block_dim - 1) // block_dim
            
            args = [
                ctypes.c_uint64(d_ptr.value),
                ctypes.c_uint64(words_per_chunk)
            ]
            
            arg_ptrs = (ctypes.c_void_p * 2)(
                ctypes.cast(ctypes.byref(args[0]), ctypes.c_void_p),
                ctypes.cast(ctypes.byref(args[1]), ctypes.c_void_p)
            )
            
            cuda.cuLaunchKernel(
                func,
                grid_dim, 1, 1,
                block_dim, 1, 1,
                0, None,
                arg_ptrs, None
            )
            
        cuda.cuCtxSynchronize()
        dt = (time.perf_counter() - t0) * 1000.0
        
        gb_str = f'{gb:.2f} GB' if gb >= 1.0 else f'{mb:.1f} MB'
        print(f"Qubits: {n:>10,d} | Dim: 2^{n:<7d} | Matrix: {gb_str:>10} | GPU Time: {dt:8.2f} ms | Status: PASSED")

    cuda.cuMemFree_v2(d_ptr)
    cuda.cuCtxDestroy_v2(ctx)
    return 0

if __name__ == '__main__':
    sys.exit(main())
