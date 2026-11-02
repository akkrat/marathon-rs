"""Build recipes run only by the orchestrator, in a new disposable staging tree."""
from pathlib import Path
import shutil
import platform
import re
import subprocess
import tarfile
import zipfile
from common import HERE, ROOT, e, execute, inventory, require, save, select_build_source

ANDROID_INIT_SCRIPT = 'gradle-jdk21.init.gradle'
# Gradle's default 512 MB daemon heap makes D8 run out of memory merging the test APK's dex files. The setting goes
# into the build's private HOME, not into the sample.
ANDROID_GRADLE_PROPERTIES = 'org.gradle.jvmargs=-Xmx3g\n'


def recipes(vendor, stage, host):
    stage = Path(stage)
    if vendor == 'Android':
        return [(stage / 'sample/android-app', ['./gradlew', '--no-daemon', '--init-script', str(stage / ANDROID_INIT_SCRIPT),
                                                ':app:assembleDebug', ':app:assembleDebugAndroidTest'])]
    if vendor == 'iOS':
        cwd = stage / 'sample/ios-app'
        return [(cwd, ['xcodebuild', '-list', '-json', '-workspace', 'sample-app.xcworkspace']),
                (cwd, ['xcodebuild', 'build-for-testing', '-workspace', 'sample-app.xcworkspace', '-scheme', 'UITesting',
                       '-sdk', 'iphonesimulator', '-destination', 'generic/platform=iOS Simulator',
                       '-derivedDataPath', str(stage / 'derived')])]
    # The sample project is the fake-world fixture and sets CODE_SIGNING_ALLOWED=NO itself, which a bare
    # build would keep. Then Xcode copies its XCTRunner template (Apple-signed, other bundle id) without
    # re-signing it and Gatekeeper rejects the renamed runner on first launch. Command-line settings
    # beat the project's: sign locally (ad hoc), as an Xcode user's default build does.
    return [(stage, ['xcodebuild', 'build-for-testing', '-project', 'sample/macos-app/AppleWorld.xcodeproj',
                     '-scheme', 'AppleWorld', '-sdk', 'macosx', '-destination', 'platform=macOS,arch=arm64',
                     '-derivedDataPath', str(stage / 'derived'),
                     'CODE_SIGNING_ALLOWED=YES', 'CODE_SIGN_IDENTITY=-'])]


def build(vendor, host, destination, kind='frozen'):
    require(Path(host['worker_home']).resolve() == Path.home().resolve(), 'build must run on the configured worker account')
    require(platform.system() == ('Linux' if vendor == 'Android' else 'Darwin'), 'wrong build host OS')
    if vendor != 'Android':
        version = subprocess.check_output(['xcodebuild', '-version'], text=True, timeout=30)
        expected = '27' if host['profile'] == 'local-27' else '26.6'
        require(re.search(r'^Xcode ' + re.escape(expected) + r'(?:\.|\s|$)', version), 'wrong build toolchain')
    stage = Path(destination).resolve()
    kotlin = host.get('kotlin_source')
    if kotlin:
        require(not stage.is_relative_to(Path(kotlin).resolve()), 'never build inside the frozen source')
    # WP 7.5: the source is chosen once, here, and recorded in build.json; every later step reads it from there.
    source, samples = select_build_source(vendor, kotlin, kind)
    stage.mkdir(parents=True, exist_ok=False)
    if vendor == 'macOS':
        shutil.copytree(ROOT / 'sample/macos-app', stage / 'sample/macos-app')
        src = stage / 'sample/macos-app/Tests/SmokeTests.m'
        text = src.read_text()
        require(text.endswith('@end\n'), 'sample overlay anchor changed')
        src.write_text(text[:-5] + (HERE / 'slow-test.m.inc').read_text() + '@end\n')
        save(stage / 'overlay.json', {'source': e.sha((ROOT / 'sample/macos-app/Tests/SmokeTests.m').read_bytes()),
                                    'overlay': e.sha((HERE / 'slow-test.m.inc').read_bytes())})
    else:
        commit = source
        sample = 'sample/ios-app' if vendor == 'iOS' else 'sample/android-app'
        archive = stage / 'sample.tar'
        with archive.open('wb') as target:
            subprocess.run(['git', '-C', kotlin, 'archive', commit, sample], stdout=target, check=True, timeout=120)
        with tarfile.open(archive) as source_tar:
            for member in source_tar.getmembers():
                require(not member.name.startswith('/') and '..' not in Path(member.name).parts,
                        'archive traversal')
                require(member.isfile() or member.isdir(), 'unexpected link in source sample archive')
            source_tar.extractall(stage, filter='data')
    env = dict(PATH='/usr/bin:/bin:/usr/sbin:/sbin', HOME=str(stage / 'build-home'),
               JAVA_HOME=host['java_home'], LANG='en_US.UTF-8', LC_ALL='en_US.UTF-8')
    Path(env['HOME']).mkdir()
    env.update(host.get('environment', {}))
    if vendor == 'Android':
        env.update(ANDROID_HOME=host['sdk'], ANDROID_SDK_ROOT=host['sdk'])
    if vendor == 'Android':
        # The overlay lives beside the sample, not in it, so the extracted sample stays byte-identical.
        shutil.copy2(HERE / 'templates' / ANDROID_INIT_SCRIPT, stage / ANDROID_INIT_SCRIPT)
        (Path(env['HOME']) / '.gradle').mkdir()
        (Path(env['HOME']) / '.gradle/gradle.properties').write_text(ANDROID_GRADLE_PROPERTIES)
        save(stage / 'overlay.json', {'init_script': e.sha((HERE / 'templates' / ANDROID_INIT_SCRIPT).read_bytes()),
                                      'gradle_properties': ANDROID_GRADLE_PROPERTIES})
    steps = recipes(vendor, stage, host)
    results = []
    for n, (cwd, argv) in enumerate(steps):
        result = execute(argv, cwd, env, stage / f'build-{n}', timeout=1800)
        results.append(result)
        require(result['exit'] == 0 and not result['watchdog'] and result['joined'], 'sample build failed; retain attempt')
        if vendor == 'iOS' and n == 0:
            schemes = e.loads((stage / 'build-0/stdout').read_bytes())['workspace']['schemes']
            require('UITesting' in schemes, 'missing UITesting scheme')
            # The historical script names this second scheme. Build it only when
            # actually present, and require hosted products below in either case.
            if 'sample-appUnitTests' in schemes:
                args = list(steps[1][1]); args[args.index('-scheme') + 1] = 'sample-appUnitTests'
                steps.append((cwd, args))
    if vendor == 'Android':
        products = stage / 'sample/android-app/app/build/outputs/apk'
        for name in ('debug/app-debug.apk', 'androidTest/debug/app-debug-androidTest.apk'):
            require((products / name).is_file(), 'missing APK')
        with zipfile.ZipFile(products / 'androidTest/debug/app-debug-androidTest.apk') as apk:
            dex = {n: apk.read(n)[:8].hex() for n in apk.namelist() if n.endswith('.dex')}
        require(dex, 'test APK has no DEX')
        save(stage / 'dex-magic.json', dex)
    else:
        products = stage / 'derived/Build/Products'
        required = ('sample-app.app', 'sample-appUnitTests.xctest', 'sample-appUITests-Runner.app') if vendor == 'iOS' else (
            'AppleWorld.app', 'AppleWorldTests.xctest', 'AppleWorldUITests-Runner.app')
        for name in required:
            require(any(products.rglob(name)), 'missing real build product: ' + name)
    manifest = dict(source=source, source_kind=kind, samples=samples, native_source=subprocess.check_output(['git', '-C', ROOT, 'rev-parse', 'HEAD'], text=True).strip(),
                    vendor=vendor, worker_id=host['worker_id'], profile=host['profile'],
                    products_path=str(products), products=inventory(products), commands=results)
    save(stage / 'build.json', manifest)
    return manifest
