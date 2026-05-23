#!/usr/bin/env python3
"""
qperf-bench.py — Run QEMU with qperf plugin, inject bench-virtio-blk workload.
Usage: python3 scripts/qperf-bench.py <original|patched> <output_dir>
"""
import subprocess, sys, os, time, select, signal

VARIANT = sys.argv[1]
OUTDIR = sys.argv[2]

WORKDIR = "/work"
KERNEL_BIN = f"{WORKDIR}/target/qperf/kernels/starryos-{VARIANT}.bin"
ELF = f"{WORKDIR}/target/qperf/kernels/starryos-{VARIANT}"
ROOTFS = f"{WORKDIR}/target/rootfs/rootfs-riscv64-alpine.img"
PLUGIN = f"{WORKDIR}/tools/qperf/target/release/libqperf.so"
QPERF_OUT = f"{WORKDIR}/{OUTDIR}/qperf.bin"

os.makedirs(f"{WORKDIR}/{OUTDIR}", exist_ok=True)

print(f"=== qperf-bench: {VARIANT} ===")
print(f"kernel: {KERNEL_BIN}")
print(f"out:    {QPERF_OUT}")

cmd = [
    "qemu-system-riscv64",
    "-machine", "virt",
    "-m", "512M",
    "-nographic",
    "-cpu", "rv64",
    "-bios", "none",
    "-device", "virtio-blk-pci,drive=disk0",
    "-drive", f"id=disk0,if=none,format=raw,file={ROOTFS}",
    "-device", "virtio-net-pci,netdev=net0",
    "-netdev", "user,id=net0",
    "-kernel", KERNEL_BIN,
    "-plugin", f"{PLUGIN},freq=999,max_depth=64,queue_size=4096,out={QPERF_OUT}",
]

proc = subprocess.Popen(
    cmd, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT
)

shell_detected = False
bench_sent = False
bench_done = False
output_lines = []

try:
    while True:
        ready, _, _ = select.select([proc.stdout], [], [], 1.0)
        if ready:
            data = proc.stdout.readline()
            if not data:
                break
            line = data.decode("utf-8", errors="replace").rstrip()
            print(line)

            if not shell_detected and "root@starry:" in line:
                shell_detected = True
                print(">>> SHELL DETECTED <<<", file=sys.stderr)

            if shell_detected and not bench_sent:
                time.sleep(1)
                proc.stdin.write(b"/usr/bin/bench-virtio-blk\n")
                proc.stdin.flush()
                bench_sent = True
                print(">>> BENCHMARK INJECTED <<<", file=sys.stderr)

            if "BENCH_PASS" in line:
                bench_done = True
                print(">>> BENCHMARK PASSED, waiting 3s for extra samples <<<", file=sys.stderr)
                time.sleep(3)
                break

            if "BENCH_FAIL" in line:
                print(">>> BENCHMARK FAILED <<<", file=sys.stderr)
                break

        # Check if process died
        if proc.poll() is not None:
            break

        # Timeout after 5 minutes
        if bench_sent and not bench_done:
            pass  # keep waiting
except Exception as e:
    print(f"Error: {e}", file=sys.stderr)
finally:
    print(">>> Stopping QEMU <<<", file=sys.stderr)
    proc.terminate()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait()

print(f"QEMU exited with code: {proc.returncode}")

if not os.path.exists(QPERF_OUT) or os.path.getsize(QPERF_OUT) == 0:
    print("ERROR: qperf.bin is empty!")
    sys.exit(1)

size = os.path.getsize(QPERF_OUT)
print(f"qperf.bin: {size} bytes")

# Run analyzer
FOLDED = f"{WORKDIR}/{OUTDIR}/stack.folded"
ANALYZER = f"{WORKDIR}/tools/qperf/target/release/qperf-analyzer"
subprocess.run([ANALYZER, "-e", ELF, QPERF_OUT, FOLDED], check=True)

with open(FOLDED) as f:
    lines = sum(1 for _ in f)
print(f"Folded stacks: {lines} lines")
print(f"=== Done: {VARIANT} ===")
