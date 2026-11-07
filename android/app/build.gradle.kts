// SPDX-License-Identifier: GPL-3.0-or-later

plugins {
    id("com.android.application")
    id("com.google.protobuf")
}

android {
    namespace = "org.slateway.app"
    compileSdk = 35

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
