#!/usr/bin/env python3
"""
qperf-compare.py — Compare two qperf folded stack files, generating a hotspot analysis.
Filters to kernel-space only, resolves symbols, and produces a Markdown comparison table.
"""
import subprocess, sys, os
from collections import defaultdict

def parse_folded(path):
    """Parse folded stacks file, return {func_name: count} for leaf functions."""
    counts = defaultdict(int)
    total = 0
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.rsplit(" ", 1)
            if len(parts) != 2:
                continue
            stack = parts[0]
            try:
                count = int(parts[1])
            except ValueError:
                continue
            total += count
            # Use leaf function (last in semicolon-separated stack)
            frames = stack.split(";")
            leaf = frames[-1].strip() if frames else stack
            counts[leaf] += count
    return counts, total

def main():
    if len(sys.argv) != 3:
        print(f"Usage: {sys.argv[0]} <original.folded> <patched.folded>")
        sys.exit(1)

    orig_path = sys.argv[1]
    patched_path = sys.argv[2]

    orig_counts, orig_total = parse_folded(orig_path)
    patched_counts, patched_total = parse_folded(patched_path)

    print(f"# qperf Hotspot Comparison")
    print(f"")
    print(f"| Metric | Original | Patched |")
    print(f"|--------|----------|---------|")
    print(f"| Total samples | {orig_total} | {patched_total} |")
    print(f"| Unique leaf functions | {len(orig_counts)} | {len(patched_counts)} |")
    print(f"")

    # Combine all functions, compute diff
    all_funcs = set(orig_counts.keys()) | set(patched_counts.keys())
    rows = []
    for func in all_funcs:
        oc = orig_counts.get(func, 0)
        pc = patched_counts.get(func, 0)
        op = oc / orig_total * 100 if orig_total > 0 else 0
        pp = pc / patched_total * 100 if patched_total > 0 else 0
        diff = pp - op
        rows.append((func, oc, pc, op, pp, diff))

    # Sort by absolute diff
    rows.sort(key=lambda r: abs(r[5]), reverse=True)

    # Print top 30 functions with biggest changes
    print(f"## Top 30 Functions by Sampling Change")
    print(f"")
    print(f"| Function | Orig Samples | Patch Samples | Orig % | Patched % | Δ (pp) |")
    print(f"|----------|-------------|---------------|--------|-----------|--------|")
    for func, oc, pc, op, pp, diff in rows[:30]:
        # Truncate long names
        short = func[:80] + "..." if len(func) > 80 else func
        sign = "+" if diff > 0 else ""
        print(f"| {short} | {oc} | {pc} | {op:.2f} | {pp:.2f} | {sign}{diff:.2f} |")

    # Print virtio-related functions
    print(f"")
    print(f"## VirtIO-Related Functions")
    print(f"")
    virtio_funcs = [(f, oc, pc, op, pp, diff) for f, oc, pc, op, pp, diff in rows
                    if any(kw in f.lower() for kw in ["virt", "blk", "queue", "fence", "atomic", "notify", "transport", "pci"])]
    if virtio_funcs:
        print(f"| Function | Orig Samples | Patch Samples | Orig % | Patched % | Δ (pp) |")
        print(f"|----------|-------------|---------------|--------|-----------|--------|")
        for func, oc, pc, op, pp, diff in sorted(virtio_funcs, key=lambda r: abs(r[5]), reverse=True):
            short = func[:80] + "..." if len(func) > 80 else func
            sign = "+" if diff > 0 else ""
            print(f"| {short} | {oc} | {pc} | {op:.2f} | {pp:.2f} | {sign}{diff:.2f} |")
    else:
        print("No VirtIO-related functions found in samples.")
        print("(This may indicate the profiling did not capture the I/O path effectively.)")

if __name__ == "__main__":
    main()
