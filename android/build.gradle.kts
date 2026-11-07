// SPDX-License-Identifier: GPL-3.0-or-later

// AGP 9 has built-in Kotlin (no separate kotlin-android plugin).
plugins {
    id("com.android.application") version "9.1.0" apply false
    id("com.google.protobuf") version "0.10.1" apply false
}
