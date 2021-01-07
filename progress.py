#!/usr/bin/env python3

import json
import sqlite3
import sys

assert sys.version_info.major == 3

SONGS_PER_MINUTE = 6

def go():
    conn = sqlite3.connect("bsmeta.db")
    cur = conn.cursor()

    print('fetching counts')
    cur.execute("select count(*), sum(length(data))/1024, sum(length(zipdata))/1024 from tsong s, tsongdata sd where s.key = sd.key")
    fetched_songs, meta_size_kb, total_size_kb = cur.fetchone()
    cur.execute("select count(*) from tsong where tsong.deleted = 0")
    total_songs, = cur.fetchone()
    print('fetched counts')

    #print('fetching metas')
    #cur.execute('select extra_meta from tsong where extra_meta is not null')
    #extra_metas = cur.fetchall()
    #print('fetched all metas, processing')
    #dl_kb = 0
    #for extra_meta, in extra_metas:
    #    extra_meta = json.loads(extra_meta)
    #    dl_kb += extra_meta['zip_size']/1024

    def extrapolate(val):
        return val * (total_songs / fetched_songs)

    pct_progress = fetched_songs * 100 / total_songs
    predicted_meta_size_gb = extrapolate(meta_size_kb) / 1024 / 1024
    predicted_total_size_gb = extrapolate(total_size_kb) / 1024 / 1024
    #predicted_dl_gb = extrapolate(dl_kb) / 1024 / 1024
    eta_hours = (total_songs-fetched_songs) / SONGS_PER_MINUTE / 60
    print(f'Currently got {fetched_songs} / {total_songs} songs - {pct_progress:.02f}%')
    print(f'Got {meta_size_kb/1024/1024:.02f}GB meta and {total_size_kb/1024/1024:.02f}GB zips')
    print(f'Predicting {predicted_meta_size_gb:.02f}GB meta and {predicted_total_size_gb:.02f}GB zips')
    print(f'At {SONGS_PER_MINUTE} songs per min, predicting {eta_hours:.02f} hours ({eta_hours/24:.02f} days) to completion')

go()
