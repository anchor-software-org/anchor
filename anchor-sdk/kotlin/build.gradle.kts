// Also select the catalog's Kotlin version when the SDK is built standalone.
buildscript {
    dependencies {
        classpath(libs.kotlin.gradle.plugin)
    }
}

plugins {
    alias(libs.plugins.android.library)
}

version = "1.0.0"

android {
    namespace = "org.anchor.sdk"
    compileSdk { version = release(37) }
    defaultConfig {
        minSdk = 30
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
        consumerProguardFiles("consumer-rules.pro")
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_11
        targetCompatibility = JavaVersion.VERSION_11
    }
}

// This module is intentionally a standalone Gradle build as well as the
// `:anchorSdk` project included by the Anchor Android app. That lets external
// users clone Anchor recursively and build/test the SDK without the product
// application or unpublished package registries.

val protocolRoot = layout.projectDirectory.dir("../protocol")
val protocolFiles = fileTree(protocolRoot) { include("anchor/v1/**/*.proto") }

abstract class GenerateAnchorProtocol : Exec() {
    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty
}

val generateAnchorProtocol = tasks.register<GenerateAnchorProtocol>("generateAnchorProtocol") {
    inputs.files(protocolFiles)
    outputDirectory.set(layout.buildDirectory.dir("generated/source/anchorProtocol/main/java"))
    doFirst {
        outputDirectory.get().asFile.mkdirs()
        commandLine(
            "protoc", "-I", protocolRoot.asFile.absolutePath,
            "--java_out=lite:" + outputDirectory.get().asFile.absolutePath,
            *protocolFiles.files.sorted().map { it.absolutePath }.toTypedArray(),
        )
    }
}

androidComponents.onVariants { variant ->
    variant.sources.java?.addGeneratedSourceDirectory(generateAnchorProtocol) {
        it.outputDirectory
    }
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
