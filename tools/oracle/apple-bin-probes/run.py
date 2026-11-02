#!/usr/bin/env python3
"""WP 4.4 oracle: compile the frozen bin/, model/ and test/TestRequest.kt sources with Probe.kt against the
read-only installed jars, prepare AppleTestBundle witness directories from fixtures/bundles/*.zip in a temp
dir, and write fixtures/oracle/apple-bin/probes.jsonl. Usage: run.py [output]"""
import os, pathlib, plistlib, shutil, subprocess, sys, tempfile, zipfile

here = pathlib.Path(__file__).resolve().parent
repo = here.parents[2]
source = pathlib.Path(os.environ.get('MARATHON_KT', os.path.expanduser('~/src/marathon')))
lib = pathlib.Path(os.environ.get('MARATHON_KOTLIN_LIB', str(source / 'cli/build/install/marathon/lib')))
cache = pathlib.Path.home() / '.gradle/caches/modules-2/files-2.1'
def jar(g, a, v): return next((cache / g / a / v).glob('*/*.jar'))
compiler = [jar('org.jetbrains.kotlin', 'kotlin-compiler-embeddable', '1.9.24'), jar('org.jetbrains.kotlin', 'kotlin-stdlib', '1.9.24'),
            jar('org.jetbrains.kotlin', 'kotlin-script-runtime', '1.9.24'), jar('org.jetbrains.kotlin', 'kotlin-reflect', '1.6.10'),
            jar('org.jetbrains.intellij.deps', 'trove4j', '1.0.20200330'), jar('org.jetbrains', 'annotations', '13.0')]
java = str(pathlib.Path(os.environ.get('JAVA_HOME', '/Applications/Android Studio.app/Contents/jbr/Contents/Home')) / 'bin/java')
cp = os.pathsep.join(map(str, sorted(lib.glob('*.jar'))))
base = source / 'vendor/vendor-apple/base/src/main/kotlin/com/malinskiy/marathon/apple'
files = sorted(str(p) for p in [*(base / 'bin').rglob('*.kt'), *(base / 'model').glob('*.kt'), base / 'test/TestRequest.kt'])
out = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else repo / 'fixtures/oracle/apple-bin/probes.jsonl'

IOS = {'app': 'sample-app.app', 'runner': 'sample-appUITests-Runner.app', 'xctest': 'sample-appUITests-Runner.app/PlugIns/sample-appUITests.xctest'}
MAC = {'app': 'AppleWorld.app', 'runner': 'AppleWorldUITests-Runner.app', 'xctest': 'AppleWorldUITests-Runner.app/Contents/PlugIns/AppleWorldUITests.xctest'}
MACHO = bytes.fromhex('cffaedfe') + b'\0' * 28

def case(root, name, zipname, spec, sdk, edit=None, retry=None):
    d = root / name
    d.mkdir()
    if zipname:
        zipfile.ZipFile(repo / 'fixtures/bundles' / zipname).extractall(d)
    if edit:
        edit(d)
    (d / 'spec.txt').write_text(''.join(f'{k}={v}\n' for k, v in {**spec, 'sdk': sdk}.items()))
    if retry:
        (d / 'after-first-access.sh').write_text(retry)

def drop_key(path, key):
    def edit(d):
        p = d / path
        data = plistlib.loads(p.read_bytes())
        del data[key]
        p.write_bytes(plistlib.dumps(data))
    return edit

def set_key(path, key, value):
    def edit(d):
        p = d / path
        data = plistlib.loads(p.read_bytes())
        data[key] = value
        p.write_bytes(plistlib.dumps(data, fmt=plistlib.FMT_BINARY))
    return edit

def files_in(xctest, entries):
    def edit(d):
        x = d / xctest
        for f in x.iterdir():
            if f.name != 'Info.plist' and f.is_file():
                f.unlink()
        for name, content in entries.items():
            p = x / name
            if isinstance(content, tuple):
                os.symlink(content[0], p)
            else:
                p.write_bytes(content)
                if name == 'unreadable':
                    p.chmod(0)
    return edit

def prepare(root):
    case(root, 'ios-ok', 'ios.zip', IOS, 'IPHONESIMULATOR')
    case(root, 'ios-unit-no-runner', 'ios.zip', {'app': IOS['app'], 'runner': '', 'xctest': 'sample-app.app/PlugIns/sample-appUnitTests.xctest'}, 'IPHONEOS')
    case(root, 'macos-ok', 'macos.zip', MAC, 'MACOS')
    case(root, 'macos-as-ios', 'macos.zip', MAC, 'IPHONEOS')
    case(root, 'ios-as-macos', 'ios.zip', IOS, 'MACOS')
    case(root, 'no-app', 'ios.zip', {**IOS, 'app': ''}, 'IPHONESIMULATOR')
    case(root, 'app-missing-dir', 'ios.zip', {**IOS, 'app': 'absent.app'}, 'IPHONESIMULATOR')
    case(root, 'no-bundle-identifier', 'ios.zip', IOS, 'IPHONESIMULATOR', drop_key(IOS['app'] + '/Info.plist', 'CFBundleIdentifier'))
    case(root, 'no-bundle-name', 'ios.zip', IOS, 'IPHONESIMULATOR', drop_key(IOS['xctest'] + '/Info.plist', 'CFBundleName'))
    case(root, 'bundle-name-separators', 'ios.zip', IOS, 'IPHONESIMULATOR', set_key(IOS['xctest'] + '/Info.plist', 'CFBundleName', 'My-Test Name_x-'))
    case(root, 'bundle-identifier-wrong-type', 'ios.zip', IOS, 'IPHONESIMULATOR', set_key(IOS['app'] + '/Info.plist', 'CFBundleIdentifier', 5))
    case(root, 'multi-stem', 'ios.zip', IOS, 'IPHONESIMULATOR', files_in(IOS['xctest'], {'Zother': MACHO, 'sample-appUITests': MACHO, 'Aother': MACHO}))
    case(root, 'filters', 'ios.zip', IOS, 'IPHONESIMULATOR', files_in(IOS['xctest'], {
        'text': b'#!/bin/sh\n', 'lib.dylib': MACHO, '.hidden': MACHO, 'short': MACHO[:3], 'fat64': bytes.fromhex('cafebabf') + b'\0' * 28,
        'trailing.': MACHO, 'dangling': ('/nonexistent/target',), 'link': ('trailing.',)}))
    case(root, 'no-binaries', 'ios.zip', IOS, 'IPHONESIMULATOR', files_in(IOS['xctest'], {'text': b'x'}),
         retry=f"cp '{IOS['xctest']}/../../sample-appUITests-Runner' '{IOS['xctest']}/sample-appUITests'\n")
    case(root, 'unreadable', 'ios.zip', IOS, 'IPHONESIMULATOR', files_in(IOS['xctest'], {'unreadable': MACHO}))
    case(root, 'dotted-path', 'ios.zip', {**IOS, 'xctest': './' + IOS['xctest'].replace('/PlugIns/', '//PlugIns/') + '/'}, 'IPHONESIMULATOR')
    case(root, 'app-no-binaries', 'ios.zip', IOS, 'IPHONESIMULATOR', lambda d: [p.unlink() for p in (d / IOS['app']).iterdir() if p.is_file() and p.name not in ('Info.plist',)])

with tempfile.TemporaryDirectory(prefix='mrs44-probe-') as tmp:
    tmp = pathlib.Path(tmp)
    classes = tmp / 'classes'
    subprocess.run([java, '-cp', os.pathsep.join(map(str, compiler)), 'org.jetbrains.kotlin.cli.jvm.K2JVMCompiler', '-no-stdlib', '-no-reflect',
                    '-nowarn', '-jvm-target', '17', '-cp', cp, '-d', str(classes), str(here / 'Probe.kt'), *files], check=True)
    bundles = tmp / 'bundles'
    bundles.mkdir()
    prepare(bundles)
    result = subprocess.run([java, '-cp', str(classes) + os.pathsep + cp, 'ProbeKt', str(repo / 'fixtures'), str(bundles)],
                            check=True, stdout=subprocess.PIPE, timeout=300, cwd=bundles)
    lines = [l for l in result.stdout.decode().split('\n') if l.startswith('{"case"')]
    # Witness paths are temp dirs: make them stable.
    text = '\n'.join(lines).replace(str(bundles.resolve()), '<bundles>').replace(str(bundles), '<bundles>') + '\n'
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(text)
    print(f'{len(lines)} cases -> {out}')
