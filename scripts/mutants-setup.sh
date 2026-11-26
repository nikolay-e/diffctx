#!/usr/bin/env bash
# The runner side of .github/workflows/mutants.yml: install the pinned cargo-mutants, then cap the memory
# of the cgroup the mutated tests run in below the machine's.
set -euo pipefail

base=https://github.com/sourcefrog/cargo-mutants/releases/download
curl -fsSL -o cm.tgz "$base/v${CARGO_MUTANTS_VERSION}/cargo-mutants-x86_64-unknown-linux-gnu.tar.gz"
echo "${CARGO_MUTANTS_SHA256}  cm.tgz" | sha256sum -c -
tar xzf cm.tgz -C "$HOME/.cargo/bin" cargo-mutants
cargo mutants --version

# A mutant that grows memory without bound took the whole VM with it: the runner got a shutdown signal and
# the job's results were lost (occurrence 6/8 and 11/15). In a cgroup capped under the machine's memory the
# kernel kills the mutated test instead, which cargo-mutants counts as caught.
total_kb=$(awk '/MemTotal/ {print $2}' /proc/meminfo)
cap=$(((total_kb - 2500000) * 1024))
sudo mkdir -p /sys/fs/cgroup/mutants
echo "$cap" | sudo tee /sys/fs/cgroup/mutants/memory.max
echo 0 | sudo tee /sys/fs/cgroup/mutants/memory.swap.max
cat /sys/fs/cgroup/mutants/memory.max
