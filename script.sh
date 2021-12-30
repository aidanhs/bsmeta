#!/bin/bash
set -o errexit
set -o pipefail
set -o nounset
set -o xtrace

OPT=
OPT=--release

export RUST_LOG=bsmeta=trace

if [ "$1" = sync ]; then
    shift
    rsync -avrz --progress --partial \
        ./target/release/bsmeta ./.env ./static \
        "$1":~/work/bsmeta/
    rsync -avrz --progress --partial --relative \
        ./plugins/dist/ \
        "$1":~/work/bsmeta/

elif [ "$1" = build ]; then
    WASI_ROOT=$(pwd)/wasmtime/crates/wasi-common/WASI cargo build $OPT

elif [ "$1" = check ]; then
    WASI_ROOT=$(pwd)/wasmtime/crates/wasi-common/WASI cargo check $OPT

elif [ "$1" = doc ]; then
    WASI_ROOT=$(pwd)/wasmtime/crates/wasi-common/WASI cargo doc $OPT --open

elif [ "$1" = cargo ]; then
    shift
    WASI_ROOT=$(pwd)/wasmtime/crates/wasi-common/WASI cargo "$@"

elif [ "$1" = run ]; then
    shift
    RUST_BACKTRACE=1 WASI_ROOT=$(pwd)/wasmtime/crates/wasi-common/WASI cargo run $OPT -- "$@"
    #RUST_BACKTRACE=1 WASI_ROOT=$(pwd)/wasmtime/crates/wasi-common/WASI cargo flamegraph -- "$@"

elif [ "$1" = mkdb ]; then
    . .env
    db="$(echo "$DATABASE_URL" | sed 's/^sqlite://g')"
    rm -f "$db"
    sqlite3 "$db" <schema.sql

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
        rm -f dist/*.wasm

        # TODO: -g
        # TODO wasm-opt: -O0 -g

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

        # Python - with help from https://github.com/aidanhs/empython
        cd cpython
        git reset --hard
        git clean -fxd
        ## First we native build
        #./configure
        #make python
        #cp python ../python.native
        ## Now we wasm build
        #git clean -fxd
        (cd Modules/zlib && wasiconfigure ./configure --static && wasimake make libz.a)
        cat ../cpython-preconfigure.patch | git apply
        CFLAGS="-D_WASI_EMULATED_SIGNAL" wasiconfigure ./configure --without-threads --without-pymalloc --disable-ipv6 --prefix=$(pwd)/dist
        cat ../cpython.patch | git apply
        wasimake make libpython3.5.a
        # TODO: do something more advanced, to only put in optimized compiled libs (i.e. .pyo)
        (cd Lib && zip -x 'test/*' -x 'ensurepip/*' -x 'idlelib/*' -x 'distutils/*' -r ../lib.zip *)
        # TODO: embed it into the wasm somehow
        #xxd -i lib.zip lib.zip.h
        cp lib.zip ../dist/
        cd ..
        wasmcc -c interp-py-redefs.c -o /tmp/interp-py-redefs.o
        # https://github.com/WebAssembly/wasi-libc/issues/233 - size stack-size up!
        wasicc -Wl,--allow-undefined -Wall -O2 -Icpython -o /tmp/py.wasm /tmp/interp-py-redefs.o interp-py.c cpython/libpython3.5.a cpython/Modules/zlib/libz.a -lwasi-emulated-signal -Wl,-z,stack-size=$((8*1024*1024)) -Wl,--initial-memory=$((32*1024*1024))
        wasm-opt --fpcast-emu -O0 /tmp/py.wasm -o dist/py.wasm

        cd ..

    elif [ "$arg" = build-plugins ]; then
        cd plugins
        mkdir -p dist
        rm -f dist/*.tar

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
