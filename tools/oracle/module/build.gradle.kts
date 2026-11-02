// The oracle subproject of the Kotlin marathon clone (tools/oracle/build/marathon-src). Never shipped.
// tools/oracle/patches/0001-oracle-module.patch includes it from the clone's settings.gradle.kts; the sources stay here, in
// tools/oracle/module, so that they are reviewed as ordinary files.
plugins {
    application
    id("org.jetbrains.kotlin.jvm")
}

// Build outputs stay inside the disposable clone, never in the marathon-rs tree.
layout.buildDirectory.set(rootProject.layout.projectDirectory.dir("oracle-build"))

application {
    mainClass.set("oracle.MainKt")
    applicationName = "oracle"
}

dependencies {
    testImplementation(TestLibraries.junit5)
    implementation(project(":cli"))
    implementation(project(":core"))
    implementation(project(":vendor:vendor-test"))
    implementation(Libraries.kotlinCoroutines)
    implementation(Libraries.ktorNetwork)
    implementation(TestLibraries.coroutinesTest)
    implementation(TestLibraries.jsonAssert)
    implementation(project(":configuration"))
    implementation(project(":vendor:vendor-android"))
    implementation(project(":vendor:vendor-apple:base"))
    implementation(project(":vendor:vendor-apple:ios"))
    implementation(project(":vendor:vendor-apple:macos"))
    implementation(Libraries.kotlinStdLib)
    implementation(Libraries.kotlinReflect)
    implementation(Libraries.gson)
    implementation(Libraries.logbackClassic)
    implementation(Libraries.apacheCommonsText)
    implementation(Libraries.jacksonDatabind)
    implementation(Libraries.jacksonKotlin)
    implementation(Libraries.jacksonYaml)
    implementation(Libraries.jacksonJSR310)
    // The versions the marathon distribution ships (cli/build/install/marathon/lib).
    implementation("org.apache.commons:commons-lang3:3.14.0")
    implementation("org.yaml:snakeyaml:2.2")
}

tasks.withType<org.jetbrains.kotlin.gradle.tasks.KotlinCompile> {
    kotlinOptions.jvmTarget = "17"
}
tasks.withType<JavaCompile> {
    sourceCompatibility = "17"
    targetCompatibility = "17"
}

if (rootProject.findProject(":oracle-hooks") != null) {
    dependencies { implementation(project(":oracle-hooks")) }
    kotlin.sourceSets["main"].kotlin.srcDir("src/hooks/kotlin")
}

setupTestTask(jacoco = false)
