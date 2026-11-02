// The OracleHooks service of the patched Kotlin marathon clone (tools/oracle/build/marathon-patched). Never shipped.
// tools/oracle/patches/0002-oracle-hooks.patch includes it from the clone's settings.gradle.kts and makes the configuration
// and usage-analytics modules depend on it; the sources stay here, in tools/oracle/hooks, so that they are reviewed as
// ordinary files. docs/design/replay-tapes.md specifies the tapes it reads.
plugins {
    `java-library`
    id("org.jetbrains.kotlin.jvm")
}

// Build outputs stay inside the disposable clone, never in the marathon-rs tree.
layout.buildDirectory.set(rootProject.layout.projectDirectory.dir("oracle-hooks-build"))

dependencies {
    implementation(Libraries.kotlinStdLib)
    implementation(Libraries.kotlinReflect)
    implementation(Libraries.kotlinCoroutines)
    implementation(Libraries.gson)
    implementation(Libraries.logbackClassic)
    testImplementation(TestLibraries.junit5)
    testImplementation(TestLibraries.coroutinesTest)
}

setupKotlinCompiler()

setupTestTask(jacoco = false)

tasks.withType<Test> {
    systemProperty("oracle.test.classpath", sourceSets["test"].runtimeClasspath.asPath)
}
