#!/usr/bin/env bash
# Run real vendored boundary probes without a TDLib client, receive pump or Cargo.
set -euo pipefail
repo_root=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$repo_root"

shopt -s nullglob
futures=(target/debug/deps/libfutures_channel-*.rlib)
json=(target/debug/deps/libserde_json-*.rlib)
logs=(target/debug/deps/liblog-*.rlib)
tdlib=(target/debug/deps/libtdlib_rs-*.rlib)
if (( ${#futures[@]} == 0 || ${#json[@]} == 0 || ${#logs[@]} == 0 || ${#tdlib[@]} == 0 )); then
    printf '%s\n' 'Missing cached native boundary dependencies; run cargo test --locked --no-run first (in an environment with dependencies already available).' >&2
    exit 1
fi

# A Rust type from tdlib_rs must use the same serde/serde_core graph as
# serde_json::from_value. Cargo fingerprint dependencies record the exact
# serde_json build used by each cached tdlib_rs rlib; glob order does not.
matching_json_artifact() {
    local metadata=$1 expected candidate suffix fingerprint value reversed index
    [[ $metadata =~ \"serde_json\",false,([0-9]+) ]] || return 1
    printf -v expected '%016x' "${BASH_REMATCH[1]}"
    for candidate in "${json[@]}"; do
        suffix=${candidate##*-}
        suffix=${suffix%.rlib}
        fingerprint="target/debug/.fingerprint/serde_json-${suffix}/lib-serde_json"
        [[ -f $fingerprint ]] || continue
        IFS= read -r value < "$fingerprint" || [[ -n $value ]] || continue
        [[ $value =~ ^[[:xdigit:]]{16}$ ]] || continue
        reversed=
        for (( index=14; index>=0; index-=2 )); do
            reversed+=${value:index:2}
        done
        if [[ $reversed == "$expected" ]]; then
            chosen_json=$candidate
            return 0
        fi
    done
    return 1
}

chosen_tdlib=
chosen_json=
for candidate in "${tdlib[@]}"; do
    suffix=${candidate##*-}
    suffix=${suffix%.rlib}
    fingerprint="target/debug/.fingerprint/tdlib-rs-${suffix}/lib-tdlib_rs.json"
    [[ -f $fingerprint ]] || continue
    IFS= read -r metadata < "$fingerprint" || [[ -n $metadata ]] || continue
    if matching_json_artifact "$metadata"; then
        chosen_tdlib=$candidate
        break
    fi
done
if [[ -z $chosen_tdlib ]]; then
    printf '%s\n' 'No cached tdlib_rs rlib has a matching serde_json dependency artifact; run cargo test --locked --no-run first.' >&2
    exit 1
fi

tdlib_lib="${LOCAL_TDLIB_PATH:-third_party/tdlib}/lib"

mkdir -p target/security
rustc --test --edition=2024 scripts/security/observer_probe.rs \
    --extern "futures_channel=${futures[0]}" \
    --extern "serde_json=$chosen_json" \
    --extern "log=${logs[0]}" \
    --extern "tdlib_rs=$chosen_tdlib" \
    -L dependency=target/debug/deps \
    -L "native=$tdlib_lib" \
    -o target/security/observer_probe_bin
LD_LIBRARY_PATH="$tdlib_lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}" \
    target/security/observer_probe_bin security_probe_ --ignored --nocapture --test-threads=1
