#!/bin/bash
# qperf-bench.sh — Run QEMU with qperf plugin, inject bench-virtio-blk workload
# Uses release kernel for fast boot, release ELF for symbol resolution
set -euo pipefail

VARIANT="${1:?Usage: $0 <original|patched> <output_dir>}"
OUTDIR="${2:?Usage: $0 <original|patched> <output_dir>}"
WORKDIR="/work"
KERNEL_BIN="$WORKDIR/target/qperf/kernels/starryos-${VARIANT}-release.bin"
ELF="$WORKDIR/target/qperf/kernels/starryos-${VARIANT}-release"
ROOTFS="$WORKDIR/target/rootfs/rootfs-riscv64-alpine.img"
PLUGIN="$WORKDIR/tools/qperf/target/release/libqperf.so"
QPERF_OUT="$WORKDIR/${OUTDIR}/qperf.bin"
INPUT_FIFO="/tmp/qemu-input-${VARIANT}"

mkdir -p "$WORKDIR/${OUTDIR}"
rm -f "$INPUT_FIFO"
mkfifo "$INPUT_FIFO"

echo "=== qperf-bench: $VARIANT ==="
echo "kernel: $KERNEL_BIN"
echo "elf:    $ELF"
echo "out:    $QPERF_OUT"

# Start QEMU with stdin from FIFO
qemu-system-riscv64 \
  -machine virt -m 512M -nographic -cpu rv64 \
  -device virtio-blk-pci,drive=disk0 \
  -drive "id=disk0,if=none,format=raw,file=${ROOTFS}" \
  -device virtio-net-pci,netdev=net0 \
  -netdev user,id=net0 \
  -kernel "$KERNEL_BIN" \
  -plugin "${PLUGIN},freq=999,max_depth=64,queue_size=4096,out=${QPERF_OUT}" \
  < "$INPUT_FIFO" &> "$WORKDIR/${OUTDIR}/qemu.log" &

QEMU_PID=$!
echo "QEMU PID: $QEMU_PID"

# Keep FIFO open for writing
exec 3>"$INPUT_FIFO"

# Wait for boot and inject benchmark
BENCH_SENT=0
TIMEOUT_SEC=180
ELAPSED=0
while [ $ELAPSED -lt $TIMEOUT_SEC ]; do
  if [ $BENCH_SENT -eq 0 ] && grep -q "root@starry:" "$WORKDIR/${OUTDIR}/qemu.log" 2>/dev/null; then
    sleep 2
    echo "/usr/bin/bench-virtio-blk" >&3
    BENCH_SENT=1
    echo ">>> BENCHMARK INJECTED at ${ELAPSED}s <<<"
  fi

  if [ $BENCH_SENT -eq 1 ] && grep -q "BENCH_PASS" "$WORKDIR/${OUTDIR}/qemu.log" 2>/dev/null; then
    echo ">>> BENCH PASS at ${ELAPSED}s <<<"
    sleep 5  # extra samples after benchmark
    break
  fi

  if grep -q "BENCH_FAIL" "$WORKDIR/${OUTDIR}/qemu.log" 2>/dev/null; then
    echo ">>> BENCH FAIL at ${ELAPSED}s <<<"
    break
  fi

  if ! kill -0 $QEMU_PID 2>/dev/null; then
    echo "QEMU died at ${ELAPSED}s"
    break
  fi
  sleep 2
  ELAPSED=$((ELAPSED + 2))
done

echo "Terminating QEMU..."
exec 3>&-
kill -TERM $QEMU_PID 2>/dev/null || true

WAIT=0
while [ $WAIT -lt 30 ] && kill -0 $QEMU_PID 2>/dev/null; do
  sleep 1
  WAIT=$((WAIT + 1))
done
if kill -0 $QEMU_PID 2>/dev/null; then
  kill -KILL $QEMU_PID 2>/dev/null || true
fi
wait $QEMU_PID 2>/dev/null || true
echo "QEMU exited"

rm -f "$INPUT_FIFO"

# Check summary
SUMMARY="${QPERF_OUT}.summary.txt"
if [ -f "$SUMMARY" ]; then
  echo "Plugin summary:"
  cat "$SUMMARY"
fi

if [ ! -s "$QPERF_OUT" ]; then
  echo "ERROR: qperf.bin is empty!"
  tail -20 "$WORKDIR/${OUTDIR}/qemu.log"
  exit 1
fi

SIZE=$(stat -c%s "$QPERF_OUT")
echo "qperf.bin: $SIZE bytes"

# Run analyzer
FOLDED="$WORKDIR/${OUTDIR}/stack.folded"
ANALYZER="$WORKDIR/tools/qperf/target/release/qperf-analyzer"
echo "Running qperf-analyzer..."
"$ANALYZER" -e "$ELF" "$QPERF_OUT" "$FOLDED"

LINES=$(wc -l < "$FOLDED")
echo "Folded stacks: $LINES lines"
echo "=== Done: $VARIANT ==="
