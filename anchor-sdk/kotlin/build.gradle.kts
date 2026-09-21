plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.android)
}

version = "1.0.0"

android {
    namespace = "org.anchor.sdk"
    compileSdk { version = release(36) }
    defaultConfig {
        minSdk = 30
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }
    kotlinOptions { jvmTarget = "11" }
}

// This module is intentionally a standalone Gradle build as well as the
// `:anchorSdk` project included by the Anchor Android app. That lets external
// users clone Anchor recursively and build/test the SDK without the product
// application or unpublished package registries.

val protocolRoot = layout.projectDirectory.dir("../protocol")
val generatedProtocolDirectory = layout.buildDirectory.dir("generated/source/anchorProtocol/main/java")
val protocolFiles = fileTree(protocolRoot) { include("anchor/v1/**/*.proto") }

val generateAnchorProtocol by tasks.registering(Exec::class) {
    inputs.files(protocolFiles)
    outputs.dir(generatedProtocolDirectory)
    doFirst {
        generatedProtocolDirectory.get().asFile.mkdirs()
        commandLine(
            "protoc", "-I", protocolRoot.asFile.absolutePath,
            "--java_out=lite:" + generatedProtocolDirectory.get().asFile.absolutePath,
            *protocolFiles.files.sorted().map { it.absolutePath }.toTypedArray(),
        )
    }
}

android.sourceSets.getByName("main").java.srcDir(generatedProtocolDirectory)
tasks.configureEach {
    if (name.startsWith("compile") || name.startsWith("test")) dependsOn(generateAnchorProtocol)
}

dependencies {
    implementation(libs.kotlinx.coroutines.android)
    // Must match the Java generation version emitted by the pinned protoc
    // toolchain. The build will pin/provision protoc before this SDK is used in
    // CI; do not silently mix a host compiler with an older runtime.
    // Generated capability messages are part of the public SDK surface, so
    // consumers compiling against ClipboardProtocol also need the lite
    // runtime on their classpath.
    api("com.google.protobuf:protobuf-javalite:4.36.2")
    implementation("org.bouncycastle:bcprov-jdk18on:1.79")
    implementation("org.bouncycastle:bcpkix-jdk18on:1.79")
    testImplementation(libs.junit)
    androidTestImplementation(libs.androidx.junit)
    androidTestImplementation("androidx.test:runner:1.7.0")
}
