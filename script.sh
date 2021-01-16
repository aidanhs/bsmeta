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

elif [ "$1" = buildplugins ]; then
    cd quickjs

    ## Native
    #make clean
    #make -j8

    ## Emscripten
    #EMFLAGS="-s STANDALONE_WASM=1 -s ERROR_ON_UNDEFINED_SYMBOLS=0 -s EXPORTED_FUNCTIONS=[\"_do_analysis\"]"
    #make clean
    #make -j8 CONFIG_DEFAULT_AR=y CROSS_PREFIX=em CC="emcc $EMFLAGS" CONFIG_CLANG=y

    ## wasienv
    #make clean
    ## https://github.com/WebAssembly/wasi-libc/issues/85 - WASM has limited rounding modes, we just define them arbitrarily
    ## and accept that rounding will be wrong ¯\_(ツ)_/¯
    #make -j8 libquickjs.a \
    #    CONFIG_DEFAULT_AR=y CONFIG_CLANG=y \
    #    CROSS_PREFIX=wasi CC="wasicc -DEMSCRIPTEN -DFE_DOWNWARD=100 -DFE_UPWARD=101"

    cd ..

    #gcc -O2 -Wall -lm myeval.c quickjs/libquickjs.a
    #emcc --no-entry $EMFLAGS -O2 -o out.wasm myeval.c quickjs/libquickjs.a
    wasicc -Wl,--allow-undefined -Wl,--export=do_analysis -O2 -o out.wasm myeval.c quickjs/libquickjs.a

elif [ "$1" = run ]; then
    shift

else
    echo invalid command
    exit 1
fi
