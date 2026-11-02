#!/usr/bin/env python3
"""Pack only required products from build.sh's staging directory, losslessly."""
import hashlib
import json
from pathlib import Path
import stat
import sys
import zipfile

stage=Path(sys.argv[1]).resolve(strict=True)
output=stage/'packed'
output.mkdir()
for platform,folder,names in [
    ('ios','ios-derived/Build/Products/Debug-iphonesimulator',['sample-app.app','sample-appUITests-Runner.app']),
    ('macos','macos-derived/Build/Products/Debug',['AppleWorld.app','AppleWorldUITests-Runner.app'])
]:
    base=stage/folder
    manifest=[]
    with zipfile.ZipFile(output/(platform+'.zip'),'w',zipfile.ZIP_DEFLATED,compresslevel=9) as z:
        for name in names:
            for p in sorted((base/name).rglob('*')):
                rel=p.relative_to(base).as_posix()
                mode=p.lstat().st_mode
                if p.is_symlink():
                    data=p.readlink().as_posix().encode();kind='symlink'
                elif p.is_dir():
                    continue
                else:
                    data=p.read_bytes();kind='file'
                info=zipfile.ZipInfo(rel,(2000,1,1,0,0,0))
                info.create_system=3
                info.external_attr=mode<<16
                info.compress_type=zipfile.ZIP_DEFLATED
                z.writestr(info,data)
                manifest.append(dict(path=rel,kind=kind,mode=oct(stat.S_IMODE(mode)),size=len(data),sha256=hashlib.sha256(data).hexdigest()))
    (output/(platform+'-tree.json')).write_text(json.dumps(manifest,indent=2)+'\n')
print(output)
