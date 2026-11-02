"""Real rsync uploads seed hostile links before each filesystem regression."""
import os
import shlex
import sys

COMMANDS = {
    'copy-file-link': ('cp -R /tmp/marathon/source /tmp/marathon/file-dest', 1),
    'copy-directory-link': ('cp -R /tmp/marathon/source /tmp/marathon/dir-dest', 1),
    'copy-basename-link': ('cp -R /tmp/marathon/source /tmp/marathon/base-dest', 1),
    'move-basename-link': ('mv -f /tmp/marathon/move-file /tmp/marathon/move-dest', 1),
    'move-through-link': ('mv -f /tmp/marathon/move-file /tmp/marathon/dir-link/new', 1),
    'remove-through-link': ('rm -rf /tmp/marathon/dir-link/sentinel', 1),
    'link-through-link': ('ln -s /tmp/marathon/move-file /tmp/marathon/dir-link/new', 1),
    'find-through-link': ('find /tmp/marathon/dir-link -mindepth 1 -maxdepth 1 -exec rm -rf {} +', 2),
    'writefile-temporary-link': ("sh -c 'umask 077 && echo WA== | base64 --decode > /tmp/marathon/file.marathon-tmp && [ $(wc -c < /tmp/marathon/file.marathon-tmp) -eq 1 ] && mv -f /tmp/marathon/file.marathon-tmp /tmp/marathon/file || { rm -f /tmp/marathon/file.marathon-tmp; exit 1; }'", 1),
    'writefile-replaces-link': ("sh -c 'umask 077 && echo WA== | base64 --decode > /tmp/marathon/write-link.marathon-tmp && [ $(wc -c < /tmp/marathon/write-link.marathon-tmp) -eq 1 ] && mv -f /tmp/marathon/write-link.marathon-tmp /tmp/marathon/write-link || { rm -f /tmp/marathon/write-link.marathon-tmp; exit 1; }'", 0),
    'remove-trailing-link': ('rm -rf /tmp/marathon/remove-link/', 0),
    'move-trailing-link': ('mv -f /tmp/marathon/move-link/ /tmp/marathon/moved-link', 0),
    'remove-tree-links': ('rm -rf /tmp/marathon/remove-tree', 0),
    'find-removes-links': ('find /tmp/marathon/find-tree -mindepth 1 -maxdepth 1 -exec rm -rf {} +', 0),
}
RUN_CASES = ['containment-upload', *COMMANDS, 'rsync-replaces-descendant-links', 'rsync-link-operand']
CHECK_CASES = ['containment-uploaded-links', 'containment-sentinel-unchanged', 'containment-link-results', 'containment-rsync-results']


def exercise(w, run, check, rsync):
    source = w.dir/'upload'
    source.mkdir()
    outside = w.dir/'outside'
    outside.mkdir()
    sentinel = outside/'sentinel'
    sentinel.write_bytes(b'original')
    sentinel.chmod(0o640)
    for folder in ['source/nested', 'file-dest/source', 'dir-dest/source', 'base-dest', 'move-dest', 'remove-tree', 'find-tree', 'rsync-tree']:
        (source/folder).mkdir(parents=True, exist_ok=True)
    for name in ['source/file', 'source/nested/sentinel', 'move-file']:
        (source/name).write_bytes(b'OVERWRITTEN')
    links = {
        'file-dest/source/file': sentinel, 'dir-dest/source/nested': outside,
        'base-dest/source': outside, 'move-dest/move-file': sentinel,
        'dir-link': outside, 'file.marathon-tmp': sentinel, 'write-link': sentinel,
        'remove-link': outside, 'move-link': outside,
        'remove-tree/link': outside, 'find-tree/link': outside,
        'rsync-tree/file': sentinel, 'rsync-tree/nested': outside,
        'rsync-tree/.rsync-partial': outside,
    }
    for name, target in links.items(): os.symlink(target, source/name)
    flags = [rsync, '-rlpt', '--partial', '--partial-dir=.rsync-partial', '--delay-updates', '--rsh', shlex.join([str(x) for x in w.ssh()[:-1]]), '--rsync-path=/usr/bin/rsync']
    run('containment-upload', flags[:2]+flags[5:]+[str(source)+'/', '127.0.0.1:/tmp/marathon/'])
    check('containment-uploaded-links', all(os.readlink(w.dir/'root'/name)==str(target) for name,target in links.items()))
    for name, (command, expected) in COMMANDS.items(): run(name, w.ssh()+[command], expected=expected)
    run('rsync-replaces-descendant-links', flags+[str(source/'source')+'/', '127.0.0.1:/tmp/marathon/rsync-tree/'])
    run('rsync-link-operand', flags+[str(source/'source')+'/', '127.0.0.1:/tmp/marathon/dir-link/'], expected=255 if sys.platform=='darwin' else 12)
    check('containment-sentinel-unchanged', sentinel.read_bytes()==b'original' and sentinel.stat().st_mode&0o777==0o640 and sorted(p.name for p in outside.iterdir())==['sentinel'])
    root=w.dir/'root'
    check('containment-link-results', (root/'write-link').read_bytes()==b'X' and not (root/'write-link').is_symlink() and not (root/'remove-link').is_symlink() and (root/'moved-link').is_symlink() and not (root/'remove-tree').exists() and not list((root/'find-tree').iterdir()))
    check('containment-rsync-results', (root/'rsync-tree/file').read_bytes()==b'OVERWRITTEN' and not (root/'rsync-tree/file').is_symlink() and not (root/'rsync-tree/nested').is_symlink())
