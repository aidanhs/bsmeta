#!/usr/bin/env python3

import json
import sqlite3
import sys

assert sys.version_info.major == 3

SONGS_PER_MINUTE = 1

def go():
    conn = sqlite3.connect("bsmeta.db")
    cur = conn.cursor()
    cur.execute("select count(*), sum(length(data))/1024 from tsong where tsong.data is not null")
    fetched_songs, size_kb = cur.fetchone()
    cur.execute("select count(*) from tsong where tsong.deleted = 0")
    total_songs, = cur.fetchone()

    cur.execute('select extra_meta from tsong where extra_meta is not null')
    extra_metas = cur.fetchall()
    dl_kb = 0
    for extra_meta, in extra_metas:
        extra_meta = json.loads(extra_meta)
        dl_kb += extra_meta['zip_size']/1024

    def extrapolate(val):
        return val * (total_songs / fetched_songs)

    pct_progress = fetched_songs * 100 / total_songs
    predicted_size_gb = extrapolate(size_kb) / 1024 / 1024
    predicted_dl_gb = extrapolate(dl_kb) / 1024 / 1024
    eta_hours = (total_songs-fetched_songs) / SONGS_PER_MINUTE / 60
    print(f'Currently got {fetched_songs} / {total_songs} songs')
    print(f'Predicting {predicted_size_gb:.02f}GB database')
    print(f'At {pct_progress:.02f}% ({dl_kb/1024/1024:.02f}GB downloaded of ~{predicted_dl_gb:.02f}GB)')
    print(f'At {SONGS_PER_MINUTE} songs per min, predicting {eta_hours:.02f} hours ({eta_hours/24:.02f} days) to completion')

go()
