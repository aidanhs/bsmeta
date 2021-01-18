#!/usr/bin/env python3
import json
import io
import tarfile

plugins = json.load(open('pluginlist.json'))
pluginlist = {}
for name, info in plugins.items():
    pluginlist[name] = info['interp']
    tf = tarfile.open('dist/' + name + '.tar', 'w')
    for mapfrom, mapto in info['files'].items():
        data = open(mapfrom, 'rb').read()
        ti = tarfile.TarInfo(mapto)
        ti.size = len(data)
        tf.addfile(ti, io.BytesIO(data))
    tf.close()
open('dist/pluginlist.json', 'wb').write(json.dumps(pluginlist).encode('utf8'))
