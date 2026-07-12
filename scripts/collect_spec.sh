#!/usr/bin/env bash
# collect_spec.sh: dump a machine's hardware + software spec as Markdown.
#
# Prints to stdout; redirect into a spec file of your choice.
# Portable across bare metal and VMs; every probe degrades to "n/a" when the
# tool or file is absent, so it never aborts. Run under a login shell so the
# toolchain versions (rustc/gcc/go) resolve:
#
#   ssh <host> "bash -lc 'bash path/to/ticklog/scripts/collect_spec.sh <label>'" \
#     > <host>_spec.md
#
# Some rows (dmidecode memory speed) need root; they print "n/a (need root)"
# otherwise.

label="${1:-$(hostname)}"

# Print "$*" if it produces output, else a placeholder. Used as: cap <cmd...>
cap() {
    local out
    out="$("$@" 2>/dev/null)" || true
    if [[ -n "$out" ]]; then printf '%s\n' "$out"; else echo "n/a"; fi
}

# Print the contents of a sysfs/proc file, or "n/a".
catf() {
    if [[ -r "$1" ]]; then cat "$1" 2>/dev/null; else echo "n/a"; fi
}

# Markdown section with a fenced block from a command.
block() {
    local title="$1"; shift
    echo "### $title"
    echo '```text'
    cap "$@"
    echo '```'
    echo
}

echo "# ${label} hardware & software spec"
echo
echo "Collected by \`scripts/collect_spec.sh\` on \`$(date -u '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null || echo n/a)\` (UTC)."
echo "Hostname: \`$(hostname 2>/dev/null || echo n/a)\`  Kernel: \`$(uname -r 2>/dev/null || echo n/a)\`"
echo

echo "## Summary"
echo
model="$(grep -m1 'model name' /proc/cpuinfo 2>/dev/null | cut -d: -f2- | sed 's/^ //')"
sockets="$(lscpu 2>/dev/null | awk -F: '/^Socket/{print $2}' | tr -d ' ')"
cps="$(lscpu 2>/dev/null | awk -F: '/^Core\(s\) per socket/{print $2}' | tr -d ' ')"
tpc="$(lscpu 2>/dev/null | awk -F: '/^Thread\(s\) per core/{print $2}' | tr -d ' ')"
cpus="$(nproc --all 2>/dev/null)"
memg="$(free -h 2>/dev/null | awk '/^Mem:/{print $2}')"
virt="$(systemd-detect-virt 2>/dev/null || echo n/a)"
echo "| Field | Value |"
echo "| ----- | ----- |"
echo "| CPU model | ${model:-n/a} |"
echo "| Sockets x cores x threads | ${sockets:-?} x ${cps:-?} x ${tpc:-?} |"
echo "| Logical CPUs (nproc --all) | ${cpus:-n/a} |"
echo "| Memory total | ${memg:-n/a} |"
echo "| Virtualization | ${virt:-none} |"
echo "| OS | $(. /etc/os-release 2>/dev/null && echo "$PRETTY_NAME" || echo n/a) |"
echo

echo "## CPU"
echo
block "lscpu" lscpu
echo "### Topology (lscpu -e: core/socket/node + online)"
echo '```text'
cap lscpu -e
echo '```'
echo
echo "### TSC / timing flags (from /proc/cpuinfo)"
echo '```text'
grep -m1 -oE '(constant_tsc|nonstop_tsc|tsc_known_freq|rdtscp|tsc_adjust|tsc_deadline_timer)( |$)' /proc/cpuinfo 2>/dev/null | tr '\n' ' ' || echo n/a
echo
echo "tsc_freq_khz (if exposed): $(catf /sys/devices/system/cpu/cpu0/tsc_freq_khz)"
echo '```'
echo
echo "### SMT"
echo '```text'
echo "smt active: $(catf /sys/devices/system/cpu/smt/active)   control: $(catf /sys/devices/system/cpu/smt/control)"
echo '```'
echo

echo "## Frequency & power"
echo
echo '```text'
echo "cpufreq driver:   $(catf /sys/devices/system/cpu/cpu0/cpufreq/scaling_driver)"
echo "governor (cpu0):  $(catf /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor)"
echo "scaling min/max:  $(catf /sys/devices/system/cpu/cpu0/cpufreq/scaling_min_freq) / $(catf /sys/devices/system/cpu/cpu0/cpufreq/scaling_max_freq) kHz"
echo "cpuinfo min/max:  $(catf /sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_min_freq) / $(catf /sys/devices/system/cpu/cpu0/cpufreq/cpuinfo_max_freq) kHz"
echo "intel_pstate no_turbo: $(catf /sys/devices/system/cpu/intel_pstate/no_turbo)   (0 = turbo on)"
echo "cpufreq boost:         $(catf /sys/devices/system/cpu/cpufreq/boost)"
echo "governors available:   $(catf /sys/devices/system/cpu/cpu0/cpufreq/scaling_available_governors)"
echo '```'
echo "> Note: run \`turbostat --quiet sleep 1\` under load to see achieved MHz; static files above show config, not achieved."
echo

echo "## Memory"
echo
block "free -h" free -h
echo "### DIMM type / speed (dmidecode, needs root)"
echo '```text'
if [[ "$(id -u)" == "0" ]] && command -v dmidecode >/dev/null 2>&1; then
    dmidecode -t memory 2>/dev/null | grep -E '^\s*(Size|Type|Speed|Configured Memory Speed|Rank):' | sort | uniq -c || echo n/a
else
    echo "n/a (need root + dmidecode)"
fi
echo '```'

echo "## NUMA"
echo
block "numactl -H" numactl -H

echo "## Cache"
echo
echo '```text'
for idx in /sys/devices/system/cpu/cpu0/cache/index*; do
    [[ -d "$idx" ]] || continue
    echo "L$(catf $idx/level) $(catf $idx/type): size=$(catf $idx/size) line=$(catf $idx/coherency_line_size)B shared=$(catf $idx/shared_cpu_list)"
done 2>/dev/null || echo n/a
echo '```'
echo

echo "## Kernel, isolation & IRQ"
echo
echo '```text'
echo "cmdline: $(catf /proc/cmdline)"
echo
echo "isolated cores: $(catf /sys/devices/system/cpu/isolated)"
echo "nohz_full:      $(catf /sys/devices/system/cpu/nohz_full)"
echo "THP:            $(catf /sys/kernel/mm/transparent_hugepage/enabled)"
echo "perf_event_paranoid: $(catf /proc/sys/kernel/perf_event_paranoid)"
echo "nmi_watchdog:        $(catf /proc/sys/kernel/nmi_watchdog)"
echo "irqbalance:     $(systemctl is-active irqbalance 2>/dev/null || echo n/a)"
echo "tuned profile:  $(tuned-adm active 2>/dev/null | sed 's/Current active profile: //' || echo n/a)"
echo '```'
echo

echo "## CPU vulnerabilities / mitigations"
echo
echo '```text'
for f in /sys/devices/system/cpu/vulnerabilities/*; do
    [[ -r "$f" ]] && printf '%-24s %s\n' "$(basename "$f")" "$(cat "$f")"
done 2>/dev/null || echo n/a
echo '```'
echo

echo "## Storage"
echo
block "lsblk" lsblk -o NAME,SIZE,TYPE,MODEL,MOUNTPOINT
if command -v nvme >/dev/null 2>&1; then block "nvme list" nvme list; fi

echo "## Toolchains"
echo
echo '```text'
echo "rustc:   $(rustc --version 2>/dev/null || echo n/a)"
echo "cargo:   $(cargo --version 2>/dev/null || echo n/a)"
echo "rustup:  $(rustup show active-toolchain 2>/dev/null || echo n/a)"
echo "gcc:     $(gcc --version 2>/dev/null | head -1 || echo n/a)"
echo "g++:     $(g++ --version 2>/dev/null | head -1 || echo n/a)"
echo "clang:   $(clang --version 2>/dev/null | head -1 || echo n/a)"
echo "cmake:   $(cmake --version 2>/dev/null | head -1 || echo n/a)"
echo "make:    $(make --version 2>/dev/null | head -1 || echo n/a)"
echo "go:      $(go version 2>/dev/null || echo n/a)"
echo "python3: $(python3 --version 2>/dev/null || echo n/a)"
echo "perf:    $(perf --version 2>/dev/null || echo n/a)"
echo '```'
