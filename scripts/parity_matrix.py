#!/usr/bin/env python3
"""Generate docs/parity-matrix.csv: one row per Kotlin test (and per static parameterized invocation) of the
reference marathon checkout, with the work package (PLAN.md §7) that ports it and its porting status.

Usage: scripts/parity_matrix.py [--check] /path/to/marathon

On regeneration the status, rust_test and commit of rows that are found again are kept. With --check the
CSV is regenerated in memory and the script fails if the committed file differs; nothing is written.
Both modes fail if a row has no wp or an `n/a` row has no reason in rust_test.
"""
import csv
import io
import pathlib
import re
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from source_inventory import owner, test_owner  # noqa: E402  (the RULES live there; never copy them)

OUT = pathlib.Path(__file__).resolve().parent.parent / "docs" / "parity-matrix.csv"
COLUMNS = ["source_set", "kotlin_file", "test_class", "test_name", "kind", "invocation", "wp", "status",
           "rust_test", "commit"]
KEY = COLUMNS[:6]
SOURCE_SET = re.compile(r"^(?P<module>.+)/src/(?P<set>test|integrationTest)/")

# Placeholder rows for the test suites of the replaced libraries; WP 6.4 expands its row.
# WP 6.3 expanded the adam instrumentation placeholder into one row per test of
# adam 0.5.8's InstrumentationResponseTransformerTest (16), plus the single
# ProtoInstrumentationResponseTransformerTest test (n/a under WP 6.3 P1).
_ADAM_INSTRUMENTATION_TESTS = [
    "testSingleFailure", "testComplex1", "testComplex2", "testNoResultsReported",
    "testIncompleteTests", "testIncompleteTests2", "testOtherFailure", "testBufferFraming",
    "testWindowsLineEnding", "testSingleBlock", "testFailureHandling", "testMetrics",
    "testRunMetrics", "testSingleTestMetric", "testSingleIgnoredTest", "missingListenerTest",
]
# WP 6.4 expanded the dex-test-parser placeholder into one row per test of
# tag 2.3.4's DexParserShould (21) and AbstractClassInSecondDex (2).
_DEX_PARSER_SHOULD_TESTS = [
    "parseCorrectNumberOfTestMethods", "parseMethodWithMultipleMethodAnnotations",
    "parseMethodWithChildclassAnnotation", "parseInheritedMethodAnnotation",
    "parsNonInheritedMethodAnnotation", "parseInheritedClassAnnotation",
    "parseStringAnnotationValues", "parseIntAnnotationValues", "parseBoolAnnotationValues",
    "parseLongAnnotationValues", "parseFloatAnnotationValues", "parseDoubleAnnotationValues",
    "parseByteAnnotationValues", "parseFloatMaxValuesInDoubleFields", "parseCharAnnotationValues",
    "parseShortAnnotationValues", "parseClassArrayAnnotationnValues",
    "parseMultipleValuesInASingleAnnotation", "parseEnumAnnotation", "parseTypeAnnotation",
    "parseAbstractClassTestMethodCorrectly",
]
_ABSTRACT_CLASS_IN_SECOND_DEX_TESTS = [
    "parseMethodFromBaseAbstractClass_whenAbstractClassInTheSecondDex",
    "parseMethodFromConcreteClassThatExtendsFromAbstract_whenAbstractClassInTheSecondDex",
]
# WP 6.1 expanded the rest of adam 0.5.8's tag tests (`adam/src/test`, 237 in all with 6.3's 17), per its
# orchestrator decision 1. A class whose simple name is not unique carries its package after `request.`.
# A non-empty reason makes a pending external row n/a.
_NOT_CALLED = "WP 6.1 P1: marathon never calls this adam code at e47352f2, so it is not ported"
_UNUSED = "adam API marathon never uses at e47352f2 (WP 6.1 decision 1)"
_ADAM_CLIENT_TESTS = [
    # 6.1: 22
    ("AndroidDebugBridgeClientTest", ["testRequestRejection", "testRequestRejectionDuringHandshake", "testRequestRejectionDuringHandshakeWithNoMessage", "testRequestRejectionAndUnexpectedMessageLength", "testRequestValidation"], "6.1", ""),
    ("AsyncShellCommandRequestTest", ["testSimpleCommand"], "6.1", ""),
    ("TargetTest", ["testSerialization"], "6.1", ""),
    ("AbbExecRequestTest", ["testSerialize", "testValidation", "testValidationFailure", "testDummy"], "6.1", ""),
    ("AsyncDeviceMonitorRequestTest", ["testReturnsProperContent"], "6.1", ""),
    ("FetchDeviceFeaturesRequestTest", ["testSerialization", "testResponse"], "6.1", ""),
    ("ListDevicesRequestTest", ["testReturnsProperContent"], "6.1", ""),
    ("GetAdbServerVersionRequestTest", ["testReturnsProperVersion"], "6.1", ""),
    ("shell.v1.ChanneledShellCommandRequestTest", ["testReturnsProperContent"], "6.1", ""),
    ("shell.v1.ShellCommandRequestTest", ["testReturnsProperContent", "testReturnsNonStrippedStdout"], "6.1", ""),
    ("ShellResultResponseTransformerTest", ["testNoExitCodeDelimiter", "testInvalidExitCode"], "6.1", ""),
    ("shell.v2.ChanneledShellCommandRequestTest", ["testReturnsProperContent"], "6.1", ""),
    # 6.1 n/a: 13
    ("AbbRequestTest", ["testSerialize", "testValidation", "testValidationFailure"], "6.1", _NOT_CALLED),
    ("ExecInRequestTest", ["testSerialize"], "6.1", _NOT_CALLED),
    ("FetchHostFeaturesRequestTest", ["testSerialization", "testResponse"], "6.1", _NOT_CALLED),
    ("KillAdbRequestTest", ["testSerialization", "testDummy"], "6.1", _NOT_CALLED),
    ("shell.v2.ShellCommandRequestTest", ["testReturnsNonStrippedStdout", "testUnsupportedWINDOW_SIZE_CHANGE", "testUnsupportedSTDIN", "testUnsupportedINVALID", "testUnsupportedCLOSE_STDIN"], "6.1", _NOT_CALLED),
    # 6.2: 105
    ("AsyncFileReaderTest", ["testExtension"], "6.2", ""),
    ("AsyncFileWriterTest", ["testWrite"], "6.2", ""),
    ("ScreenCaptureRequestTest", ["testProtocol1", "testProtocol16bit", "test with buffered image adapter", "test with buffered image adapter 16 bit", "test with buffered image adapter unaligned 32 bit ", "test with buffered image adapter with srgb color model", "testProtocolUnsupported"], "6.2", ""),
    ("BufferedImageScreenCaptureAdapterTest", ["testThrowsExceptionIfUnsupportedImage"], "6.2", ""),
    ("InstallRemotePackageRequestTest", ["testSerialize", "testReinstall", "testMultipleReinstall", "testValidation"], "6.2", ""),
    ("LegacySideloadRequestTest", ["testSerialize", "testTransfer", "testTransferFailure", "testValidation"], "6.2", ""),
    ("PmListRequestTest", ["testReturnsProperContent", "testReturnsProperContentWithPath"], "6.2", ""),
    ("SideloadRequestTest", ["testSerialize", "testTransfer", "testTransferFailure", "testValidation"], "6.2", ""),
    ("UninstallRemotePackageRequestTest", ["testSerialize", "testSerializeWithRemoveDataFlag"], "6.2", ""),
    ("AddSessionRequestTest", ["serialize", "serializeAbb", "testRead", "testReadException"], "6.2", ""),
    ("CreateIndividualPackageSessionRequestTest", ["serialize", "serializeAbb", "serializeNoFeatures", "serializeCustomized", "serializedApex", "testRead", "testReadException", "testReadNoSession"], "6.2", ""),
    ("CreateMultiPackageSessionRequestTest", ["serialize", "serializeAbb", "serializeReinstall", "serializeApex", "serializeApkSplit", "testRead", "testReadException", "testReadNoSession", "testValidation", "testValidationFailureFileNotFound", "testValidationFailureNonApk", "testValidationFailureFolder", "testValidationFailureFeatures", "testValidationApex", "testValidationApkSplit", "testValidationFailureApkSplit"], "6.2", ""),
    ("InstallCommitRequestTest", ["serialize", "serializeAbb", "serializeAbandon", "testRead", "testReadException", "testSerializeNoFeatures"], "6.2", ""),
    ("WriteIndividualPackageRequestTest", ["serialize", "serializeAbb", "serializeNoFeatures", "testRead", "testReadException"], "6.2", ""),
    ("ListReversePortForwardsRequestTest", ["testReturnsProperContent"], "6.2", ""),
    ("RemoveAllReversePortForwardsRequestTest", ["testSerializeDefault", "testDummy"], "6.2", ""),
    ("RemoveReversePortForwardRequestTest", ["testSerializeDefault", "testDummy"], "6.2", ""),
    ("ReversePortForwardRequestTest", ["testSerializeDefault", "testSerializeNoDefault", "testRead", "testReadFailure"], "6.2", ""),
    ("ListFilesRequestTest", ["testReturnsProperContent"], "6.2", ""),
    ("CompatListFileRequestTest", ["testV1", "testV2"], "6.2", ""),
    ("CompatPullFileRequestTest", ["testV1", "testV2"], "6.2", ""),
    ("CompatPushFileRequestTest", ["testV1", "testV2"], "6.2", ""),
    ("CompatStatFileRequestTest", ["testV1", "testReturnsProperContent"], "6.2", ""),
    ("sync.v1.ListFileRequestTest", ["testSerialize"], "6.2", ""),
    ("sync.v1.PullFileRequestTest", ["testSerialize", "testSingleChunkHappyPath", "testMultipleChunksHappyPath", "testTransportFail", "testTransportPacketSizeFailure", "testTransportUnsupportedPacket"], "6.2", ""),
    ("sync.v1.PushFileRequestTest", ["testSerialize", "testSingleChunkHappyPath", "testTransportFailureOnDone"], "6.2", ""),
    ("sync.v1.StatFileRequestTest", ["testReturnsProperContent"], "6.2", ""),
    ("sync.v2.ListFileRequestTest", ["testSerialize"], "6.2", ""),
    ("sync.v2.PullFileRequestTest", ["testSerialize", "testSingleChunkHappyPath", "testMultipleChunksHappyPath", "testTransportFail", "testTransportPacketSizeFailure", "testTransportUnsupportedPacket"], "6.2", ""),
    ("sync.v2.PushFileRequestTest", ["testSerialize", "testSingleChunkHappyPath", "testTransportFailureOnDone"], "6.2", ""),
    ("sync.v2.StatFileRequestTest", ["testReturnsProperContent"], "6.2", ""),
    # 6.5: 12
    ("AdbBinaryIntegrationTest", ["testStartAndKillAdbServer"], "6.5", ""),
    ("DiscoverAdbSocketInteractorTest", ["testDefault", "testProperty"], "6.5", ""),
    ("AsyncLogcatRequestTest", ["testModeArguments", "testBuffers", "testContinuous", "testSinceContinuous", "testSinceYearContinuous", "testSinceTimestampContinuous", "testFilterspec"], "6.5", ""),
    ("GetPropRequestTest", ["testGetAll", "testReturnsProperContent"], "6.5", ""),
    # 6.6a: 10
    ("InstrumentOptionsTest", ["testDefault", "testAll", "testAllWithProperties", "testMultipleOverrides"], "6.6a", ""),
    ("TestRunnerRequestTest", ["testSerialize", "testDefaultSerialize", "testV1", "testV2", "testV2IgnoresStderr", "testReturnsContentOnFailure"], "6.6a", ""),
    # n/a: 58
    ("GetSinglePropRequestTest", ["testGetX", "testReturnsProperContent"], "n/a", _UNUSED),
    ("RestartAdbdRequestTest", ["testSerializeRootMode", "testSerializeUnrootMode", "testSerializeUsbMode", "testSerializeTcpIpMode", "testReturnsResultString"], "n/a", _UNUSED),
    ("EmulatorCommandRequestTest", ["testHappyPath"], "n/a", _UNUSED),
    ("ListPortForwardsRequestTest", ["testReturnsProperContent"], "n/a", _UNUSED),
    ("LocalPortSpecTest", ["testUnsupported", "testLocalUnixSocketPortSpec"], "n/a", _UNUSED),
    ("PortForwardRequestTest", ["testSerializeDefault", "testSerializeNoDefault", "testRead", "testReadFailure"], "n/a", _UNUSED),
    ("RemotePortSpecTest", ["testUnsupported", "testRemoteAbstractPortSpec", "testRemoteReservedPortSpec", "testRemoteDevPortSpec", "testJDWPPortSpec"], "n/a", _UNUSED),
    ("RemoveAllPortForwardsRequestTest", ["testSerializeDefault", "testDummy"], "n/a", _UNUSED),
    ("RemovePortForwardRequestTest", ["testSerializeDefault", "testDummy"], "n/a", _UNUSED),
    ("SyncLogcatRequestTest", ["testSinceNonBlocking", "testSinceYearNonBlocking", "testSinceTimeStampNonBlocking", "testDefault"], "n/a", _UNUSED),
    ("ListMdnsServicesRequestTest", ["testSerialize", "testReturnsContent"], "n/a", _UNUSED),
    ("MdnsCheckRequestTest", ["testSerialize", "testReturnsContent", "testReturnsUnavailable"], "n/a", _UNUSED),
    ("ConnectDeviceRequestTest", ["testSerialize", "testSerializeWithPort", "testReturnsResultString"], "n/a", _UNUSED),
    ("DisconnectDeviceRequestTest", ["testSerialize", "testSerializeWithPort", "testSerializeDisconnectAll", "testReturnsResultString"], "n/a", _UNUSED),
    ("PairDeviceRequestTest", ["testSerialize", "testResponse"], "n/a", _UNUSED),
    ("RebootRequestTest", ["testDefault", "testBootloader", "testRecovery", "testSideload", "testSideloadAutoReboot", "testDummy"], "n/a", _UNUSED),
    ("ReconnectRequestTest", ["testSerialize", "testSerializeOffline", "testSerializeDevice", "testResponse", "testResponseForSingleTarget"], "n/a", _UNUSED),
    ("RemountPartitionsRequestTest", ["testSerialize", "testSerializeAutoReboot", "testReturnsProperContent"], "n/a", _UNUSED),
    ("SetDmVerityCheckingRequestTest", ["testSerialize", "testRead"], "n/a", _UNUSED),
]
# WP 6.2 decision P1: these 33 unused APIs stay assigned but are deliberately omitted.
_ADAM_62_UNUSED = {
    "LegacySideloadRequestTest", "SideloadRequestTest", "PmListRequestTest",
    "CreateMultiPackageSessionRequestTest", "AddSessionRequestTest",
    "ListReversePortForwardsRequestTest", "RemoveAllReversePortForwardsRequestTest",
}
_ADAM_CLIENT_TESTS = [
    (cls, tests, wp, "marathon never calls this adam API at e47352f2"
     if wp == "6.2" and cls in _ADAM_62_UNUSED else reason)
    for cls, tests, wp, reason in _ADAM_CLIENT_TESTS
]

# WP 3.10: the rebased commit that added each run-flow suite.
_RUN_FLOW_310_IMPLEMENTATION = {
    "marathon_scenarios": "28ec57ce6e5e8e5e8c61edc7e590966d8b59347a",
    "startup_timings": "a7bb69ff9479cad27431254008d3ac52b1633307",
    "parse_command": "a7bb69ff9479cad27431254008d3ac52b1633307",
}

# Test locations are kept here so WP 6.2's status update is reproducible, not a CSV hand edit.
_ADAM_62_IMPLEMENTATION = "209b91400cd62d328906cdaf20b500a8acfbfd44"
_ADAM_62_MODULES = {
    "AsyncFileReaderTest": ("sync", "async_file_reader_test"),
    "AsyncFileWriterTest": ("sync", "async_file_writer_test"),
    "ScreenCaptureRequestTest": ("framebuffer", "screen_capture_request_test"),
    "BufferedImageScreenCaptureAdapterTest": ("framebuffer", "buffered_image_screen_capture_adapter_test"),
    "ListFilesRequestTest": ("sync", "list_files_request_test"),
}


def adam_62_port(cls, method):
    def snake(name):
        name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", name.strip())
        return re.sub(r"[^a-zA-Z0-9]+", "_", name).strip("_").lower()

    if cls in _ADAM_62_MODULES:
        group, module = _ADAM_62_MODULES[cls]
    elif cls.startswith("sync."):
        _, version, name = cls.split(".")
        group, module = "sync", version + "::" + snake(name)
    elif cls.startswith("Compat"):
        group, module = "sync", snake(cls)
    elif cls in {"ReversePortForwardRequestTest", "RemoveReversePortForwardRequestTest"}:
        group, module = "reverse", snake(cls)
    else:
        group, module = "pkg", snake(cls)
    return ["ported", f"crates/marathon-adb/src/client/{group}/tests.rs::{module}::{snake(method)}",
            _ADAM_62_IMPLEMENTATION]

EXTERNAL = [
    *[("external", "adam@0.5.8", "InstrumentationResponseTransformerTest", t, "external", "", "6.3")
      for t in _ADAM_INSTRUMENTATION_TESTS],
    ("external", "adam@0.5.8", "ProtoInstrumentationResponseTransformerTest", "testSingleFailure",
     "external", "", "6.3"),
    *[("external", "adam@0.5.8", cls, t, "external", "", wp, reason)
      for cls, tests, wp, reason in _ADAM_CLIENT_TESTS for t in tests],
    ("external", "adam@0.5.8", "server-stub tests", "*", "external", "", "0.5"),
    *[("external", "dex-test-parser@2.3.4", "DexParserShould", t, "external", "", "6.4")
      for t in _DEX_PARSER_SHOULD_TESTS],
    *[("external", "dex-test-parser@2.3.4", "AbstractClassInSecondDex", t, "external", "", "6.4")
      for t in _ABSTRACT_CLASS_IN_SECOND_DEX_TESTS],
]


# ---- lexing ---------------------------------------------------------------------------------------------

def blank(text: str) -> str:
    """Return text with comments and string-literal contents replaced by spaces (newlines kept), so that
    braces, parentheses and annotations can be found structurally. Offsets are preserved."""
    out = list(text)
    i, n = 0, len(text)
    # Stack of contexts: "code" (with brace depth) or a string kind; templates push code inside strings.
    stack = [["code", 0]]

    def wipe(a, b):
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    while i < n:
        ctx = stack[-1]
        c = text[i]
        if ctx[0] == "code":
            if text.startswith("//", i):
                j = text.find("\n", i)
                j = n if j < 0 else j
                wipe(i, j)
                i = j
            elif text.startswith("/*", i):
                depth, j = 1, i + 2
                while j < n and depth:
                    if text.startswith("/*", j):
                        depth, j = depth + 1, j + 2
                    elif text.startswith("*/", j):
                        depth, j = depth - 1, j + 2
                    else:
                        j += 1
                wipe(i, j)
                i = j
            elif text.startswith('"""', i):
                stack.append(["raw", 0])
                i += 3
            elif c == '"':
                stack.append(["str", 0])
                i += 1
            elif c == "`":
                # Backticked identifiers (test names) may hold quotes; keep them verbatim.
                j = text.find("`", i + 1)
                i = n if j < 0 else j + 1
            elif c == "'":
                j = i + 1
                while j < n and text[j] != "'":
                    j += 2 if text[j] == "\\" else 1
                wipe(i + 1, j)
                i = j + 1
            elif c == "{":
                ctx[1] += 1
                i += 1
            elif c == "}":
                if ctx[1] == 0 and len(stack) > 1:
                    stack.pop()  # end of a ${...} template
                    wipe(i, i + 1)
                else:
                    ctx[1] -= 1
                i += 1
            else:
                i += 1
        else:
            raw = ctx[0] == "raw"
            if raw and text.startswith('"""', i):
                while i + 3 < n and text[i + 3] == '"':  # """" ends with the last three quotes
                    i += 1
                stack.pop()
                i += 3
            elif not raw and c == '"':
                stack.pop()
                i += 1
            elif not raw and c == "\\":
                wipe(i, min(i + 2, n))
                i += 2
            elif text.startswith("${", i):
                wipe(i, i + 2)
                stack.append(["code", 0])
                i += 2
            else:
                wipe(i, i + 1)
                i += 1
    return "".join(out)


def match_close(code: str, i: int) -> int:
    """Index just past the bracket matching the one at code[i]."""
    pairs = {"(": ")", "[": "]", "{": "}"}
    opener, closer = code[i], pairs[code[i]]
    depth = 0
    for j in range(i, len(code)):
        if code[j] == opener:
            depth += 1
        elif code[j] == closer:
            depth -= 1
            if depth == 0:
                return j + 1
    return len(code)


def split_top(text: str, sep: str = ","):
    """Split text on sep at bracket depth 0, ignoring separators inside string literals."""
    code = blank(text)
    parts, depth, start = [], 0, 0
    for k, c in enumerate(code):
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif c == sep and depth == 0:
            parts.append(text[start:k])
            start = k + 1
    parts.append(text[start:])
    return [p.strip() for p in parts if p.strip()]


def collapse(s: str) -> str:
    return re.sub(r"\s+", " ", s).strip()


# ---- declarations -------------------------------------------------------------------------------------------

CLASS = re.compile(r"\b(?:class|object)\s+(\w+)")
FUN = re.compile(r"\bfun\s+(?:<[^>]*>\s*)?(`[^`\n]+`|\w+)\s*\(")
ANNOTATION = re.compile(r"@([\w.]+)")


def classes(code: str):
    """(name, body start, body end, parent index) for each class or object with a body."""
    found = []
    for m in CLASS.finditer(code):
        j = m.end()
        # Skip the primary constructor, supertypes and so on up to the body or the end of the declaration.
        while j < len(code) and code[j] not in "{\n;=":
            j = match_close(code, j) if code[j] in "([" else j + 1
        if j < len(code) and code[j] == "\n":
            k = j
            while k < len(code) and code[k] in " \t\n":
                k += 1
            if k < len(code) and code[k] in "{:":
                j = k
                while j < len(code) and code[j] != "{":
                    j = match_close(code, j) if code[j] in "([" else j + 1
        if j < len(code) and code[j] == "{":
            found.append((m.group(1), j, match_close(code, j)))
    return found


def enclosing_class(spans, pos):
    names = [(start, name) for name, start, end in spans if start < pos < end]
    return ".".join(name for _, name in sorted(names))


def annotation_block(code: str, text: str, fun_start: int):
    """Raw text of the annotations and modifiers directly in front of a fun declaration."""
    i = fun_start
    while True:
        k = i
        while k > 0 and code[k - 1] in " \t\n":
            k -= 1
        if k == 0:
            return text[i:fun_start]
        if code[k - 1] == ")":
            # Either an annotation's argument list or the end of the previous member.
            depth, j = 0, k - 1
            while j >= 0:
                if code[j] == ")":
                    depth += 1
                elif code[j] == "(":
                    depth -= 1
                    if depth == 0:
                        break
                j -= 1
            m = re.search(r"@[\w.]+\s*$", code[:j])
            if not m:
                return text[i:fun_start]
            i = m.start()
            continue
        m = re.search(r"(@[\w.:]+|\b(?:private|public|internal|protected|override|open|suspend|inline))\s*$",
                      code[:k])
        if not m:
            return text[i:fun_start]
        i = m.start()


def annotation_args(block: str, name: str):
    """Argument text of the first @name(...) in an annotation block, '' for a bare @name, None if absent."""
    m = re.search(r"@" + name + r"\b", block)
    if not m:
        return None
    rest = block[m.end():]
    stripped = rest.lstrip()
    if not stripped.startswith("("):
        return ""
    start = len(rest) - len(stripped)
    return rest[start + 1:match_close(rest, start) - 1]


# ---- argument sources ------------------------------------------------------------------------------------------

def array_items(expr: str):
    """Items of a Kotlin array literal: [a, b], arrayOf(a, b), intArrayOf(...) and so on."""
    expr = expr.strip()
    m = re.match(r"^(?:\w*[aA]rrayOf)?\s*[\[(]([\s\S]*)[\])]$", expr)
    if not m:
        return [expr]
    return split_top(m.group(1))


def named_args(args: str):
    out = {}
    parts = split_top(args)
    if not any(re.match(r"^\w+\s*=", p) for p in parts):
        return {"value": "[" + args + "]"} if len(parts) > 1 else ({"value": parts[0]} if parts else {})
    for part in parts:
        m = re.match(r"^(\w+)\s*=\s*([\s\S]*)$", part)
        if m:
            out[m.group(1)] = m.group(2)
        else:
            out["value"] = part
    return out


def value_source(args: str):
    for key, expr in named_args(args).items():
        if key in ("strings", "ints", "longs", "shorts", "bytes", "chars", "doubles", "floats", "booleans",
                   "classes"):
            return [collapse(v) for v in array_items(expr)]
    return None


def csv_source(args: str):
    params = named_args(args)
    expr = params.get("value", params.get("textBlock"))
    if expr is None:
        return None
    if expr.strip().startswith('"""'):
        lines = expr.strip()[3:-3].splitlines()
        return [collapse(line) for line in lines if line.strip()]
    return [collapse(v) for v in array_items(expr)]


def enum_constants(enums, name: str):
    body = enums.get(name)
    if body is None:
        return None
    # Work on the blanked body so that KDoc on the constants and string arguments can't confuse the split.
    code = blank(body)
    end = len(code)
    depth = 0
    for k, c in enumerate(code):
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        elif c == ";" and depth == 0:
            end = k
            break
    constants = []
    for part in split_top(code[:end]):
        m = re.match(r"^(?:@\w+(?:\([^)]*\))?\s*)*([A-Za-z_]\w*)", part)
        if m:
            constants.append(m.group(1))
    return constants or None


def enum_source(args: str, enums):
    params = named_args(args)
    m = re.match(r"^(\w+)::class$", params.get("value", "").strip())
    if not m:
        return None
    constants = enum_constants(enums, m.group(1))
    if constants is None:
        return None
    names = params.get("names")
    if names is not None:
        wanted = [v.strip('"') for v in array_items(names)]
        mode = params.get("mode", "INCLUDE").split(".")[-1]
        if mode == "INCLUDE":
            constants = [c for c in constants if c in wanted]
        elif mode == "EXCLUDE":
            constants = [c for c in constants if c not in wanted]
        else:
            return None
    return constants or None


def method_source(args: str, method: str, code: str, text: str, seen=()):
    params = named_args(args) if args else {}
    names = [v.strip().strip('"') for v in array_items(params["value"])] if "value" in params else [method]
    items = []
    for provider in names:
        if provider in seen:
            return None
        if "#" in provider:
            return None  # provider in another class
        m = re.search(r"\bfun\s+" + re.escape(provider) + r"\s*\(\s*\)", code)
        if not m:
            return None
        # The body is either `= expr` or `{ return expr }`; the expr must be a listOf/Stream.of literal.
        j = m.end()
        # XcresulttoolGetTest.xcodes projects the first Arguments.of column of versions().
        projection = re.match(
            r"\s*(?::[^={]*)?=\s*(\w+)\(\)\.map\s*\{\s*it\.get\(\)\[0\]\s+as\s+String\?\s*\}",
            code[j:],
        )
        if projection:
            source = method_source('"' + projection.group(1) + '"', method, code, text,
                                   (*seen, provider))
            if source is None:
                return None
            for entry in source:
                argument = re.fullmatch(r"Arguments\.of\(([\s\S]*)\)", entry)
                if not argument:
                    return None
                columns = split_top(argument.group(1))
                if not columns:
                    return None
                items.append(columns[0])
            continue
        lit = re.compile(r"\s*(?::[^={]*)?(?:=|\{\s*return)\s*(listOf|Stream\.of|arrayOf)\s*(?:<[^>]*>)?\s*\(")
        lm = lit.match(code, j)
        if not lm:
            return None
        open_paren = lm.end() - 1
        close = match_close(code, open_paren)
        after = code[close:close + 40].lstrip()
        if after.startswith("."):
            return None  # e.g. listOf(...).map { }
        items += [collapse(v) for v in split_top(text[open_paren + 1:close - 1])]
    return items


def invocations(block: str, method: str, code: str, text: str, enums):
    """List of invocation labels, or None when they can't be resolved statically."""
    args = annotation_args(block, "ValueSource")
    if args is not None:
        return value_source(args)
    args = annotation_args(block, "CsvSource")
    if args is not None:
        return csv_source(args)
    args = annotation_args(block, "EnumSource")
    if args is not None:
        return enum_source(args, enums)
    args = annotation_args(block, "MethodSource")
    if args is not None and len(re.findall(r"@\w+Source\b", block)) == 1:
        return method_source(args, method, code, text)
    return None


def label(i: int, value: str) -> str:
    value = value if len(value) <= 100 else value[:97] + "..."
    return f"[{i}] {value}"


# ---- scanning ------------------------------------------------------------------------------------------------

def commented_out(text: str) -> bool:
    """True if the whole file is commented out: nothing but comments (line or block) remains."""
    return bool(text.strip()) and not blank(text).strip()


def uncomment(text: str) -> str:
    """Remove one level of commenting: an enclosing /* ... */ block, then `//` line prefixes."""
    stripped = text.strip()
    if stripped.startswith("/*") and stripped.endswith("*/"):
        text = stripped[2:-2]
    return "\n".join(re.sub(r"^\s*// ?", "", line) for line in text.splitlines())


JUNIT_ANNOTATIONS = {
    "org.junit.jupiter.api.Test": "Test",
    "org.junit.Test": "Test",
    "kotlin.test.Test": "Test",
    "org.junit.jupiter.params.ParameterizedTest": "ParameterizedTest",
    "org.junit.jupiter.api.Disabled": "Disabled",
    "org.junit.Ignore": "Ignore",
    "kotlin.test.Ignore": "Ignore",
}


def annotation_names(text: str):
    """Map from the name an annotation is written with in this file to its JUnit meaning.
    Handles `import org.junit.jupiter.api.Test as JUnitTest` and a non-JUnit `Test` import shadowing JUnit's."""
    names = {v: v for v in JUNIT_ANNOTATIONS.values()}
    names.update({fq: short for fq, short in JUNIT_ANNOTATIONS.items()})
    for m in re.finditer(r"^\s*import\s+([\w.]+)(?:\s+as\s+(\w+))?\s*$", text, re.M):
        fq, alias = m.group(1), m.group(2)
        simple = fq.rsplit(".", 1)[-1]
        if fq in JUNIT_ANNOTATIONS:
            names[alias or simple] = JUNIT_ANNOTATIONS[fq]
        elif alias is None and simple in names and names.get(simple) == simple:
            del names[simple]  # e.g. a project class named Test imported without an alias
    return names


def load_enums(root: pathlib.Path):
    enums, seen = {}, set()
    for p in root.rglob("*.kt"):
        if "/build/" in str(p):
            continue
        text = p.read_text(encoding="utf-8", errors="replace")
        code = blank(text)
        for m in re.finditer(r"\benum\s+class\s+(\w+)[^{]*\{", code):
            name = m.group(1)
            body = text[m.end():match_close(code, m.end() - 1) - 1]
            if name in seen:
                enums.pop(name, None)  # ambiguous name: don't guess
            else:
                enums[name] = body
            seen.add(name)
    return enums


MAIN_BY_NAME = {}


def main_file(root: pathlib.Path, rel: str):
    """The main file a test covers: the same path in src/main without the Test suffix, else the only main
    file of the module with that name (tests are sometimes in another package)."""
    main = re.sub(r"/src/(test|integrationTest)/", "/src/main/", rel)
    main = re.sub(r"(Integration)?Test\.kt$", ".kt", main)
    if (root / main).is_file():
        return main
    module = SOURCE_SET.match(rel).group("module")
    if module not in MAIN_BY_NAME:
        index = {}
        for p in (root / module / "src" / "main").rglob("*.kt"):
            index.setdefault(p.name, []).append(str(p.relative_to(root)))
        MAIN_BY_NAME[module] = index
    found = MAIN_BY_NAME[module].get(pathlib.PurePath(main).name, [])
    return found[0] if len(found) == 1 else None


def wp_for(root: pathlib.Path, rel: str, name: str):
    """(wp, reason) for a test: explicit test rules, then the covered main file, then the test path."""
    match = test_owner(rel, name)
    if match:
        return match
    main = main_file(root, rel)
    match = owner(main) if main else None
    if not match:
        match = owner(rel)
    return match if match else ("", "")


def scan_file(root: pathlib.Path, rel: str, enums):
    source_set = SOURCE_SET.match(rel).group("set")
    text = (root / rel).read_text(encoding="utf-8", errors="replace")
    commented = commented_out(text)
    if commented:
        text = uncomment(text)
    code = blank(text)
    spans = classes(code)
    known = annotation_names(text)
    rows = []
    for m in FUN.finditer(code):
        name = text[m.start(1):m.end(1)].strip("`")
        block = annotation_block(code, text, m.start())
        tags = {known[a] for a in ANNOTATION.findall(blank(block)) if a in known}
        if not tags & {"Test", "ParameterizedTest"}:
            continue
        cls = enclosing_class(spans, m.start())
        class_disabled = any(
            re.search(r"@(Disabled|Ignore)\b[^\n]*\n(?:\s*@[^\n]*\n)*\s*(?:\w+\s+)*class\s+" + re.escape(c) + r"\b",
                      code)
            for c in cls.split(".") if c
        )
        if commented:
            kind = "commented"
        elif tags & {"Disabled", "Ignore"} or class_disabled:
            kind = "disabled"
        elif "ParameterizedTest" in tags:
            kind = "parameterized"
        else:
            kind = "test"
        base = [source_set, rel, cls, name, kind]
        if "ParameterizedTest" in tags:
            values = invocations(block, name, code, text, enums)
        wp, reason = wp_for(root, rel, name)
        if "ParameterizedTest" in tags:
            values = invocations(block, name, code, text, enums)
            if not values:
                rows.append(base + ["unknown", wp, reason])
            else:
                rows += [base + [label(i, v), wp, reason] for i, v in enumerate(values, 1)]
        else:
            rows.append(base + ["", wp, reason])
    return rows


def generate(root: pathlib.Path, existing):
    enums = load_enums(root)
    files = sorted(
        str(p.relative_to(root))
        for p in root.rglob("*.kt")
        if SOURCE_SET.match(str(p.relative_to(root))) and "/build/" not in str(p)
    )
    rows = []
    for rel in files:
        rows += scan_file(root, rel, enums)
    rows += [list(r) + [""] * (8 - len(r)) for r in EXTERNAL]
    out = []
    for *r, reason in rows:
        key = tuple(r[:6])
        old = existing.get(key)
        tail = [old["status"], old["rust_test"], old["commit"]] if old else ["pending", "", ""]
        if (r[6] == "n/a" or (r[0] == "external" and reason)) and tail[0] == "pending":
            # Tests of code that is not ported (wp n/a, or an external row with a reason) are n/a, with
            # the reason.
            tail = ["n/a", reason, ""]
        # WP 5.3: AwaitRemovalTest's three source names are the Rust test names.
        if r[6] == "5.3" and r[2] == "AwaitRemovalTest":
            name = re.sub(r"[^a-zA-Z0-9]+", "_", r[3]).strip("_").lower()
            tail = ["ported", f"crates/marathon-ios/tests/await_removal.rs::{name}", "2981543c"]
        # WP 5.4: only vendor-local rows remain after the approved integration split.
        if r[6] == "5.4":
            suite = {"DeviceTrackerTest": "device_tracker", "SimulatorFactoryTest": "simulator_factory",
                     "SimulatorProvisionerTest": "simulator_provisioner", "WorkerProvisioningContextTest": "worker_provisioning_context"}[r[2]]
            name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", r[3])
            name = re.sub(r"[^a-zA-Z0-9]+", "_", name).strip("_").lower()
            tail = ["ported", f"crates/marathon-ios/tests/{suite}.rs::{name}", "f2b7acd"]
        # WP 5.5a: parameter cases share a Rust method that executes every source invocation.
        if r[6] == "5.5a":
            suite = "simulator_provider_updates" if r[2] == "AppleSimulatorProviderUpdateTest" else "simulator_provider"
            name = re.sub(r"\s+\[\d+\]$", "", r[3])
            name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", name)
            name = re.sub(r"[^a-zA-Z0-9]+", "_", name).strip("_").lower()
            tail = ["ported", f"crates/marathon-ios/tests/{suite}.rs::{name}", "7a013fd4"]
        # WP 5.5b: all 37 original methods, including the two real startup paths.
        if r[6] == "5.5b":
            suite = {"StartupPhasesTest": "simulator_admission",
                     "AppleSimulatorProviderReconnectTest": "simulator_recovery",
                     "AppleSimulatorProviderWorkerRetryTest": "simulator_worker_retry"}[r[2]]
            name = re.sub(r"[^a-zA-Z0-9]+", "_", r[3]).strip("_").lower()
            tail = ["ported", f"crates/marathon-ios/tests/{suite}.rs::{name}", "8c77b1c3"]
        # WP 3.5: derive reviewed suite mappings, never hand-edit generated CSVs.
        if r[6] == "3.5" and r[2] in ("PoolProgressAccumulatorTest", "TestRunResultsAccumulatorTest"):
            name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", r[3])
            name = re.sub(r"[^a-zA-Z0-9]+", "_", name).strip("_").lower()
            suite = "pool_progress" if r[2] == "PoolProgressAccumulatorTest" else "run_results"
            tail = ["ported", f"crates/marathon-core/tests/{suite}.rs::{name}", tail[2]]
        if r[6] == "3.8":
            suites = {
                "DevicePoolActorSchedulingTest": "pool_scheduling",
                "DevicePoolActorBatchAccountingTest": "pool_batch_accounting",
                "DevicePoolActorSchedulingCharacterizationTest": "pool_scheduling_characterization",
            }
            if r[2] in suites:
                name = re.sub(r"[^a-zA-Z0-9]+", "_", r[3]).strip("_").lower()
                tail = ["ported", f"crates/marathon-core/tests/{suites[r[2]]}.rs::{name}", tail[2]]
        # WP 3.10: the run-flow suites, under each Kotlin test's name in snake_case.
        if r[6] == "3.10":
            if r[2] == "StartupTimingsTest":
                suite = "startup_timings"
            elif r[2] == "MarathonTestParseCommandTest":
                suite = "parse_command"
            else:
                suite = "marathon_scenarios"
            name = re.sub(r"[^a-zA-Z0-9]+", "_", r[3]).strip("_").lower()
            tail = ["ported", f"crates/marathon-core/tests/{suite}.rs::{name}", _RUN_FLOW_310_IMPLEMENTATION[suite]]
        # WP 3.4b retains the abstract rows; each shared Rust case runs all three backends.
        if r[6] == "3.4b":
            name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", r[3])
            name = re.sub(r"[^a-zA-Z0-9]+", "_", name).strip("_").lower()
            suite = "clients" if r[2] == "BasicGraphiteClientTest" else "history"
            namespace = {"InfluxDbProviderIntegrationTest": "influx::", "InfluxDb2ProviderIntegrationTest": "influx2::"}.get(r[2], "")
            tail = ["ported", f"crates/marathon-analytics/tests/{suite}.rs::{namespace}{name}", tail[2]]
        if r[6] == "3.9c" and r[2] == "RotationTest":
            name = {"testValue": "test_value", "testOf": "test_of"}[r[3]]
            tail = ["ported", f"crates/marathon-core/tests/screenshot.rs::{name}", "d020019"]
        if r[0] == "external" and r[1] == "adam@0.5.8" and r[6] == "6.2":
            tail = ["n/a", reason, ""] if r[2] in _ADAM_62_UNUSED else adam_62_port(r[2], r[3])
        if r[6] == "4.7":
            name = re.sub(r"([A-Z]+)([A-Z][a-z])", r"\1_\2", r[3])
            name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", name)
            name = re.sub(r"[^a-zA-Z0-9]+", "_", name).strip("_").lower()
            if r[2] == "TestResultsListenerTest" and r[5]:
                version = re.search(r'"([^" ]+)"', r[5]).group(1).replace(".", "_")
                name += "_" + version
            suite = "apple_logparser" if r[2] in ("DeviceFailureParserTest", "TestRunProgressParserTest") else "apple_listeners"
            tail = ["ported", f"crates/marathon-apple/tests/{suite}.rs::{name}", tail[2]]
        if r[6] == "4.9":
            name = re.sub(r"[^a-zA-Z0-9]+", "_", r[3]).strip("_").lower()
            suite = "tests/artifact_listener.rs" if r[2] == "ResultBundleRunListenerTest" else "src/artifact_processor/tests.rs"
            tail = ["ported", f"crates/marathon-apple/{suite}::{name}", "b40dd4c"]
        # WP 6.5 decisions: all assigned tests; seven unused adam options omitted.
        if r[6] == "6.5":
            unused = r[2] == "DiscoverAdbSocketInteractorTest" or (
                r[2] == "AsyncLogcatRequestTest" and r[3] not in {"testModeArguments", "testContinuous"})
            if unused:
                tail = ["n/a", "marathon never uses this option", ""]
            else:
                suites = {"AndroidAppInstallerTest": "installer_test",
                          "BaseAndroidDeviceTest": "device_test", "RemoteFileManagerTest": "device_test",
                          "VideoConfigurationTest": "device_test", "AdbBinaryIntegrationTest": "provider_test"}
                name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", r[3])
                name = re.sub(r"[^a-zA-Z0-9]+", "_", name).strip("_").lower()
                suite = suites.get(r[2], "models_test")
                tail = ["ported", f"crates/marathon-android/tests/{suite}.rs::{name}", tail[2]]
        if r[6] == "6.6a":
            namespace = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", r[2]).lower()
            name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", r[3]).lower()
            crate, suite = ("marathon-adb", "instrument_request") if r[0] == "external" else ("marathon-android", "android_listeners")
            tail = ["ported", f"crates/{crate}/tests/{suite}.rs::{namespace}::{name}", "4c1b346c"]
        if r[6] == "6.6b":
            namespace = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", r[2]).lower()
            name = re.sub(r"([a-z0-9])([A-Z])", r"\1_\2", r[3]).lower()
            tail = ["ported", f"crates/marathon-android/tests/android_artifacts.rs::{namespace}::{name}", tail[2]]
        out.append(r + tail)
    return out


def read_existing():
    if not OUT.exists():
        return {}
    with OUT.open(newline="", encoding="utf-8") as f:
        return {tuple(r[k] for k in KEY): r for r in csv.DictReader(f)}


def render(rows) -> str:
    buf = io.StringIO(newline="")
    w = csv.writer(buf)
    w.writerow(COLUMNS)
    w.writerows(rows)
    return buf.getvalue()


def main() -> int:
    args = sys.argv[1:]
    check = "--check" in args
    args = [a for a in args if a != "--check"]
    # An empty argument is the current directory to pathlib, and is what an unset variable expands to.
    if (
        len(args) != 1
        or not args[0]
        or args[0].startswith("-")
        or not (pathlib.Path(args[0]) / "build.gradle.kts").is_file()
        or not (pathlib.Path(args[0]) / "vendor").is_dir()
    ):
        # A wrong argument must not reach generate: it would find no tests, and the file would lose their rows.
        print(__doc__, file=sys.stderr)
        return 2
    existing = read_existing()
    rows = generate(pathlib.Path(args[0]).resolve(), existing)
    text = render(rows)
    status = 0
    bad = [r for r in rows if r[7] == "n/a" and not r[8].strip()]
    for r in bad:
        print(f"n/a without a reason: {r[1]} {r[2]}.{r[3]} {r[5]}", file=sys.stderr)
        status = 1
    for r in rows:
        if not r[6]:
            print(f"no wp: {r[1]} {r[2]}.{r[3]} {r[5]}", file=sys.stderr)
            status = 1
    dropped = set(existing) - {tuple(r[:6]) for r in rows}
    for key in sorted(dropped):
        if existing[key]["status"] != "pending":
            print(f"row no longer found, was {existing[key]['status']}: {' '.join(key)}", file=sys.stderr)
    if check:
        current = OUT.read_bytes().decode("utf-8") if OUT.exists() else ""
        if current != text:
            print(f"{OUT} is out of date; rerun without --check", file=sys.stderr)
            return 1
        print(f"parity matrix up to date ({len(rows)} rows)")
    else:
        OUT.write_bytes(text.encode("utf-8"))
        print(f"{len(rows)} rows -> {OUT}")
    return status


if __name__ == "__main__":
    sys.exit(main())
