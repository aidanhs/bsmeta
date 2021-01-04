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

elif [ "$1" = run ]; then
    shift

else
    echo invalid command
    exit 1
fi
