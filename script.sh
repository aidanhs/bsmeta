#!/bin/bash
set -o errexit
set -o pipefail
set -o nounset
set -o xtrace

if [ "$1" = sync ]; then
    shift
    cargo build --release
    rsync -avrz --progress --partial \
        ./target/release/bsmeta ./.env ./bsmeta-empty.db ../songsdata.json ./progress.py \
        "$1":~/work/bsmeta/

elif [ "$1" = build ]; then
    WASI_ROOT=$(pwd)/wasmtime/crates/wasi-common/WASI cargo build --release

elif [ "$1" = doc ]; then
    WASI_ROOT=$(pwd)/wasmtime/crates/wasi-common/WASI cargo doc --release --open

elif [ "$1" = run ]; then
    shift
    RUST_BACKTRACE=1 WASI_ROOT=$(pwd)/wasmtime/crates/wasi-common/WASI cargo run --release -- "$@"
    #RUST_BACKTRACE=1 WASI_ROOT=$(pwd)/wasmtime/crates/wasi-common/WASI cargo flamegraph -- "$@"

elif [ "$1" = plugins ]; then
    shift

    arg=
    if [ "$#" -ge 1 ]; then
        arg="$1"
        shift
    fi
    if [ "$arg" = build-interps ]; then
        cd plugins
        mkdir -p dist

        # JS
        cd quickjs
        git reset --hard
        git clean -fxd
        git apply < ../quickjs.patch
        # https://github.com/WebAssembly/wasi-libc/issues/85 - WASM has limited rounding modes,
        # we just define them arbitrarily and accept that rounding will be wrong ¯\_(ツ)_/¯
        make libquickjs.a \
            CONFIG_DEFAULT_AR=y CONFIG_CLANG=y \
            CROSS_PREFIX=wasi CC="wasicc -DEMSCRIPTEN -DFE_DOWNWARD=100 -DFE_UPWARD=101"
        cd ..
        wasicc -Wl,--allow-undefined -Wall -O2 -o dist/js.wasm interp-js.c quickjs/libquickjs.a

    elif [ "$arg" = build-plugins ]; then
        cd plugins
        mkdir -p dist

        ./genplugins.py

    elif [ "$arg" = rebuild ]; then
        rm -rf plugins/dist
        ./script.sh plugins build-interps
        ./script.sh plugins build-plugins

    else
        echo "unknown plugins subcommand"
        exit 1
    fi

else
    echo invalid command
    exit 1
fi
