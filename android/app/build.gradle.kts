// SPDX-License-Identifier: GPL-3.0-or-later

plugins {
    id("com.android.application")
    id("com.google.protobuf")
}

android {
    namespace = "org.slateway.app"
    compileSdk = 35

    signingConfigs {
        // Shared debug keystore (checked in, NOT a secret - debug keys
        // are public by definition): every machine signs the debug APK
        // identically, so an APK built on one OS installs over the
        // other's without an uninstall dance.
        getByName("debug") {
            storeFile = rootProject.file("debug.keystore")
            storePassword = "android"
            keyAlias = "androiddebugkey"
            keyPassword = "android"
        }
    }

    defaultConfig {
        applicationId = "org.slateway.app" // docs/DECISIONS.md ADR-011
        minSdk = 29 // docs/DECISIONS.md ADR-009
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
    }

    sourceSets {
        getByName("main") {
            // One normative schema: the same control.proto that
            // generates the host's Rust code (docs/PROTOCOL.md,
            // "Control schemas"). AGP 9 changed the source-set API, so
            // the protobuf plugin's `proto` directory is configured
            // dynamically here.
            withGroovyBuilder {
                "proto" { "srcDir"("../../crates/slateway-proto/proto") }
            }
            // The protobuf plugin's generated dirs are not registered
            // with AGP 9's built-in Kotlin yet; add them explicitly to
            // both the java and the kotlin compilation roots.
            java {
                srcDir("build/generated/java/generateDebugProto/java")
                srcDir("build/generated/java/generateDebugProto/kotlin")
                srcDir("build/generated/java/generateReleaseProto/java")
                srcDir("build/generated/java/generateReleaseProto/kotlin")
            }
            kotlin {
                srcDir("build/generated/java/generateDebugProto/java")
                srcDir("build/generated/java/generateDebugProto/kotlin")
                srcDir("build/generated/java/generateReleaseProto/java")
                srcDir("build/generated/java/generateReleaseProto/kotlin")
            }
        }
    }
}

protobuf {
    protoc {
        artifact = "com.google.protobuf:protoc:4.36.2"
    }
    generateProtoTasks {
        all().forEach { task ->
            task.builtins {
                create("java") {
                    option("lite")
                }
                create("kotlin") {
                    option("lite")
                }
            }
        }
    }
}

dependencies {
    implementation("com.google.protobuf:protobuf-javalite:4.36.2")
    implementation("com.google.protobuf:protobuf-kotlin-lite:4.36.2")
    testImplementation("junit:junit:4.13.2")
}

// The Noise facade (ADR-013): a Rust cdylib in android/native, built
// with cargo-ndk into jniLibs. Skipped with a warning when cargo-ndk
// or the NDK is missing; CI passes -Pslateway.nativeRequired=true to
// make the absence fatal (docs/DEVELOPMENT.md).
val nativeRequired = providers.gradleProperty("slateway.nativeRequired").getOrElse("false").toBoolean()
val buildNative = tasks.register("buildNative") {
    description = "Builds libslateway_native.so (arm64-v8a) with cargo-ndk"
    val so = layout.projectDirectory.file("src/main/jniLibs/arm64-v8a/libslateway_native.so")
    outputs.file(so)
    doLast {
        val cargoNdkAvailable = try {
            ProcessBuilder("cargo", "ndk", "--version").start().waitFor() == 0
        } catch (_: java.io.IOException) {
            false
        }
        if (!cargoNdkAvailable) {
            if (nativeRequired) {
                throw GradleException(
                    "cargo-ndk or the NDK is missing; run: cargo install cargo-ndk && sdkmanager \"ndk;28.2.13676358\""
                )
            }
            logger.warn("cargo-ndk/NDK missing; building without the native Noise facade")
            return@doLast
        }
        val cargoNdkCommand = listOf(
            "cargo", "ndk",
            "-t", "arm64-v8a",
            "-o", layout.projectDirectory.dir("src/main/jniLibs").asFile.absolutePath,
            "build", "--release",
        )
        val process = ProcessBuilder(cargoNdkCommand)
            .directory(rootProject.file("native"))
            .inheritIO()
            .start()
        if (process.waitFor() != 0) {
            throw GradleException("cargo ndk build failed: ${cargoNdkCommand.joinToString(" ")}")
        }
    }
}

tasks.named("preBuild") {
    dependsOn(buildNative)
}
