package oracle

import java.io.File
import java.util.regex.PatternSyntaxException

/**
 * The Java regex corpus of docs/research/regex.md §4 (families 1–11), through Kotlin's `Regex` (java.util.regex, default
 * flags), as marathon calls it (`oracle regex-corpus <file>`).
 *
 * The watchdog counts the character reads of the matcher instead of timing it, so that adversarial rows are deterministic: a
 * run that reads more than [READ_LIMIT] characters is aborted and recorded as `{"aborted": reads}`.
 */
object RegexCorpus {
    private const val READ_LIMIT = 20_000_000L

    private class Aborted : RuntimeException(null, null, false, false)

    /** A CharSequence that aborts the match after [READ_LIMIT] reads. */
    private class Watched(private val s: String, private val counter: LongArray) : CharSequence {
        override val length: Int get() = s.length
        override fun get(index: Int): Char {
            if (++counter[0] > READ_LIMIT) throw Aborted()
            return s[index]
        }
        override fun subSequence(startIndex: Int, endIndex: Int): CharSequence = s.subSequence(startIndex, endIndex)
        override fun toString() = s
    }

    private class Case(
        val id: String, val pattern: String, val op: String, val inputs: List<String>, val replacement: String? = null,
        val witness: List<Boolean>? = null, val limit: Int? = null,
    )

    private val terminators = listOf("\n", "\r", "\r\n", "\u0085", "\u2028", "\u2029")

    fun generate(out: File) {
        val table = JsonlTable(
            out.nameWithoutExtension,
            "Java regex corpus for spike 0.3c (docs/research/regex.md §4): Kotlin Regex(pattern) with default flags. matches = " +
                "Regex.matches (full match), find = Regex.find, findAll = Regex.findAll, replace = Regex.replace(input, " +
                "replacement) (Java replacement syntax), split = Regex.split(input) or, when the row has a limit, Regex.split(input, " +
                "limit) (Kotlin: trailing empty strings are kept).",
            linkedMapOf(
                "id" to "<family>-<nnn>",
                "family" to "ascii-classes | unicode-classes | flags | flag-scope | anchors | dot | quoting | advanced | hardcoded | split | user-realistic | adversarial | invalid | round1 | round2 | runtime-scans",
                "pattern" to "the pattern",
                "op" to "matches | find | findAll | replace | split",
                "replacement" to "replace only",
                "limit" to "split only, when present: the limit argument of Regex.split (> 0)",
                "inputs" to "input strings",
                "expected" to "one entry per input: matches -> boolean; find -> null or {range: [start, endExclusive], groups: [...]} " +
                    "(null for a group that did not participate); findAll -> list of those; replace -> string; split -> list; " +
                    "{aborted: reads} when the watchdog stopped the matcher; {error: {class, message}} for runtime errors",
                "compile_error" to "null, or {class, description, index, message} of the PatternSyntaxException",
                "scalar_expected" to "supplementary inputs, non-matches operations only: JDK results with scalar-only boundaries " +
                    "and scalar empty-match advancement (E1), preserving saved \\G end in split/replace. Never a parity pass",
                "witness" to "flag-scope rows only: per input, whether it must match. A positive witness matches only when " +
                    "the flag is restored after an inner scope; a negative one matches only when a flag leaks out of its " +
                    "scope. The generator checks the JDK agrees, so expected always equals witness",
            ),
        )
        val cases = mutableListOf<Case>()
        val counters = mutableMapOf<String, Int>()
        fun add(family: String, pattern: String, op: String, inputs: List<String>, replacement: String? = null, limit: Int? = null) {
            val n = counters.merge(family, 1, Int::plus)!!
            cases += Case("$family-%03d".format(n), pattern, op, inputs, replacement, limit = limit)
        }
        fun scope(pattern: String, positive: List<String>, negative: List<String>) {
            val n = counters.merge("flag-scope", 1, Int::plus)!!
            cases += Case(
                "flag-scope-%03d".format(n), pattern, "matches", positive + negative, null,
                positive.map { true } + negative.map { false },
            )
        }

        // 1. ASCII classes against non-ASCII input.
        val mixed = listOf(
            "", "a", "Z", "0", "9", "_", " ", "\t", "\n", "\u000b", "\u000c", "\r", "\u00a0", "\u2007", "\u3000", "é", "ж", "Ж", "\u0661",
            "\uff11", "ſ", "\u212a", "ß", "\uD83D\uDE00", "a1_", "a-b", "x y",
        )
        for (p in listOf("\\d", "\\s", "\\w", "\\D", "\\S", "\\W", "\\d+", "\\w+", "\\s*")) add("ascii-classes", p, "matches", mixed)
        for (p in listOf("\\b", "\\B", "\\bfoo\\b", "\\w+\\b")) add("ascii-classes", p, "findAll", listOf("foo", "foo bar", "éfooé", "жfoo", "foo_", "", "a.b"))

        // 2. Unicode and POSIX classes.
        for (p in listOf(
            "\\p{L}", "\\p{Lu}", "\\p{Nd}", "\\p{Graph}", "\\p{Print}", "\\p{Punct}", "\\p{InGreek}", "\\p{IsLatin}", "\\p{IsAlphabetic}",
            "\\p{javaLowerCase}", "\\p{javaUpperCase}", "\\p{javaWhitespace}", "\\p{javaMirrored}", "\\p{Alpha}", "\\p{Alnum}", "\\p{Space}",
            "\\p{XDigit}", "\\p{Cntrl}", "\\p{ASCII}", "\\P{L}", "[[:alpha:]]", "\\h", "\\v", "\\R", "\\X", "(?U)\\w", "(?U)\\d", "(?U)\\s",
            "(?U)\\p{Alpha}", "\\p{IsWhite_Space}",
        )) add("unicode-classes", p, "matches", mixed + listOf("α", "Ω", "(", "[", "!", "\u0085", "\r\n", "e\u0301"))

        // 3. Flags.
        val caseInputs = listOf("STRASSE", "straße", "Straße", "STRAßE", "abc", "ABC", "AbC", "ж", "Ж", "ǅ", "ǆ", "Ǆ", "k", "K", "\u212a", "ſ", "s", "S", "ı", "I", "i", "İ")
        for (p in listOf("(?i)straße", "(?i)abc", "(?i)ж", "(?iu)ж", "(?i)ǆ", "(?iu)ǆ", "(?i)k", "(?iu)k", "(?i)s", "(?iu)s", "(?i)i", "(?iu)i", "(?U)(?i)ж", "(?i:a)bc", "(?i)a(?-i)bc", "(?i)a(?-i:b)c", "(?u)ж"))
            add("flags", p, "matches", caseInputs)
        for (p in listOf("(?m)^a$", "(?m)^", "(?m)$", "(?s)a.b", "a.b", "(?d)a.b", "(?d)(?m)^a$", "(?x) a b # comment", "(?x)a\\ b", "(?x)[ ]a", "(?ms)^a.b$"))
            add("flags", p, "findAll", listOf("a", "a\nb", "a\rb", "a\r\nb", "a\u0085b", "a\u2028b", "ab", "a b", " a", "b\na\n", "a\n"))
        // The flag matrix: for each of i d m s u x U and a body that the flag changes, the body without the flag, the flag
        // global, scoped, negated after a global flag (globally and scoped), negated alone, and nested (the flag restored
        // after an inner negated group). `u` only changes case folding, so its rows run under (?i).
        val flagInputs = listOf(
            "ab", "AB", "aB", "Ab", "a b", "a#c\nb", "acb", "k", "K", "\u212a", "\u0436", "\u0416", "s", "S", "\u017f", "\u01c5", "\u01c6", "a", "a\n", "a\r",
            "a\r\n", "a\u0085", "a\nb", "a\rb", "a\u2028b", "b\na", "x1_", "\u00e9", "\u00c9", "\u0661", "\u00a0", " ", "\t",
        )
        val matrix = listOf(
            Triple('i', "", listOf("ab", "k", "\u0436", "\u01c6")),
            Triple('d', "", listOf("a.b", "a$", "(?m)^b")),
            Triple('m', "", listOf("^a$", "^b")),
            Triple('s', "", listOf("a.b", ".")),
            Triple('u', "(?i)", listOf("\u0436", "k", "\u017f", "\u00e9", "\u01c6")),
            Triple('x', "", listOf("a b", "a#c\nb", "[ ]a")),
            Triple('U', "", listOf("\\w", "\\d", "\\s", "\\b\u00e9", "\\p{Alpha}", "(?i)k")),
        )
        for ((f, context, bodies) in matrix) for (b in bodies) {
            listOf(
                b, "(?$f)$b", "(?$f:$b)", "(?$f)(?-$f)$b", "(?$f)(?-$f:$b)", "(?-$f)$b",
            ).forEach { add("flags", context + it, "findAll", flagInputs) }
        }

        // Flag scopes: each pattern enables or disables a flag in a group and relies on the enclosing setting after it. The
        // positive witnesses need the setting restored after the group; the negative ones would match only if it leaked.
        scope("(?i:a(?-i:b)c)", listOf("abc", "AbC", "abC"), listOf("aBc", "ABC"))
        scope("a(?i:b)c", listOf("abc", "aBc"), listOf("aBC", "Abc"))
        scope("(?i)a(?-i:b)c", listOf("AbC", "abc"), listOf("ABC", "aBc"))
        scope("(?-i:a(?i:b)c)", listOf("aBc"), listOf("aBC", "AbC"))
        scope("(?d:a.)b.", listOf("a\rbx", "axbx"), listOf("a\rb\r", "a\nbx"))
        scope("(?d)a(?-d:.)b.", listOf("axb\r", "axbx"), listOf("a\rb\r", "a\rbx"))
        scope("(?m:a$)\nb", listOf("a\nb"), listOf("ab"))
        scope("(?m:a$)\n^b", emptyList(), listOf("a\nb"))
        scope("(?m)(?-m:a)$\n^b", listOf("a\nb"), emptyList())
        scope("(?m)(?-m:a$)\n^b", emptyList(), listOf("a\nb"))
        scope("(?s:a.)b.", listOf("a\nbx"), listOf("a\nb\n"))
        scope("(?s)a(?-s:.)b.", listOf("axb\n"), listOf("a\nb\n", "a\nbx"))
        scope("(?i)(?u:ж)ж", listOf("Жж", "жж"), listOf("ЖЖ", "жЖ"))
        scope("(?iu)(?-u:ж)ж", listOf("жЖ", "жж"), listOf("Жж", "ЖЖ"))
        scope("(?x:a b)a b", listOf("aba b"), listOf("abab", "a ba b"))
        scope("(?x)a(?-x: b)c", listOf("a bc"), listOf("abc", "a b c"))
        scope("(?U:\\w)\\w", listOf("жa", "aa"), listOf("aж", "жж"))
        scope("(?U)\\w(?-U:\\w)\\w", listOf("жaж", "aaa"), listOf("жжж", "aжa"))

        // 4. Anchors.
        val anchorInputs = listOf("", "a", "ab") + terminators.flatMap { listOf("a$it", "${it}a", "a${it}b", "a$it$it", it) }
        for (p in listOf("^a$", "a$", "^a", "\\Aa\\z", "\\Aa\\Z", "a\\Z", "a\\z", "^$", "$", "\\G", "\\Ga", "(?m)a$", "(?m)^b", "a$\\n?", "\\b"))
            for (op in listOf("matches", "findAll")) add("anchors", p, op, anchorInputs)

        // 5. Dot.
        for (p in listOf(".", "(?s).", "(?s:.)", "a.b", "(?d).", ".*", "(?s).*", "[^a]", "[.]"))
            add("dot", p, "matches", listOf("", "a", "x") + terminators + listOf("\u0000", "\uD83D\uDE00"))

        // 6. Quoting.
        for (p in listOf("\\Qa.b\\E", "\\Qa.b", "a\\E", "\\Q\\E", "\\Q.*\\E+", "\\Q[\\E", "x\\Q\\Ey", "\\Q\\\\E", "[\\Q]\\E]", "\\Qa\\Eb\\Qc\\E"))
            add("quoting", p, "matches", listOf("a.b", "axb", "a", "aE", "", ".*", ".*.*", "[", "xy", "\\", "]", "abc", "\\E", "a\\E"))

        // 7. Advanced constructs.
        add("advanced", "^((?!Abstract).)*Test[s]*$", "matches", listOf(
            "FooTest", "FooTests", "FooTestss", "AbstractFooTest", "FooAbstractTest", "Foo", "Test", "", "abstractTest", "FooTest\n", "MyTestCase",
            "UITests", "Test\u2028s",
        ))
        for (p in listOf("a++b", "a*+a", "(?>a+)b", "(?>a|ab)c", "(a)\\1", "(a)?\\1", "(?<n>a)\\k<n>", "(a)|\\1b", "(?=a)a", "(?!a).", "(?<=a)b",
            "(?<!a)b", "(?<=a+)b", "(?<=a{1,3})b", "[a-z&&[^aeiou]]+", "[a-c[x-z]]+", "a{2}", "a{2,}", "a{,2}", "a{2,3}?", "x{0}", "(a|ab)(c|bcd)(d*)",
            "(?:a|b)+", "(a*)*", "(a*)+b", "(a|)+", "\\x41", "\\u0041", "\\x{1F600}", "\\0101", "\\cA", "\\e", "\\N{LATIN SMALL LETTER A}",
            "[\\w&&\\D]", "(?<a1>x)(?<b>y)?", "(?i)(a)\\1", "\\p{javaJavaIdentifierStart}", "a\\b{g}b", "\\uD800", "(?c)a"))
            add("advanced", p, "findAll", listOf("aab", "ab", "aa", "abcd", "abcbcd", "b", "ba", "xyz", "", "aaab", "\u0001", "\u001b", "A", "😀", "x", "xy"))

        // 8. Hard-coded patterns of the Kotlin marathon, with fixture-like lines.
        val xcodebuild = listOf(
            "Test Case '-[RunnerUITests ab.example_test+tty]' started.",
            "Test Case '-[RunnerUITests ab.example_test+tty]' passed (0.206 seconds).",
            "Test Case '-[RunnerUITests androidAppTest___tapsAround]' failed (12.000 seconds).",
            "Test Case '-[sample_appUITests.MoreTests testPresentModal]' skipped (0.000 seconds).",
            "Test Case '-[Suite test]' exceeded execution time allowance of 60 seconds. The test may have hung.",
            "/Users/user/git/apps/sample_appUITests/Target/TestCases/MoreTests.swift:33: error: -[sample_appUITests.MoreTests testPresentModal] : XCTAssertTrue failed - This test failed.",
            "<unknown>:0: error: -[sample_appUITests.MoreTests testPresentModal] : XCTAssertTrue failed - with details {(",
            "Test Case '-[A b c]' passed (1.2.3 seconds).",
            "Test Suite 'All tests' started at 2024-02-07 13:57:27.123",
            "",
        )
        val testCaseFinished = """Test Case '-\[(.+) (.+)]' (passed|failed|skipped) \(([\d\.]+) seconds\)\."""
        val testCaseStarted = """Test Case '-\[(.+) (.+)]' started\."""
        val failing = "(/.+|<unknown>:\\d+):\\serror:\\s[\\+\\-]\\[(.*)\\s(.*)\\]\\s:(\\s.*)"
        val timeout = """Test Case '-\[(.+) (.+)]' exceeded execution time allowance of (.+)\. The test may have hung.*"""
        for (p in listOf(testCaseStarted, testCaseFinished, failing, timeout)) for (op in listOf("matches", "find")) add("hardcoded", p, op, xcodebuild)
        val resultPaths = listOf(
            "\t/Users/xxx/Library/Developer/Xcode/DerivedData/temporary-asdgsga/Logs/Test/Test-Transient Testing-2024.02.07_13-57-27-+0000.xcresult",
            "        /var/folders/7v/T/E9A463C5/DerivedData/temporary/Logs/Test/Test-Transient Testing-2023.10.13_13-51-03-+1000.xcresult",
            "/tmp/x.xcresult  ", "Writing diagnostic log for test session to: /tmp/a/Session.log", "/tmp/a/b.log", "no path here.xcresult", "x /a.log\n",
        )
        for (p in listOf("(^\\s*|\\s+)/.+\\.xcresult\\s*$", "(^\\s*|\\s+)/.+\\.log\\s*$")) add("hardcoded", p, "find", resultPaths)
        add("hardcoded", "[^a-zA-Z0-9\\.\\#]", "replace", listOf("com.example.Foo#bar", "emulator-5554", "127.0.0.1:5555", "a b/c", "ж", "😀", ""), "-")
        add("hardcoded", "\\s+#.*$", "replace", listOf("com.example.Foo # comment", "com.example.Foo#bar", "a\t# x", "a #x\n", "# only"), "")
        add("hardcoded", "\\s", "replace", listOf("sumSeries(a.b, c)", " a \t b\n", "\u00a0"), "")
        add("hardcoded", "/[A-Za-z0-9_./@+-]+", "matches", listOf("/tmp/marathon/x.xctestrun", "/a b", "relative/path", "/tmp/ж", "/", "/a@b+c-d_e.f"))
        add("hardcoded", "(?s)\n.*\\z", "replace", listOf("rsync  version 3.2.7  protocol version 31\nCopyright", "single line", "a\r\nb", "\n"), "")
        add("hardcoded", "[A-Z0-9\\-]*", "matches", listOf("E9A463C5-EA19-4D07-9BE2-B21A150940C3", "", "abc", "ABC 1", "  E9A4"))
        add("hardcoded", "[- ]", "replace", listOf("My App-Name", "a--b  c", ""), "_")
        add("hardcoded", "\\p{Graph}+\\([^\\)]*\\)", "findAll", listOf("Metric(a=1) Other(b) bare", "x()", "a(b(c))", "ж(x)", ""))
        add("hardcoded", ".+\\[\\d+]", "matches", listOf("test[0]", "test[12]", "test[]", "[1]", "test[1]x", "test[١]"))
        val logcatHeader = "^\\[ (\\d\\d-\\d\\d \\d\\d:\\d\\d:\\d\\d\\.\\d\\d\\d) +(\\d+): *(\\w+) ([VDIWEAF])/(.*?) +]$"
        add("hardcoded", logcatHeader, "find", listOf(
            "[ 08-18 16:39:11.760  2977: 2988 D/PhoneInterfaceManager ]", "[ 08-18 16:39:11.760  2977:0x1a2b I/Tag With Spaces  ]",
            "[ 08-18 16:39:11.760  2977: 2988 X/Tag ]", "[ 08-18 16:39:11.760 2977:2988 D/ ]", "not a header",
        ))

        // nm symbol filters of NmTestParser (bin/nm/Nm.kt). They run as grep patterns on the worker, so these rows pin the Java
        // reading of the same text, plus the default testClassRegexes over the class names nm yields. The lines are real `nm`
        // output (swift-demangle'd for Swift) of a small XCTest bundle built with this Mac's Xcode.
        val nmSwift = listOf(
            "0000000000000e40 t SampleUITests.LoginTests.testLogin() -> ()",
            "0000000000000e54 t @objc SampleUITests.LoginTests.testLogin() -> ()",
            "0000000000000ea0 t SampleUITests.LoginTests.testLogout_DISABLED() -> ()",
            "0000000000000f00 t SampleUITests.LoginTests.testWith(argument: Swift.Int) -> ()",
            "0000000000000f70 t SampleUITests.LoginTests.helper() -> ()",
            "00000000000012d0 t SampleUITests.AbstractBaseTest.testInherited() -> ()",
            "0000000000001950 s method descriptor for SampleUITests.AbstractBaseTest.testInherited() -> ()",
        )
        val nmObjc = listOf(
            "000000000000072c t -[SettingsTests setUp]",
            "0000000000000718 t -[SettingsTests testToggle]",
            "0000000000000740 t -[SettingsTests test_underscore]",
            "00000000000080d0 S _OBJC_CLASS_\$_SettingsTests",
            "0000000000000758 s __OBJC_\$_INSTANCE_METHODS_SettingsTests",
        )
        add("hardcoded", "[\\.|_]test", "find", nmSwift.map { it.split(' ').getOrElse(2) { "" } } + nmSwift)
        add("hardcoded", " t ", "find", nmObjc + nmSwift)
        add("hardcoded", " test", "find", listOf("SettingsTests testToggle", "SettingsTests setUp", "SettingsTests test_underscore", "Tests testing"))
        add("hardcoded", "^((?!Abstract).)*Test[s]*$", "matches", listOf("LoginTests", "AbstractBaseTest", "SettingsTests", "SampleUITests", "Helper"))

        // split: Kotlin's Regex.split keeps trailing empty strings, unlike Java's String.split.
        add("split", ",", "split", listOf("a,b,,c,,", "", ",", "a"))
        add("split", "\\s*;\\s*", "split", listOf("x ; y;z ;", " ; "))
        // Regex.split(input, limit) with limit > 0 stops after limit - 1 matches, and the rest of the input is the last piece.
        // The zero-width pattern pins that Kotlin, unlike Java's Pattern.split, keeps a leading empty piece.
        add("split", ",", "split", listOf("a,b,,c,,", "", ",", "a", ",,,,", "a,b"), limit = 3)
        add("split", "x*", "split", listOf("abc", "", "xaxb"), limit = 3)

        // 9. Realistic user patterns (serials, filters, annotations).
        add("user-realistic", "emulator-.*", "matches", listOf("emulator-5554", "Emulator-5554", "127.0.0.1:5555", "emulator-"))
        add("user-realistic", "(?!emulator).*", "matches", listOf("emulator-5554", "R58M123", "127.0.0.1:5555"))
        add("user-realistic", "^(?:192\\.168\\.\\d+\\.\\d+:\\d+|[A-Z0-9]{8,})$", "matches", listOf("192.168.1.10:5555", "R58M12345", "r58m12345", "192.168.1.10"))
        add("user-realistic", ".*Test", "matches", listOf("FooTest", "FooTests", "Test", "com.example.FooTest"))
        add("user-realistic", "com\\.example\\..*", "matches", listOf("com.example.ui", "com.examplex", "com.example."))
        add("user-realistic", "test.*|should.*", "matches", listOf("testFoo", "shouldBar", "itWorks", ""))
        add("user-realistic", "io\\.qameta\\.allure\\.kotlin\\.(Severity|Owner)", "matches", listOf("io.qameta.allure.kotlin.Owner", "io.qameta.allure.kotlin.Epic"))
        add("user-realistic", "(?i)smoke|regression", "matches", listOf("Smoke", "REGRESSION", "smoke-test"))
        add("user-realistic", "[a-z]+_\\d{3,}", "matches", listOf("abc_123", "abc_12", "ABC_123"))

        // 10. Adversarial patterns under the watchdog.
        for (p in listOf("(a+)+$", "(a|a?)+$", "(\\w+)*!", "(a|aa)+$", "^(([a-z])+.)+[A-Z]([a-z])+$", "((?!Abstract).)*Test[s]*$", "(x+x+)+y", "(.*a){12}", "^(\\w+\\s?)*$", "(a|b|ab)*c", "(\\d+)*\\D", "^(a?){25}a{25}$")) {
            add("adversarial", p, "matches", listOf(5, 10, 15, 20, 25, 30, 40).map { "a".repeat(it) + "b" } + listOf("aaaa", "a".repeat(30) + "!"))
        }

        // 11. Invalid patterns.
        for (p in listOf("[", "(", ")", "a{2", "a{2,1}", "*a", "a**", "+", "?", "\\", "\\k<missing>", "(?<n>a)(?<n>b)", "[z-a]", "\\p{Bogus}",
            "(?<=a+b*)c", "\\1", "a\\E", "(?z)a", "[a-]", "[]a]", "{1}", "a{1}{2}", "(?<1a>x)", "\\x{110000}", "\\uZZZZ", "\\c", "\\i", "(?<=a*)b",
            "a{99999999999}", "[[:alpha:]"))
            add("invalid", p, "find", listOf("a", "ab", ""))

        // Review reproductions and all 1,084 exploratory probes. Inputs are checked in; expected results always
        // come from this same Kotlin Regex oracle, including Matcher continuation in split/replace.
        val reviewInputs = checkNotNull(RegexCorpus::class.java.getResourceAsStream("/regex-round1.jsonl"))
        reviewInputs.bufferedReader().useLines { lines -> lines.forEach { line ->
            val row = com.fasterxml.jackson.module.kotlin.jacksonObjectMapper().readTree(line)
            add("round1", row["pattern"].asText(), row["op"].asText(), row["inputs"].map { it.asText() },
                row["replacement"]?.asText(), row["limit"]?.asInt())
        } }
        // Large review reproductions are constructed here so the input-only resource stays reviewable.
        for (depth in listOf(100_000, 20_000)) {
            add("round1", "(".repeat(depth) + "a" + ")".repeat(depth), "matches", listOf("a"))
        }
        for (pattern in listOf("(?:(a)|b)*(?=)", "(?=)(a)*")) {
            add("round1", pattern, "matches", listOf("a".repeat(1_100_000)))
        }
        add("round1", ".*(?<=a+b*)c", "matches", listOf("abbbc"))
        val round2Inputs = checkNotNull(RegexCorpus::class.java.getResourceAsStream("/regex-round2.jsonl"))
        round2Inputs.bufferedReader().useLines { lines -> lines.forEach { line ->
            val row = com.fasterxml.jackson.module.kotlin.jacksonObjectMapper().readTree(line)
            add("round2", row["pattern"].asText(), row["op"].asText(), row["inputs"].map { it.asText() },
                row["replacement"]?.asText(), row["limit"]?.asInt())
        } }
        for (pattern in listOf("(?=a*\\z)b", "(?!(?:a*\\z))b")) {
            for (size in listOf(1024, 100_000, 1_000_000, 10_000_000)) {
                add("runtime-scans", pattern, "find", listOf("a".repeat(size)))
            }
        }
        for (c in cases) table.add(run(c))
        table.writeTo(out.parentFile.also { it.mkdirs() }).renameTo(out)
    }

    private fun run(c: Case): Map<String, Any?> {
        val row = linkedMapOf<String, Any?>(
            "id" to c.id, "family" to c.id.substringBeforeLast('-'), "pattern" to c.pattern, "op" to c.op,
        )
        if (c.replacement != null) row["replacement"] = c.replacement
        if (c.limit != null) row["limit"] = c.limit
        row["inputs"] = c.inputs
        val regex = try {
            Regex(c.pattern)
        } catch (e: PatternSyntaxException) {
            row["expected"] = null
            row["compile_error"] = linkedMapOf("class" to e.javaClass.name, "description" to e.description, "index" to e.index, "message" to e.message)
            return row
        }
        row["expected"] = c.inputs.map { input ->
            val counter = LongArray(1)
            val s = Watched(input, counter)
            try {
                when (c.op) {
                    "matches" -> regex.matches(s)
                    "find" -> regex.find(s)?.let { matchJson(it) }
                    "findAll" -> regex.findAll(s).map { matchJson(it) }.toList()
                    "replace" -> regex.replace(s, c.replacement!!)
                    "split" -> if (c.limit != null) regex.split(s, c.limit) else regex.split(s)
                    else -> throw IllegalArgumentException(c.op)
                }
            } catch (a: Aborted) {
                mapOf("aborted" to READ_LIMIT)
            } catch (t: Throwable) {
                mapOf("error" to linkedMapOf("class" to t.javaClass.name, "message" to t.message))
            }
        }
        if (c.op != "matches" && c.inputs.any { it.codePoints().anyMatch { cp -> cp > 0xffff } }) {
            row["scalar_expected"] = c.inputs.map { input -> scalarRun(c, input) }
        }
        row["compile_error"] = null
        if (c.witness != null) {
            check(row["expected"] == c.witness) { "${c.id} ${c.pattern}: the JDK gives ${row["expected"]}, the witnesses say ${c.witness}" }
            row["witness"] = c.witness
        }
        return row
    }

    /** E1 reference: use the JDK engine, but permit only scalar boundaries and scalar empty-match advancement. */
    private fun scalarRun(c: Case, input: String): Any? {
        fun midpoint(i: Int) = i > 0 && i < input.length &&
            Character.isSurrogatePair(input[i - 1], input[i])
        // find(start) resets Matcher state. Replace only real \G tokens with an absolute assertion at the saved end;
        // quoting and escaped backslashes must retain their original meaning. This adds no capturing groups.
        fun continuation(previousEnd: Int): String {
            val anchor = "(?<=\\A" + java.util.regex.Pattern.quote(input.substring(0, previousEnd)) + ")"
            val out = StringBuilder()
            var i = 0
            var quoted = false
            while (i < c.pattern.length) {
                val ch = c.pattern[i++]
                if (ch == '\\' && i < c.pattern.length) {
                    val next = c.pattern[i++]
                    if (!quoted && next == 'Q') quoted = true
                    else if (quoted && next == 'E') quoted = false
                    if (!quoted && next == 'G') out.append(anchor)
                    else out.append(ch).append(next)
                } else out.append(ch)
            }
            return out.toString()
        }
        fun search(from: Int, previousEnd: Int?): java.util.regex.Matcher? {
            val pattern = if (previousEnd == null) c.pattern else continuation(previousEnd)
            val matcher = java.util.regex.Pattern.compile(pattern).matcher(input)
            var index = from
            while (index <= input.length && matcher.find(index)) {
                if (!midpoint(matcher.start()) && !midpoint(matcher.end())) return matcher
                index = matcher.start() + 1
                if (midpoint(index)) index++
            }
            return null
        }
        fun matchJson(m: java.util.regex.Matcher): Map<String, Any?> = linkedMapOf(
            "range" to listOf(m.start(), m.end()), "groups" to (0..m.groupCount()).map { m.group(it) },
        )
        if (c.op == "find") return search(0, null)?.let { matchJson(it) }
        val matches = mutableListOf<Map<String, Any?>>()
        val pieces = mutableListOf<String>()
        val replacement = StringBuilder()
        var from = 0
        var last = 0
        if (c.op == "split" && c.limit == 1) return listOf(input)
        while (from <= input.length) {
            val matcher = search(from, if (c.op == "findAll") null else last) ?: break
            when (c.op) {
                "findAll" -> matches += matchJson(matcher)
                "split" -> pieces += input.substring(last, matcher.start())
                "replace" -> {
                    val expanded = StringBuffer()
                    matcher.appendReplacement(expanded, c.replacement!!)
                    replacement.append(input.substring(last, matcher.start()))
                    replacement.append(expanded.substring(matcher.start()))
                }
            }
            last = matcher.end()
            if (c.op == "split" && c.limit != null && c.limit > 0 && pieces.size == c.limit - 1) break
            from = if (matcher.start() == matcher.end()) {
                if (last == input.length) last + 1 else last + Character.charCount(input.codePointAt(last))
            } else last
        }
        return when (c.op) {
            "findAll" -> matches
            "split" -> pieces + input.substring(last)
            "replace" -> replacement.append(input.substring(last)).toString()
            else -> error(c.op)
        }
    }

    private fun matchJson(m: MatchResult): Map<String, Any?> = linkedMapOf(
        "range" to listOf(m.range.first, m.range.last + 1),
        "groups" to m.groups.map { it?.value },
    )
}
