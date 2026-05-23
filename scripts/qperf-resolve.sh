#!/usr/bin/env python3
"""
resolve-folded.py — Re-resolve folded stack addresses using addr2line CLI.
Handles symbol resolution for ELF files without DWARF (using symtab only).
"""
import subprocess, sys, os, re
from collections import defaultdict

def resolve_addrs(elf_path, addrs):
    """Resolve a batch of addresses using addr2line."""
    if not addrs:
        return {}
    addr_list = list(addrs)
    cmd = ["riscv64-linux-musl-addr2line", "-e", elf_path, "-f", "-C"]
    proc = subprocess.run(cmd, input="\n".join(addr_list), capture_output=True, text=True)
    lines = proc.stdout.strip().split("\n")
    result = {}
    for i, addr in enumerate(addr_list):
        if i * 2 + 1 < len(lines):
            func = lines[i * 2].strip()
            loc = lines[i * 2 + 1].strip()
            if func == "??" or func.startswith("0x"):
                result[addr] = addr  # keep raw address
            else:
                result[addr] = func
        else:
            result[addr] = addr
    return result

def main():
    if len(sys.argv) != 4:
        print(f"Usage: {sys.argv[0]} <elf> <input.folded> <output.folded>")
        sys.exit(1)
    elf_path = sys.argv[1]
    input_path = sys.argv[2]
    output_path = sys.argv[3]

    # First pass: collect all unique addresses
    all_addrs = set()
    stacks = []
    with open(input_path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            parts = line.rsplit(" ", 1)
            if len(parts) != 2:
                continue
            stack_str = parts[0]
            count = parts[1]
            addrs = [a.strip() for a in stack_str.split(";") if a.strip()]
            stacks.append((addrs, count))
            all_addrs.update(addrs)

    print(f"Unique addresses: {len(all_addrs)}")

    # Resolve in batches
    addr_map = {}
    batch_size = 500
    addr_list = sorted(all_addrs)
    for i in range(0, len(addr_list), batch_size):
        batch = addr_list[i:i+batch_size]
        resolved = resolve_addrs(elf_path, batch)
        addr_map.update(resolved)
        if (i // batch_size) % 20 == 0:
            print(f"  Resolved {min(i+batch_size, len(addr_list))}/{len(addr_list)}...")

    # Second pass: write resolved folded stacks
    with open(output_path, "w") as out:
        for addrs, count in stacks:
            resolved = []
            for a in addrs:
                resolved.append(addr_map.get(a, a))
            out.write(";".join(resolved) + " " + count + "\n")

    print(f"Output written to {output_path}")

if __name__ == "__main__":
    main()
